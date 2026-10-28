//! Bounded DEX provenance for direct literal config-key consumers.
//! Retrieval contracts are inferred from key/default arguments, never from an obfuscated name.
use crate::{
    engine::{NativeEngine, Project},
    native_cfg::{ControlFlowGraph, EdgeKind, instruction_width},
    native_dex::{DexClass, DexMethod},
    native_engine::disassembly::{method_id, symbol},
    native_ir::{DecodedMethod, Instruction},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Flow {
    read: usize,
    path: String,
    kind: Kind,
    transforms: BTreeSet<String>,
    may: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Raw,
    Object,
    Array,
    Element,
    Pattern,
    Matcher,
    List,
    Iterator,
    Uri,
    Predicate,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Origin {
    Unknown,
    Literal(Arc<str>),
    Allocation(usize),
    Ambiguous,
    Flow(Flow),
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    registers: Vec<Origin>,
    result: Origin,
    collections: BTreeMap<usize, Flow>,
    invalid_collections: BTreeSet<usize>,
}
impl State {
    fn unknown(count: usize) -> Self {
        Self {
            registers: vec![Origin::Unknown; count],
            result: Origin::Unknown,
            collections: BTreeMap::new(),
            invalid_collections: BTreeSet::new(),
        }
    }
    fn merge(&mut self, incoming: &Self) -> bool {
        let before = self.clone();
        for (a, b) in self.registers.iter_mut().zip(&incoming.registers) {
            if a != b {
                *a = match (&*a, b) {
                    (Origin::Unknown, Origin::Flow(f)) | (Origin::Flow(f), Origin::Unknown)
                        if f.may =>
                    {
                        Origin::Flow(f.clone())
                    }
                    (Origin::Unknown, Origin::Allocation(n))
                    | (Origin::Allocation(n), Origin::Unknown) => Origin::Allocation(*n),
                    (Origin::Flow(a), Origin::Flow(b))
                        if a.may
                            && b.may
                            && a.read == b.read
                            && a.path == b.path
                            && a.kind == b.kind =>
                    {
                        let mut f = a.clone();
                        f.transforms.extend(b.transforms.iter().cloned());
                        Origin::Flow(f)
                    }
                    _ => Origin::Ambiguous,
                };
            }
        }
        self.invalid_collections
            .extend(incoming.invalid_collections.iter().copied());
        for (allocation, flow) in &incoming.collections {
            if let Some(old) = self.collections.get_mut(allocation) {
                if old.read == flow.read && old.path == flow.path && old.kind == flow.kind {
                    old.transforms.extend(flow.transforms.iter().cloned());
                    old.may |= flow.may;
                } else {
                    self.invalid_collections.insert(*allocation);
                }
            } else {
                self.collections.insert(*allocation, flow.clone());
            }
        }
        self.collections
            .retain(|a, _| !self.invalid_collections.contains(a));
        if self.result != incoming.result {
            self.result = Origin::Unknown;
        }
        *self != before
    }
}
#[derive(Default)]
struct Evidence {
    reads: BTreeMap<usize, Value>,
    arrays: BTreeMap<(usize, String), Value>,
    operations: BTreeMap<String, Value>,
    unresolved: BTreeSet<String>,
}
struct Frame<'a> {
    class: &'a DexClass,
    method: &'a DexMethod,
    depth: usize,
    via: &'a [Value],
}
struct Analysis<'a> {
    classes: &'a HashMap<String, &'a DexClass>,
    key: &'a str,
    cancel: &'a AtomicBool,
    work: usize,
    evidence: Evidence,
}
fn argument(state: &State, i: &Instruction, n: usize) -> Origin {
    i.reads
        .get(n)
        .and_then(|r| state.registers.get(r.register as usize))
        .cloned()
        .unwrap_or(Origin::Unknown)
}
fn pointer(parent: &str, name: &str) -> String {
    format!("{parent}/{}", name.replace('~', "~0").replace('/', "~1"))
}
fn method_symbol(class: &DexClass, i: &Instruction) -> Option<String> {
    symbol(&class.symbols, 'm', i.reference?.index as usize)
}
fn invoke(i: &Instruction) -> bool {
    matches!(i.opcode,0x6e..=0x72|0x74..=0x78|0xfa|0xfb)
}
fn transformed(value: Origin, name: &str, kind: Option<Kind>) -> Origin {
    if let Origin::Flow(mut flow) = value {
        if let Some(kind) = kind {
            flow.kind = kind;
        }
        if !name.is_empty() {
            flow.transforms.insert(name.into());
        }
        Origin::Flow(flow)
    } else {
        Origin::Unknown
    }
}
fn named(target: &str, owner: &str, method: &str) -> bool {
    target.starts_with(&format!("{owner}.{method}("))
}

impl Analysis<'_> {
    fn analyze(
        &mut self,
        class: &DexClass,
        method: &DexMethod,
        parameters: &[Origin],
        depth: usize,
        via: &[Value],
        emit: bool,
    ) -> Result<()> {
        let frame = Frame {
            class,
            method,
            depth,
            via,
        };
        let code = method.code.as_ref().context("Method has no code")?;
        ensure!(
            code.instructions.len() <= 32768 && code.registers <= 512,
            "Config flow exceeds method budget"
        );
        let ir = DecodedMethod::decode(code)?;
        let cfg = ControlFlowGraph::build(code)?;
        ensure!(
            cfg.blocks.len().saturating_mul(code.registers as usize) <= 100_000,
            "Config flow exceeds state budget"
        );
        let instructions: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
        let mut initial = State::unknown(code.registers as usize);
        let first = usize::from(
            code.registers
                .checked_sub(code.ins)
                .context("Invalid input registers")?,
        );
        for (slot, value) in initial.registers[first..].iter_mut().zip(parameters) {
            *slot = value.clone();
        }
        let mut states = vec![None; cfg.blocks.len()];
        let entry = *cfg.block_at.get(&0).context("Missing entry block")?;
        states[entry] = Some(initial);
        let mut queue = VecDeque::from([entry]);
        let mut queued = BTreeSet::from([entry]);
        while let Some(block) = queue.pop_front() {
            queued.remove(&block);
            ensure!(!self.cancel.load(Ordering::Relaxed), "CANCELLED");
            let mut state = states[block].clone().unwrap();
            for pc in &cfg.blocks[block].instructions {
                self.work += 1;
                ensure!(
                    self.work <= 1_000_000,
                    "Config flow exceeds instruction budget"
                );
                self.transfer(&frame, instructions[pc], &mut state, false)?;
            }
            for edge in &cfg.blocks[block].successors {
                // Exceptional paths receive no successful-call provenance.
                let incoming = if edge.kind == EdgeKind::Exceptional {
                    State::unknown(code.registers as usize)
                } else {
                    state.clone()
                };
                let changed = match &mut states[edge.target] {
                    None => {
                        states[edge.target] = Some(incoming);
                        true
                    }
                    Some(old) => old.merge(&incoming),
                };
                if changed && queued.insert(edge.target) {
                    queue.push_back(edge.target);
                }
            }
        }
        if emit {
            // Emit only after the fixed point: transient predecessor values are not evidence.
            for (block, input) in cfg.blocks.iter().zip(states) {
                let Some(mut state) = input else { continue };
                for pc in &block.instructions {
                    self.transfer(&frame, instructions[pc], &mut state, true)?;
                }
            }
        }
        Ok(())
    }
    fn transfer(
        &mut self,
        frame: &Frame<'_>,
        i: &Instruction,
        state: &mut State,
        emit: bool,
    ) -> Result<()> {
        let class = frame.class;
        ensure!(!self.cancel.load(Ordering::Relaxed), "CANCELLED");
        let mut value = match i.opcode {
            0x01..=0x09 | 0x1f => argument(state, i, 0),
            0x0a..=0x0c => state.result.clone(),
            0x1a | 0x1b => i
                .reference
                .and_then(|r| class.symbols.strings.get(r.index as usize))
                .filter(|s| s.len() <= 65536)
                .map(|s| Origin::Literal(Arc::from(s.as_str())))
                .unwrap_or(Origin::Unknown),
            0x22 => Origin::Allocation(i.pc),
            _ => Origin::Unknown,
        };
        if emit && matches!(i.opcode, 0x32..=0x3d) {
            for r in &i.reads {
                if let Origin::Flow(f) = &state.registers[r.register as usize]
                    && f.kind == Kind::Predicate
                {
                    let record = json!({"read_call_site_offset":f.read,"method_id":method_id(frame.method),"call_site_offset":i.pc,"operation":"conditional_on_comparison","array_path":f.path,"transforms":f.transforms,"via":frame.via,"proof":"comparison result reaches conditional operand; branch feasibility and downstream sinks are not inferred"});
                    self.evidence.operations.insert(record.to_string(), record);
                }
            }
        }
        state.result = Origin::Unknown;
        if invoke(i) {
            if let Some(target) = method_symbol(class, i) {
                value = self.call(frame, i, state, &target, emit)?;
            }
            state.result = value.clone();
        }
        for write in &i.writes {
            for word in 0..write.kind.word_count() {
                state.registers[write.register as usize + word] = value.clone();
            }
        }
        Ok(())
    }
    fn call(
        &mut self,
        frame: &Frame<'_>,
        i: &Instruction,
        state: &mut State,
        target: &str,
        emit: bool,
    ) -> Result<Origin> {
        let method = frame.method;
        let depth = frame.depth;
        let via = frame.via;
        let args: Vec<_> = (0..i.reads.len()).map(|n| argument(state, i, n)).collect();
        let get = |n: usize| {
            let origin = args.get(n).cloned().unwrap_or(Origin::Unknown);
            if let Origin::Allocation(a) = origin
                && let Some(f) = state.collections.get(&a)
            {
                return Origin::Flow(f.clone());
            }
            origin
        };
        let static_call = matches!(i.opcode, 0x71 | 0x77);
        let start = usize::from(!static_call);
        if depth == 0
            && get(1) == Origin::Literal(Arc::from(self.key))
            && [
                "java.util.Map",
                "java.util.HashMap",
                "java.util.LinkedHashMap",
            ]
            .iter()
            .any(|o| named(target, o, "get"))
            && target.ends_with("(Ljava/lang/Object;)Ljava/lang/Object;")
        {
            if emit {
                self.evidence.reads.insert(i.pc,json!({"call_site_offset":i.pc,"reader_method_id":target,"default_raw":Value::Null,"default_status":"not_supplied","contract":"keyed map lookup; remote-setting origin requires caller verification"}));
            }
            return Ok(Origin::Flow(Flow {
                read: i.pc,
                path: String::new(),
                kind: Kind::Raw,
                transforms: BTreeSet::new(),
                may: true,
            }));
        }
        let tracked = matches!(get(0), Origin::Flow(_));
        let kotlin_split = target.starts_with("kotlin.text.")
            && target.contains("(Ljava/lang/CharSequence;[Ljava/lang/String;I)Ljava/util/List;");
        if tracked && (named(target, "java.lang.String", "split") || kotlin_split) {
            let value = transformed(
                get(0),
                &if kotlin_split {
                    format!("signature model: {target}")
                } else {
                    target.to_owned()
                },
                Some(Kind::List),
            );
            if emit && let Origin::Flow(f) = &value {
                self.record_array(f, method_id(method), i.pc);
            }
            return Ok(value);
        }
        if tracked
            && (named(target, "java.lang.Object", "toString")
                || named(target, "android.net.Uri", "parse")
                || named(target, "android.net.Uri", "getHost")
                || (target.starts_with("kotlin.text.")
                    && target.ends_with("(Ljava/lang/CharSequence;)Ljava/lang/CharSequence;")))
        {
            let kind = if named(target, "android.net.Uri", "parse") {
                Some(Kind::Uri)
            } else if named(target, "android.net.Uri", "getHost") {
                Some(Kind::Element)
            } else {
                None
            };
            let model = if target.starts_with("kotlin.text.") {
                format!("signature model: {target}")
            } else {
                target.to_owned()
            };
            return Ok(transformed(get(0), &model, kind));
        }
        if tracked
            && [
                "java.lang.Iterable",
                "java.util.Collection",
                "java.util.List",
                "java.util.ArrayList",
            ]
            .iter()
            .any(|o| named(target, o, "iterator"))
        {
            return Ok(transformed(get(0), target, Some(Kind::Iterator)));
        }
        if tracked && named(target, "java.util.Iterator", "next") {
            return Ok(transformed(get(0), target, Some(Kind::Element)));
        }
        if [
            "java.util.Collection",
            "java.util.List",
            "java.util.ArrayList",
        ]
        .iter()
        .any(|o| named(target, o, "add"))
            && let (Some(Origin::Allocation(a)), Origin::Flow(mut f)) = (args.first(), get(1))
        {
            f.kind = Kind::List;
            f.may = true;
            f.transforms.insert(target.into());
            if !state.invalid_collections.contains(a) {
                if state
                    .collections
                    .get(a)
                    .is_some_and(|old| old.read != f.read || old.path != f.path)
                {
                    state.collections.remove(a);
                    state.invalid_collections.insert(*a);
                } else {
                    if let Some(old) = state.collections.get(a) {
                        f.transforms.extend(old.transforms.iter().cloned());
                    }
                    state.collections.insert(*a, f);
                }
            }
            return Ok(Origin::Unknown);
        }
        if tracked
            && (named(target, "java.util.Iterator", "hasNext")
                || named(target, "java.util.Collection", "isEmpty")
                || (target.starts_with("kotlin.collections.")
                    && target.contains(".collectionSizeOrDefault(")))
        {
            return Ok(Origin::Unknown);
        }
        if depth == 0
            && get(start) == Origin::Literal(Arc::from(self.key))
            && target.ends_with("(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;")
        {
            if emit {
                let raw = match get(start + 1) {
                    Origin::Literal(s) => Some(s),
                    _ => None,
                };
                let parsed = raw
                    .as_deref()
                    .filter(|s| s.len() <= 65536)
                    .and_then(|s| serde_json::from_str::<Value>(s).ok());
                self.evidence.reads.insert(i.pc,json!({"call_site_offset":i.pc,"reader_method_id":target,
                    "default_raw":raw.as_deref().filter(|s|s.len()<=65536),"default_json":parsed,
                    "default_status":if raw.as_ref().is_some_and(|s|s.len()>65536){"exceeds_64KiB"}else if parsed.is_some(){"literal_json"}else if raw.is_some(){"literal_non_json"}else{"unresolved"},
                    "contract":"candidate key/default string retrieval; remote-config semantics require reader verification"}));
            }
            return Ok(Origin::Flow(Flow {
                read: i.pc,
                path: String::new(),
                kind: Kind::Raw,
                transforms: BTreeSet::new(),
                may: false,
            }));
        }
        if (named(target, "org.json.JSONObject", "<init>")
            || named(target, "org.json.JSONArray", "<init>"))
            && target.ends_with("(Ljava/lang/String;)V")
        {
            if let (Origin::Allocation(allocation), Origin::Flow(flow)) = (get(0), get(1)) {
                let mut flow = flow;
                flow.kind = if target.starts_with("org.json.JSONObject.") {
                    Kind::Object
                } else {
                    Kind::Array
                };
                for slot in &mut state.registers {
                    if *slot == Origin::Allocation(allocation) {
                        *slot = Origin::Flow(flow.clone());
                    }
                }
                if emit && flow.kind == Kind::Array {
                    self.record_array(&flow, method_id(method), i.pc);
                }
            }
            return Ok(Origin::Unknown);
        }
        if [
            "optJSONArray",
            "getJSONArray",
            "optJSONObject",
            "getJSONObject",
        ]
        .iter()
        .any(|name| named(target, "org.json.JSONObject", name))
            && let (Origin::Flow(mut flow), Origin::Literal(key)) = (get(0), get(1))
            && flow.kind == Kind::Object
        {
            flow.path = pointer(&flow.path, &key);
            flow.kind = if target.ends_with("Lorg/json/JSONArray;") {
                Kind::Array
            } else {
                Kind::Object
            };
            if emit && flow.kind == Kind::Array {
                self.record_array(&flow, method_id(method), i.pc);
            }
            return Ok(Origin::Flow(flow));
        }
        if ["optString", "getString", "get", "opt"]
            .iter()
            .any(|name| named(target, "org.json.JSONArray", name))
            && let Origin::Flow(mut flow) = get(0)
            && flow.kind == Kind::Array
        {
            flow.kind = Kind::Element;
            return Ok(Origin::Flow(flow));
        }
        if ["toLowerCase", "toUpperCase", "trim"]
            .iter()
            .any(|name| named(target, "java.lang.String", name))
        {
            return Ok(transformed(get(0), target, None));
        }
        if named(target, "java.util.Objects", "toString")
            || named(target, "java.lang.String", "valueOf")
        {
            return Ok(get(0));
        }
        if named(target, "java.util.regex.Pattern", "compile") {
            return Ok(transformed(get(0), "", Some(Kind::Pattern)));
        }
        if named(target, "java.util.regex.Pattern", "matcher")
            && let Origin::Flow(flow) = get(0)
            && flow.kind == Kind::Pattern
        {
            return Ok(transformed(Origin::Flow(flow), "", Some(Kind::Matcher)));
        }
        let operation = if named(target, "java.lang.String", "equals") {
            Some("equals")
        } else if named(target, "java.lang.String", "equalsIgnoreCase") {
            Some("equalsIgnoreCase")
        } else if named(target, "java.lang.String", "endsWith") {
            Some("endsWith")
        } else if named(target, "java.lang.String", "startsWith") {
            Some("startsWith")
        } else if named(target, "java.lang.String", "matches")
            || named(target, "java.util.regex.Pattern", "matches")
            || named(target, "java.util.regex.Matcher", "matches")
        {
            Some("regex_matches")
        } else if named(target, "java.util.regex.Matcher", "find") {
            Some("regex_find")
        } else {
            None
        };
        if let Some(operation) = operation {
            if emit {
                for (position, arg) in args.iter().enumerate() {
                    if let Origin::Flow(flow) = arg
                        && matches!(flow.kind, Kind::Element | Kind::Matcher | Kind::Raw)
                    {
                        let record = json!({"read_call_site_offset":flow.read,"array_path":flow.path,"operation":operation,"method_id":method_id(method),"call_site_offset":i.pc,"target_method_id":target,"argument_word":position,"transforms":flow.transforms,"via":via,"proof":if flow.may {"modeled possible DEX provenance; transforms may occur on alternative paths; not runtime reachability"} else {"DEX register provenance; static possible use, not runtime reachability"}});
                        self.evidence.operations.insert(record.to_string(), record);
                    }
                }
            }
            return Ok(args
                .iter()
                .find_map(|a| {
                    if matches!(a, Origin::Flow(_)) {
                        Some(transformed(a.clone(), target, Some(Kind::Predicate)))
                    } else {
                        None
                    }
                })
                .unwrap_or(Origin::Unknown));
        }
        if named(target, "org.json.JSONArray", "length")
            || named(target, "org.json.JSONObject", "has")
        {
            return Ok(Origin::Unknown);
        }
        // Escaping collection aliases can be mutated by the callee. Invalidation
        // is absorbing at CFG joins, so later iterations cannot revive stale flow.
        for arg in &args {
            if let Origin::Allocation(a) = arg
                && state.collections.remove(a).is_some()
            {
                state.invalid_collections.insert(*a);
            }
        }
        // Unknown calls may mutate JSON containers. Retain immutable element strings,
        // but invalidate mutable aliases instead of propagating a stale array source.
        for arg in &args {
            if let Origin::Flow(flow) = arg
                && matches!(flow.kind, Kind::Raw | Kind::Object | Kind::Array)
            {
                if emit {
                    self.evidence.unresolved.insert(format!(
                        "{target}: unmodeled config/container use at {}@{:x}",
                        method_id(method),
                        i.pc
                    ));
                }
                if matches!(flow.kind, Kind::Object | Kind::Array) {
                    for slot in &mut state.registers {
                        if matches!(slot,Origin::Flow(other) if other.read==flow.read && matches!(other.kind,Kind::Object|Kind::Array))
                        {
                            *slot = Origin::Unknown;
                        }
                    }
                }
            }
        }
        // Inspect bounded app helpers only when array-element provenance reaches their arguments.
        if emit
            && args.iter().any(
                |a| matches!(a,Origin::Flow(f) if matches!(f.kind,Kind::Element|Kind::Matcher)),
            )
        {
            let head = target.split_once('(').map(|(h, _)| h).unwrap_or("");
            if let Some((owner, _)) = head.rsplit_once('.')
                && let Some(helper_class) = self.classes.get(owner).copied()
                && let Some(helper) = helper_class.methods.iter().find(|m| method_id(m) == target)
            {
                if depth < 2 && matches!(i.opcode, 0x70 | 0x71 | 0x76 | 0x77) {
                    let mut chain = via.to_vec();
                    chain.push(json!({"method_id":method_id(method),"call_site_offset":i.pc,"target_method_id":target}));
                    if let Err(error) =
                        self.analyze(helper_class, helper, &args, depth + 1, &chain, true)
                    {
                        self.evidence
                            .unresolved
                            .insert(format!("{target}: {error}"));
                    }
                } else {
                    self.evidence.unresolved.insert(format!(
                        "{target}: helper depth or dynamic dispatch boundary"
                    ));
                }
            } else if !named(target, "android.text.TextUtils", "isEmpty") {
                self.evidence.unresolved.insert(format!(
                    "{target}: unmodeled array-element use at {}@{:x}",
                    method_id(method),
                    i.pc
                ));
            }
        }
        Ok(Origin::Unknown)
    }
    fn record_array(&mut self, flow: &Flow, method: String, pc: usize) {
        self.evidence.arrays.insert((flow.read,flow.path.clone()),json!({"read_call_site_offset":flow.read,"path":flow.path,"method_id":method,"call_site_offset":pc}));
    }
}

// Report exact enclosing-method caller context separately from value provenance.
// A call edge does not establish that every branch in its callee affected a result.
fn caller_context(engine: &NativeEngine, method: &DexMethod, cancel: &AtomicBool) -> Result<Value> {
    let root = method_id(method);
    let mut graph = engine.call_graph_query_by_id(
        &root,
        &json!({"direction":"callers","projection":"call_sites","depth":2,"limit":64}),
        cancel,
    )?;
    if let Some(rows) = graph["call_sites"].as_array_mut() {
        for row in rows {
            let id = row["caller_method_id"].as_str().unwrap_or("").to_owned();
            let pc = row["call_site_offset"].as_u64().unwrap_or(u64::MAX) as usize;
            let usage = crate::call_graph::owner(&id)
                .and_then(|o| engine.dex_class(o))
                .and_then(|c| c.methods.iter().find(|m| method_id(m) == id))
                .and_then(|m| m.code.as_ref())
                .and_then(|code| DecodedMethod::decode(code).ok())
                .and_then(|ir| result_use(&ir, pc));
            row["result_use"] = usage.unwrap_or_else(|| json!({"status":"unresolved"}));
            for key in [
                "invoke_words",
                "caller_signature",
                "caller_code_offset_bytes",
            ] {
                row.as_object_mut().unwrap().remove(key);
            }
        }
    }
    graph["scope"] = json!(
        "Exact declared callers of the enclosing consumer method, depth 2; immediate result use is proven separately. This is call context, not an interprocedural taint or runtime reachability claim. Resume omitted sites with get_call_graph using the same root and depth."
    );
    Ok(graph)
}
fn result_use(ir: &DecodedMethod, pc: usize) -> Option<Value> {
    let index = ir.instructions.iter().position(|i| i.pc == pc)?;
    let next = ir.instructions.get(index + 1)?;
    if !matches!(next.opcode, 0x0a..=0x0c) {
        return None;
    }
    let mut aliases = BTreeSet::from([next.writes.first()?.register]);
    for i in ir.instructions.iter().skip(index + 2).take(16) {
        if i.reads.iter().any(|r| aliases.contains(&r.register)) {
            let operation = match i.opcode {
                0x0f..=0x11 => "returned",
                0x32..=0x3d => "conditional",
                _ => "other_use",
            };
            if matches!(i.opcode, 0x01..=0x09) {
                aliases.insert(i.writes.first()?.register);
                continue;
            }
            return Some(
                json!({"status":"exact_register_use","operation":operation,"offset":i.pc,"branch_target":i.branch_target}),
            );
        }
        for w in &i.writes {
            aliases.remove(&w.register);
        }
        if aliases.is_empty() || matches!(i.opcode,0x0e..=0x11|0x27..=0x2c|0x32..=0x3d) {
            break;
        }
    }
    None
}

pub fn search(
    engine: &NativeEngine,
    project: &Project,
    key: &str,
    offset: usize,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Value> {
    ensure!(
        !key.is_empty() && key.len() <= 4096,
        "Key must contain 1..4096 bytes"
    );
    ensure!((1..=100).contains(&limit), "Limit must be 1..100");
    let classes: HashMap<_, _> = project
        .classes
        .iter()
        .map(|name| {
            Ok((
                name.clone(),
                engine.dex_class(name).context("Missing DEX class")?,
            ))
        })
        .collect::<Result<_>>()?;
    let mut indexes: HashMap<usize, Option<usize>> = HashMap::new();
    let mut candidates = Vec::new();
    let mut scan_errors = Vec::new();
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let class = engine.dex_class(name).context("Missing DEX class")?;
        let index = *indexes
            .entry(Arc::as_ptr(&class.symbols) as usize)
            .or_insert_with(|| class.symbols.strings.iter().position(|s| s == key));
        let Some(index) = index else { continue };
        for method in &class.methods {
            let Some(code) = &method.code else { continue };
            let mut pc = 0;
            while pc < code.instructions.len() {
                if pc.is_multiple_of(256) {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                }
                let (width, payload) = match instruction_width(&code.instructions, pc) {
                    Ok(v) => v,
                    Err(e) => {
                        scan_errors
                            .push(json!({"method_id":method_id(method),"reason":e.to_string()}));
                        ensure!(
                            scan_errors.len() <= 100,
                            "Too many unreadable DEX methods during key scan"
                        );
                        break;
                    }
                };
                let op = code.instructions[pc] as u8;
                let found = !payload
                    && match op {
                        0x1a => usize::from(code.instructions[pc + 1]) == index,
                        0x1b => {
                            (u32::from(code.instructions[pc + 1])
                                | (u32::from(code.instructions[pc + 2]) << 16))
                                as usize
                                == index
                        }
                        _ => false,
                    };
                if found {
                    candidates.push((class, method));
                    break;
                }
                pc += width;
            }
        }
    }
    ensure!(
        scan_errors.len() <= 100,
        "Too many unreadable DEX methods during key scan"
    );
    ensure!(offset <= candidates.len(), "Invalid offset");
    let mut items = Vec::new();
    let mut next = offset;
    let mut bytes = 0;
    for (class, method) in candidates.iter().skip(offset).take(limit) {
        let mut analysis = Analysis {
            classes: &classes,
            key,
            cancel,
            work: 0,
            evidence: Evidence::default(),
        };
        let error = analysis
            .analyze(class, method, &[], 0, &[], true)
            .err()
            .map(|e| e.to_string());
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let e = analysis.evidence;
        let arrays: Vec<_> = e
            .arrays
            .into_iter()
            .map(|((read, path), mut array)| {
                let default = e
                    .reads
                    .get(&read)
                    .filter(|r| r["default_status"] == "literal_json")
                    .and_then(|r| r.get("default_json"))
                    .and_then(|v| v.pointer(&path));
                array["default_present"] = json!(default.is_some());
                array["default_value"] = json!(default);
                array["default_is_array"] = json!(default.is_some_and(Value::is_array));
                array["operations"] = json!(
                        e.operations
                            .values()
                            .filter(|op| op["read_call_site_offset"] == read
                                && op["array_path"] == path)
                            .collect::<Vec<_>>()
                    );
                array
            })
            .collect();
        let callers = if !e.reads.is_empty() {
            match caller_context(engine, method, cancel) {
                Ok(value) => value,
                Err(error) => {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    json!({"status":"unavailable","reason":error.to_string()})
                }
            }
        } else {
            Value::Null
        };
        let item = json!({"caller_context":callers,"operations":e.operations.values().collect::<Vec<_>>(),"method_id":method_id(method),"dex_entry":class.symbols.origin.get(),"reads":e.reads.values().collect::<Vec<_>>(),"arrays":arrays,"analysis_error":error,"unresolved_uses":e.unresolved,
            "status":if error.is_some(){"partial"}else if e.reads.is_empty(){"literal_only"}else{"candidate_consumer"}});
        let size = serde_json::to_vec(&item)?.len();
        ensure!(
            size <= 1_000_000,
            "Single consumer exceeds output budget; use get_method_source"
        );
        if bytes + size > 1_000_000 && !items.is_empty() {
            break;
        }
        bytes += size;
        items.push(item);
        next += 1;
    }
    Ok(
        json!({"key":key,"items":items,"total":candidates.len(),"offset":offset,"next_offset":(next<candidates.len()).then_some(next),"scan_errors":scan_errors,
        "coverage":"Exact const-string/jumbo key sites; two-string argument/string-return retrieval candidates; local register flow, org.json arrays, selected String/regex APIs and static/direct helpers up to depth 2. Also models map-key reads, String/Kotlin split signatures, local collection iteration and URI host extraction as possible flows. Field flows, dynamic keys, arbitrary collection adapters, reflection and virtual helper dispatch remain unresolved.",
        "defaults_scope":"Packaged literal arguments only; runtime remote values are unknown. Reader semantics are inferred, not asserted from method names.","offset_unit":"DEX code units"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    struct Fixture {
        class: DexClass,
        symbols: DexSymbols,
        words: Vec<u16>,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                class: crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
                    .unwrap()
                    .classes
                    .remove(0),
                symbols: DexSymbols::default(),
                words: vec![],
            }
        }
        fn string(&mut self, s: &str) -> u16 {
            self.symbols.strings.push(s.into());
            (self.symbols.strings.len() - 1) as u16
        }
        fn ty(&mut self, s: &str) -> u16 {
            self.symbols.types.push(s.into());
            (self.symbols.types.len() - 1) as u16
        }
        fn literal(&mut self, reg: u16, s: &str) {
            let index = self.string(s);
            self.words.extend([0x1a | (reg << 8), index]);
        }
        fn call(
            &mut self,
            owner: &str,
            name: &str,
            params: &[&str],
            ret: &str,
            regs: &[u16],
            static_call: bool,
        ) -> usize {
            let ty = self.ty(owner);
            let name = self.string(name);
            let proto = self.symbols.protos.len() as u16;
            self.symbols
                .protos
                .push((ret.into(), params.iter().map(|p| (*p).into()).collect()));
            let index = self.symbols.methods.len() as u16;
            self.symbols.methods.push((ty, proto, u32::from(name)));
            let pc = self.words.len();
            let op = if static_call { 0x71 } else { 0x6e };
            let mut packed = 0;
            for (i, r) in regs.iter().take(4).enumerate() {
                packed |= r << (i * 4);
            }
            self.words.extend([
                op | ((regs.len() as u16) << 12) | (regs.get(4).copied().unwrap_or(0) << 8),
                index,
                packed,
            ]);
            pc
        }
        fn result(&mut self, reg: u16) {
            self.words.push(0x0c | (reg << 8));
        }
        fn base(&mut self) {
            self.literal(0, "hosts.key");
            self.literal(1, r#"{"suffix":[".example.com"],"full":["example.com"]}"#);
            self.call(
                "Lapp/Config;",
                "get",
                &["Ljava/lang/String;", "Ljava/lang/String;"],
                "Ljava/lang/String;",
                &[0, 1],
                true,
            );
            self.result(0);
            let ty = self.ty("Lorg/json/JSONObject;");
            self.words.extend([0x122, ty]);
            let pc = self.call(
                "Lorg/json/JSONObject;",
                "<init>",
                &["Ljava/lang/String;"],
                "V",
                &[1, 0],
                false,
            );
            self.words[pc] = (self.words[pc] & 0xff00) | 0x70;
        }
        fn element(&mut self, name: &str) {
            self.literal(2, name);
            self.call(
                "Lorg/json/JSONObject;",
                "optJSONArray",
                &["Ljava/lang/String;"],
                "Lorg/json/JSONArray;",
                &[1, 2],
                false,
            );
            self.result(2);
            self.words.push(0x312);
            self.call(
                "Lorg/json/JSONArray;",
                "optString",
                &["I"],
                "Ljava/lang/String;",
                &[2, 3],
                false,
            );
            self.result(4);
        }
        fn finish(mut self, name: &str) -> DexClass {
            self.words.push(0x000e);
            self.class.descriptor = format!("L{};", name.replace('.', "/")).into();
            self.class.methods = vec![DexMethod {
                declaring_type: self.class.descriptor.clone(),
                name: "check".into(),
                return_type: "V".into(),
                parameters: vec!["Ljava/lang/String;".into()],
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 6,
                    ins: 1,
                    outs: 5,
                    tries: 0,
                    try_regions: vec![],
                    instructions: self.words,
                    offset: 0,
                }),
            }];
            self.class.symbols = Arc::new(self.symbols);
            self.class
        }
    }
    fn analyze(class: &DexClass, other: Option<&DexClass>) -> Evidence {
        let mut classes = HashMap::from([("app.Root".into(), class)]);
        if let Some(other) = other {
            classes.insert("app.Helper".into(), other);
        }
        let mut a = Analysis {
            classes: &classes,
            key: "hosts.key",
            cancel: &AtomicBool::new(false),
            work: 0,
            evidence: Evidence::default(),
        };
        a.analyze(class, &class.methods[0], &[], 0, &[], true)
            .unwrap();
        a.evidence
    }
    #[test]
    fn separates_arrays_and_follows_case_folding_and_regex_find() {
        let mut f = Fixture::new();
        f.base();
        f.element("suffix");
        f.call(
            "Ljava/lang/String;",
            "toLowerCase",
            &[],
            "Ljava/lang/String;",
            &[4],
            false,
        );
        f.result(4);
        f.call(
            "Ljava/lang/String;",
            "endsWith",
            &["Ljava/lang/String;"],
            "Z",
            &[5, 4],
            false,
        );
        f.element("regex");
        f.call(
            "Ljava/util/regex/Pattern;",
            "compile",
            &["Ljava/lang/String;"],
            "Ljava/util/regex/Pattern;",
            &[4],
            true,
        );
        f.result(4);
        f.call(
            "Ljava/util/regex/Pattern;",
            "matcher",
            &["Ljava/lang/CharSequence;"],
            "Ljava/util/regex/Matcher;",
            &[4, 5],
            false,
        );
        f.result(4);
        f.call("Ljava/util/regex/Matcher;", "find", &[], "Z", &[4], false);
        let e = analyze(&f.finish("app.Root"), None);
        assert_eq!(e.reads.len(), 1);
        assert_eq!(e.arrays.len(), 2);
        assert_eq!(e.operations.len(), 2);
        let ops: Vec<_> = e.operations.values().collect();
        let suffix = ops.iter().find(|v| v["array_path"] == "/suffix").unwrap();
        assert_eq!(suffix["operation"], "endsWith");
        assert_eq!(suffix["transforms"].as_array().unwrap().len(), 1);
        assert!(
            ops.iter()
                .any(|v| v["array_path"] == "/regex" && v["operation"] == "regex_find")
        );
        assert_eq!(
            e.reads.values().next().unwrap()["default_json"]["full"][0],
            "example.com"
        );
    }
    #[test]
    fn conflicting_branch_value_does_not_become_proven_array_use() {
        let mut f = Fixture::new();
        f.base();
        f.element("suffix");
        // One path retains the array element, the other overwrites it with a literal.
        f.words.extend([0x0538, 4, 0x0328]);
        f.words.push(0); // branch lands at const-string below
        let branch = f.words.len() - 4;
        f.literal(4, "unrelated");
        let join = f.words.len();
        f.words[branch + 2] = 0x28 | (((join - (branch + 2)) as u16) << 8);
        f.call(
            "Ljava/lang/String;",
            "endsWith",
            &["Ljava/lang/String;"],
            "Z",
            &[5, 4],
            false,
        );
        let e = analyze(&f.finish("app.Root"), None);
        assert!(e.operations.is_empty());
    }
    #[test]
    fn static_helper_preserves_argument_provenance_and_comparison_identity() {
        let mut helper = Fixture::new();
        helper.call(
            "Ljava/lang/String;",
            "equalsIgnoreCase",
            &["Ljava/lang/String;"],
            "Z",
            &[4, 5],
            false,
        );
        let mut helper = helper.finish("app.Helper");
        helper.methods[0].code.as_mut().unwrap().ins = 2;
        helper.methods[0]
            .parameters
            .push("Ljava/lang/String;".into());
        let mut f = Fixture::new();
        f.base();
        f.element("full");
        f.call(
            "Lapp/Helper;",
            "check",
            &["Ljava/lang/String;", "Ljava/lang/String;"],
            "V",
            &[5, 4],
            true,
        );
        let e = analyze(&f.finish("app.Root"), Some(&helper));
        let op = e.operations.values().next().unwrap();
        assert_eq!(op["operation"], "equalsIgnoreCase");
        assert_eq!(op["array_path"], "/full");
        assert_eq!(op["via"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn register_reuse_and_unrelated_json_do_not_create_consumers() {
        let mut f = Fixture::new();
        f.literal(0, "hosts.key");
        f.literal(0, "other.key");
        f.literal(1, "{}");
        f.call(
            "Lapp/Config;",
            "get",
            &["Ljava/lang/String;", "Ljava/lang/String;"],
            "Ljava/lang/String;",
            &[0, 1],
            true,
        );
        let e = analyze(&f.finish("app.Root"), None);
        assert!(e.reads.is_empty());
        assert!(e.arrays.is_empty());
    }
    #[test]
    fn unknown_json_mutation_invalidates_container_aliases() {
        let mut f = Fixture::new();
        f.base();
        f.call(
            "Lapp/Mutator;",
            "change",
            &["Lorg/json/JSONObject;"],
            "V",
            &[1],
            true,
        );
        f.element("suffix");
        f.call(
            "Ljava/lang/String;",
            "endsWith",
            &["Ljava/lang/String;"],
            "Z",
            &[5, 4],
            false,
        );
        let evidence = analyze(&f.finish("app.Root"), None);
        assert!(evidence.operations.is_empty());
        assert!(
            evidence
                .unresolved
                .iter()
                .any(|s| s.contains("Mutator.change"))
        );
    }
    #[test]
    fn cancellation_stops_flow() {
        let mut f = Fixture::new();
        f.base();
        let class = f.finish("app.Root");
        let classes = HashMap::new();
        let mut a = Analysis {
            classes: &classes,
            key: "hosts.key",
            cancel: &AtomicBool::new(true),
            work: 0,
            evidence: Evidence::default(),
        };
        assert!(
            a.analyze(&class, &class.methods[0], &[], 0, &[], true)
                .unwrap_err()
                .to_string()
                .contains("CANCELLED")
        );
    }
    #[test]
    fn map_key_split_uri_host_and_comparison_reaches_conditional() {
        let mut f = Fixture::new();
        f.literal(0, "hosts.key");
        f.call(
            "Ljava/util/HashMap;",
            "get",
            &["Ljava/lang/Object;"],
            "Ljava/lang/Object;",
            &[5, 0],
            false,
        );
        f.result(0);
        f.call(
            "Ljava/lang/Object;",
            "toString",
            &[],
            "Ljava/lang/String;",
            &[0],
            false,
        );
        f.result(0);
        f.words.extend([0x0112, 0x0212]);
        f.call(
            "Lkotlin/text/Obfuscated;",
            "x",
            &["Ljava/lang/CharSequence;", "[Ljava/lang/String;", "I"],
            "Ljava/util/List;",
            &[0, 1, 2],
            true,
        );
        f.result(0);
        f.call(
            "Ljava/util/List;",
            "iterator",
            &[],
            "Ljava/util/Iterator;",
            &[0],
            false,
        );
        f.result(0);
        f.call(
            "Ljava/util/Iterator;",
            "next",
            &[],
            "Ljava/lang/Object;",
            &[0],
            false,
        );
        f.result(0);
        f.call(
            "Landroid/net/Uri;",
            "parse",
            &["Ljava/lang/String;"],
            "Landroid/net/Uri;",
            &[0],
            true,
        );
        f.result(0);
        f.call(
            "Landroid/net/Uri;",
            "getHost",
            &[],
            "Ljava/lang/String;",
            &[0],
            false,
        );
        f.result(0);
        f.call(
            "Ljava/lang/String;",
            "equalsIgnoreCase",
            &["Ljava/lang/String;"],
            "Z",
            &[5, 0],
            false,
        );
        f.words.extend([0x010a, 0x0138, 2]);
        let c = f.finish("app.Root");
        let e = analyze(&c, None);
        assert_eq!(e.reads.len(), 1);
        assert_eq!(
            e.reads.values().next().unwrap()["default_status"],
            "not_supplied"
        );
        assert!(
            e.operations
                .values()
                .any(|v| v["operation"] == "conditional_on_comparison")
        );
        assert!(e.operations.values().any(|v| {
            v["operation"] == "equalsIgnoreCase"
                && v["transforms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s.as_str().unwrap().starts_with("signature model:"))
        }));
    }
    #[test]
    fn collection_join_invalidation_is_absorbing_and_order_independent() {
        let f = Flow {
            read: 1,
            path: String::new(),
            kind: Kind::List,
            transforms: BTreeSet::new(),
            may: true,
        };
        let mut a = State::unknown(2);
        a.collections.insert(10, f.clone());
        let mut b = a.clone();
        b.collections.get_mut(&10).unwrap().read = 2;
        for (mut first, second) in [(a.clone(), b.clone()), (b, a.clone())] {
            first.merge(&second);
            assert!(!first.collections.contains_key(&10));
            first.merge(&a);
            assert!(!first.collections.contains_key(&10));
            assert!(first.invalid_collections.contains(&10));
        }
    }
    #[test]
    fn caller_result_use_tracks_moves_and_stops_at_overwrite() {
        let mut f = Fixture::new();
        let pc = f.call("Lapp/Root;", "gate", &[], "Z", &[], true);
        f.words.extend([0x000a, 0x0101, 0x0138, 2]);
        let mut c = f.finish("app.Caller");
        let ir = DecodedMethod::decode(c.methods[0].code.as_ref().unwrap()).unwrap();
        assert_eq!(result_use(&ir, pc).unwrap()["operation"], "conditional");
        c.methods[0].code.as_mut().unwrap().instructions[4] = 0x0112;
        let ir = DecodedMethod::decode(c.methods[0].code.as_ref().unwrap()).unwrap();
        assert!(result_use(&ir, pc).is_none());
    }
}
