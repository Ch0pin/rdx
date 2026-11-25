//! Conservative semantic summaries of small, pure DEX comparison wrappers.
use crate::{
    engine::NativeEngine,
    method_lookup,
    native_dex::{DexClass, DexMethod},
    native_engine::disassembly::{method_id, symbol},
    native_ir::{DecodedMethod, Instruction, PoolKind},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

const STRING: &str = "Ljava/lang/String;";
const MAX_WRAPPER_UNITS: usize = 256;
const MAX_CALLER_UNITS: usize = 32768;
const MAX_STEPS: usize = 8192;
const MAX_DEPTH: usize = 4;

#[derive(Clone, Debug)]
enum Text {
    Parameter(usize),
    Literal(String),
}
impl Text {
    fn describe(&self) -> Value {
        match self {
            Self::Parameter(index) => json!({"kind":"argument","argument_index":index}),
            Self::Literal(text) => json!({"kind":"literal","value":text}),
        }
    }
}

#[derive(Clone, Debug)]
enum Register {
    Unknown,
    Null,
    Text(Text),
    Boolean(bool),
    Comparison(Value),
}
impl Register {
    fn is_zero(&self) -> Option<bool> {
        match self {
            Self::Null | Self::Boolean(false) => Some(true),
            Self::Boolean(true) | Self::Text(_) => Some(false),
            Self::Unknown | Self::Comparison(_) => None,
        }
    }
    fn is_null(&self) -> bool {
        matches!(self, Self::Null | Self::Boolean(false))
    }
}

enum Outcome {
    Return(Register, Option<usize>),
    NullThrow(Value),
    Unknown(&'static str),
}
impl Outcome {
    fn describe(self, wrapper: &str) -> Value {
        match self {
            Self::Return(Register::Boolean(value), offset) => {
                json!({"kind":"constant","value":value,
                "method_id":wrapper,"code_unit_offset":offset})
            }
            Self::Return(Register::Comparison(mut compare), offset) => {
                compare["return_method_id"] = json!(wrapper);
                compare["return_code_unit_offset"] = json!(offset);
                compare
            }
            Self::NullThrow(compare) => compare,
            Self::Return(..) => json!({"kind":"unknown","reason":"non-boolean return provenance"}),
            Self::Unknown(reason) => json!({"kind":"unknown","reason":reason}),
        }
    }
}

fn read<'a>(instruction: &Instruction, state: &'a [Register], index: usize) -> &'a Register {
    instruction
        .reads
        .get(index)
        .map_or(&Register::Unknown, |arg| &state[arg.register as usize])
}

fn exact<'a>(engine: &'a NativeEngine, id: &str) -> Result<(&'a DexClass, &'a DexMethod)> {
    ensure!(
        method_lookup::complete(id),
        "Use an exact DEX method signature"
    );
    let owner = method_lookup::owner(id)?;
    let class = engine
        .dex_class(owner)
        .context("Method declaring class is unavailable")?;
    let method = method_lookup::resolve(class, id)?;
    Ok((class, method))
}

fn evidence_sha256(class: &DexClass, method: &DexMethod, decoded: &DecodedMethod) -> String {
    let code = method.code.as_ref().expect("decoded method has code");
    let mut hash = Sha256::new();
    let mut text = |value: &str| {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    };
    text("rdx-dex-guard-v1");
    text(&method_id(method));
    text(class.symbols.origin.get().map_or("", String::as_str));
    for i in &decoded.instructions {
        if let Some(reference) = i.reference {
            let kind = match reference.kind {
                PoolKind::String => 's',
                PoolKind::Type => 't',
                PoolKind::Field => 'f',
                PoolKind::Method => 'm',
                PoolKind::Proto => 'p',
                PoolKind::CallSite | PoolKind::MethodHandle => '?',
            };
            if let Some(meaning) = symbol(&class.symbols, kind, reference.index as usize) {
                text(&meaning);
            }
        }
    }
    hash.update(method.access_flags.to_le_bytes());
    hash.update(code.registers.to_le_bytes());
    hash.update(code.ins.to_le_bytes());
    for word in &code.instructions {
        hash.update(word.to_le_bytes());
    }
    for region in &code.try_regions {
        hash.update(region.start.to_le_bytes());
        hash.update(region.end.to_le_bytes());
        for (ty, target) in region.catches.iter() {
            if let Some(ty) = ty {
                hash.update((ty.len() as u64).to_le_bytes());
                hash.update(ty.as_bytes());
            } else {
                hash.update(0u64.to_le_bytes());
            }
            hash.update(target.to_le_bytes());
        }
    }
    format!("{:x}", hash.finalize())
}

fn operation(method: &str) -> Option<&'static str> {
    match method {
        "java.lang.String.equals(Ljava/lang/Object;)Z" => Some("equals"),
        "java.lang.String.equalsIgnoreCase(Ljava/lang/String;)Z" => Some("equals_ignore_case"),
        "java.lang.String.startsWith(Ljava/lang/String;)Z" => Some("starts_with"),
        "java.lang.String.endsWith(Ljava/lang/String;)Z" => Some("ends_with"),
        "java.lang.String.contains(Ljava/lang/CharSequence;)Z" => Some("contains"),
        _ => None,
    }
}

fn compare(
    owner: &DexMethod,
    pc: usize,
    api: &str,
    operation: &str,
    receiver: &Register,
    argument: &Register,
) -> Outcome {
    let equality = matches!(operation, "equals" | "equals_ignore_case");
    if receiver.is_null() || !equality && argument.is_null() {
        return Outcome::NullThrow(json!({"kind":"throws_null_pointer",
            "null_operand":if receiver.is_null() {"receiver"}else{"argument"},
            "platform_method_id":api,"method_id":method_id(owner),"code_unit_offset":pc}));
    }
    let Register::Text(receiver) = receiver else {
        return Outcome::Unknown("platform String receiver not proved");
    };
    if equality && argument.is_null() {
        return Outcome::Return(Register::Boolean(false), None);
    }
    let Register::Text(argument) = argument else {
        return Outcome::Unknown("platform String operand not proved");
    };
    Outcome::Return(
        Register::Comparison(json!({"kind":"comparison","operation":operation,
        "receiver":receiver.describe(),"operand":argument.describe(),"platform_method_id":api,
        "method_id":method_id(owner),"code_unit_offset":pc})),
        None,
    )
}

struct Proof<'a> {
    engine: &'a NativeEngine,
    cancel: &'a AtomicBool,
    methods: BTreeMap<String, Value>,
    steps: usize,
}
impl Proof<'_> {
    fn run(
        &mut self,
        class: &DexClass,
        method: &DexMethod,
        inputs: &[Register],
        stack: &mut Vec<String>,
    ) -> Result<Outcome> {
        let Some(code) = &method.code else {
            return Ok(Outcome::Unknown("helper body unavailable"));
        };
        if code.instructions.len() > MAX_WRAPPER_UNITS
            || code.registers > 32
            || !code.try_regions.is_empty()
            || code.tries != 0
            || method.return_type.as_ref() != "Z"
            || method.parameters.len() != inputs.len()
            || method
                .parameters
                .iter()
                .any(|p| !matches!(p.as_ref(), STRING | "Z"))
        {
            return Ok(Outcome::Unknown(
                "unsupported helper size, descriptor or exception handlers",
            ));
        }
        let id = method_id(method);
        if stack.len() >= MAX_DEPTH || stack.contains(&id) {
            return Ok(Outcome::Unknown("helper call cycle or depth budget"));
        }
        let Ok(decoded) = DecodedMethod::decode(code) else {
            return Ok(Outcome::Unknown("unsupported DEX decoding"));
        };
        self.methods.entry(id.clone()).or_insert_with(|| json!({"method_id":id,
            "dex_entry":class.symbols.origin.get(),"dex_evidence_sha256":evidence_sha256(class,method,&decoded)}));
        let Some(first) = code.registers.checked_sub(code.ins) else {
            return Ok(Outcome::Unknown("invalid helper register frame"));
        };
        let first = first as usize + usize::from(method.access_flags & 8 == 0);
        if first + inputs.len() != code.registers as usize {
            return Ok(Outcome::Unknown("helper input register count mismatch"));
        }
        let mut registers = vec![Register::Unknown; code.registers as usize];
        registers[first..].clone_from_slice(inputs);
        stack.push(id);
        let result = self.execute(class, method, &decoded, registers, stack);
        stack.pop();
        result
    }

    fn execute(
        &mut self,
        class: &DexClass,
        method: &DexMethod,
        decoded: &DecodedMethod,
        mut registers: Vec<Register>,
        stack: &mut Vec<String>,
    ) -> Result<Outcome> {
        let mut pc = 0;
        let mut pending = None;
        loop {
            ensure!(!self.cancel.load(Ordering::Relaxed), "CANCELLED");
            self.steps += 1;
            if self.steps > MAX_STEPS {
                return Ok(Outcome::Unknown("symbolic execution work budget reached"));
            }
            let Ok(index) = decoded.instructions.binary_search_by_key(&pc, |i| i.pc) else {
                return Ok(Outcome::Unknown("non-executable helper branch"));
            };
            let i = &decoded.instructions[index];
            let value = read(i, &registers, 0);
            let prior_result = pending.take();
            let mut next = pc + i.width;
            let produced = match i.opcode {
                0x00 => None,
                0x01..=0x03 | 0x07..=0x09 => Some(value.clone()),
                0x0a => prior_result,
                0x0f => return Ok(Outcome::Return(value.clone(), Some(pc))),
                0x12..=0x15 if matches!(i.literal, Some(0 | 1)) => {
                    Some(Register::Boolean(i.literal == Some(1)))
                }
                0x1a | 0x1b => {
                    let text = i
                        .reference
                        .and_then(|reference| class.symbols.strings.get(reference.index as usize));
                    if let Some(text) = text.filter(|text| text.len() <= 4096) {
                        Some(Register::Text(Text::Literal(text.clone())))
                    } else {
                        return Ok(Outcome::Unknown("unavailable or oversized helper literal"));
                    }
                }
                0x28..=0x2a => {
                    next = i.branch_target.unwrap_or(pc);
                    None
                }
                0x38 | 0x39 => {
                    let Some(zero) = value.is_zero() else {
                        return Ok(Outcome::Unknown("condition uses an unproved value"));
                    };
                    if zero == (i.opcode == 0x38) {
                        next = i.branch_target.unwrap_or(pc);
                    }
                    None
                }
                0x6e | 0x74 | 0x71 | 0x77 => {
                    let Some(api) = i.reference.and_then(|reference| {
                        symbol(&class.symbols, 'm', reference.index as usize)
                    }) else {
                        return Ok(Outcome::Unknown("unresolved invoke target"));
                    };
                    let outcome = if matches!(i.opcode, 0x6e | 0x74) {
                        let Some(operation) = operation(&api) else {
                            return Ok(Outcome::Unknown("unsupported virtual call or side effect"));
                        };
                        if i.reads.len() != 2 {
                            return Ok(Outcome::Unknown("unsupported comparison register frame"));
                        }
                        compare(method, pc, &api, operation, value, read(i, &registers, 1))
                    } else {
                        let Ok((nested_class, nested)) = exact(self.engine, &api) else {
                            return Ok(Outcome::Unknown("unproved static call or side effect"));
                        };
                        if nested.access_flags & 8 == 0 {
                            return Ok(Outcome::Unknown("static call targets an instance method"));
                        }
                        if nested.declaring_type != method.declaring_type {
                            return Ok(Outcome::Unknown(
                                "cross-class static call may run a class initializer",
                            ));
                        }
                        let arguments: Vec<_> = i
                            .reads
                            .iter()
                            .map(|r| registers[r.register as usize].clone())
                            .collect();
                        self.run(nested_class, nested, &arguments, stack)?
                    };
                    match outcome {
                        Outcome::Return(result, _) => pending = Some(result),
                        other => return Ok(other),
                    }
                    None
                }
                _ => {
                    return Ok(Outcome::Unknown(
                        "unsupported instruction or possible side effect",
                    ));
                }
            };
            if let Some(produced) = produced {
                let [target] = i.writes.as_slice() else {
                    return Ok(Outcome::Unknown("unsupported comparison register write"));
                };
                registers[target.register as usize] = produced;
            } else if !i.writes.is_empty() {
                return Ok(Outcome::Unknown("unproved helper register write"));
            }
            if next <= pc {
                return Ok(Outcome::Unknown("backward branch or control-flow cycle"));
            }
            pc = next;
        }
    }
}

fn resource_evidence(table: &crate::resource_table::ResourceTable, id: u32) -> Value {
    let display = format!("0x{id:08x}");
    let Some(resource) = table.entries.get(&id).filter(|r| r.kind == "string") else {
        return json!({"resource_id":display,"status":"unresolved","reason":"loaded string resource unavailable"});
    };
    let name = format!("{}:string/{}", resource.package, resource.name);
    // Scope the generic resolver to a small string-only alias inventory.
    let mut pending = vec![id];
    let mut seen = BTreeSet::from([id]);
    let mut variant_count = 0;
    while let Some(current) = pending.pop() {
        let Some(entry) = table
            .entries
            .get(&current)
            .filter(|entry| entry.kind == "string")
        else {
            return json!({"resource_id":display,"resource_name":name,"status":"unresolved",
                "reason":"string alias points outside the loaded string table"});
        };
        variant_count += entry.variants.len();
        if variant_count > 64
            || entry
                .variants
                .iter()
                .any(|variant| variant.value.len() > 4096)
        {
            return json!({"resource_id":display,"resource_name":name,"status":"unresolved",
                "reason":"resource evidence exceeds variant or preview budget"});
        }
        for target in entry
            .variants
            .iter()
            .filter_map(|variant| variant.reference)
        {
            if seen.insert(target) {
                pending.push(target);
            }
        }
    }
    let result = table.resolve_scalar(&format!("@{display}"), &resource.package);
    let baseline = result["value"].as_str();
    let variants = result["variants"].as_array();
    if result["configuration_inventory_truncated"] == true
        || result["status"] != "resolved"
        || baseline.is_none_or(|value| value.len() > 4096)
        || variants.is_none_or(|variants| {
            variants.is_empty()
                || variants.iter().any(|variant| {
                    variant["status"] != "resolved" || variant["value"].as_str() != baseline
                })
        })
    {
        return json!({"resource_id":display,"resource_name":name,"status":"unresolved",
            "reason":"resource varies, is unresolved across packaged configurations, or exceeds preview budget"});
    }
    json!({"resource_id":display,"resource_name":name,"status":"resolved","value":baseline,
        "configuration_count":variants.map(Vec::len),"scope":"Identical value across the decoded APK resource configurations. The runtime Context and selected resources require observation."})
}

fn scenarios(method: &DexMethod, arguments: &[Value]) -> Option<Vec<(Vec<Value>, Vec<Register>)>> {
    if method.parameters.len() > 4 {
        return None;
    }
    let mut states = vec![(Vec::new(), Vec::new())];
    for (index, descriptor) in method.parameters.iter().enumerate() {
        let argument = arguments.get(index).unwrap_or(&Value::Null);
        let options = match descriptor.as_ref() {
            STRING
                if argument["status"] == "constant"
                    && argument["kind"] == "bits32"
                    && argument["value"] == 0 =>
            {
                vec![(json!("null"), Register::Null)]
            }
            STRING if argument["status"] == "constant" && argument["kind"] == "string" => {
                vec![(json!("non_null"), Register::Text(Text::Parameter(index)))]
            }
            STRING => vec![
                (json!("null"), Register::Null),
                (json!("non_null"), Register::Text(Text::Parameter(index))),
            ],
            "Z" if argument["status"] == "constant"
                && argument["kind"] == "bits32"
                && matches!(argument["value"].as_i64(), Some(0 | 1)) =>
            {
                let value = argument["value"] == 1;
                vec![(json!(value), Register::Boolean(value))]
            }
            "Z" => vec![
                (json!(false), Register::Boolean(false)),
                (json!(true), Register::Boolean(true)),
            ],
            _ => return None,
        };
        let mut next = Vec::new();
        for (when, inputs) in &states {
            for (assumption, register) in &options {
                let mut when = when.clone();
                let mut inputs = inputs.clone();
                when.push(json!({"argument_index":index,"equals":assumption}));
                inputs.push(register.clone());
                next.push((when, inputs));
            }
        }
        states = next;
    }
    Some(states)
}

pub(crate) fn query(engine: &NativeEngine, args: &Value, cancel: &AtomicBool) -> Result<Value> {
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    let mut call = Value::Null;
    let mut arguments = Vec::new();
    let id = if let Some(id) = args["method_id"].as_str() {
        method_lookup::normalize(id)?
    } else {
        let caller_id = method_lookup::normalize(
            args["caller_method_id"]
                .as_str()
                .context("Missing caller_method_id")?,
        )?;
        let (caller_class, caller) = exact(engine, &caller_id)?;
        let code = caller.code.as_ref().context("Caller body unavailable")?;
        ensure!(
            code.instructions.len() <= MAX_CALLER_UNITS,
            "Caller exceeds guard evidence budget"
        );
        let pc = args["call_site_offset"]
            .as_u64()
            .context("Missing call_site_offset")? as usize;
        let decoded = DecodedMethod::decode(code)?;
        ensure!(
            decoded
                .instructions
                .binary_search_by_key(&pc, |i| i.pc)
                .is_ok(),
            "call_site_offset must name a DEX instruction boundary"
        );
        let site = crate::native_xrefs::call_site_at(caller_class, caller, pc)?
            .context("call_site_offset does not name a DEX invoke")?;
        let mut work = 0;
        let evidence = crate::call_arguments::analyze_guard(
            caller_class,
            caller,
            cancel,
            &mut work,
            &BTreeSet::from([pc]),
        );
        let evidence_status = match evidence {
            Ok(mut evidence) => {
                let mut item = evidence.remove(&pc).unwrap_or(Value::Null);
                arguments = item["arguments"]
                    .as_array_mut()
                    .map(std::mem::take)
                    .unwrap_or_default();
                if item["status"] == "analyzed" {
                    "analyzed"
                } else {
                    "unresolved"
                }
            }
            Err(_) => {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                "unresolved"
            }
        };
        for argument in &mut arguments {
            if let Some(id) = argument["resource_id"]
                .as_u64()
                .and_then(|id| u32::try_from(id).ok())
            {
                argument["resource"] = resource_evidence(engine.resource_table(), id);
            }
        }
        call = json!({"caller_method_id":caller_id,"code_unit_offset":pc,"raw_callee_method_id":site.callee,
            "opcode":site.opcode,"instruction_words":site.words,"dex_entry":site.dex_entry,
            "caller_dex_evidence_sha256":evidence_sha256(caller_class,caller,&decoded),
            "argument_evidence_status":evidence_status,"arguments":arguments});
        if !matches!(site.opcode, 0x71 | 0x77) {
            return Ok(
                json!({"status":"unknown","reason":"caller dispatch is not a proved static helper invocation","call_site":call}),
            );
        }
        site.callee
    };
    let (class, method) = exact(engine, &id)?;
    if !call.is_null() && method.access_flags & 8 == 0 {
        return Ok(
            json!({"status":"unknown","reason":"static invoke target has an instance declaration","method_id":id,"call_site":call}),
        );
    }
    let Some(scenarios) = scenarios(method, &arguments) else {
        return Ok(
            json!({"status":"unknown","reason":"helper parameters exceed supported String/boolean case inventory","method_id":id,"call_site":call}),
        );
    };
    let mut proof = Proof {
        engine,
        cancel,
        methods: BTreeMap::new(),
        steps: 0,
    };
    let mut rows = Vec::new();
    for (when, inputs) in scenarios {
        let outcome = proof
            .run(class, method, &inputs, &mut Vec::new())?
            .describe(&id);
        rows.push(json!({"when":when,"outcome":outcome}));
    }
    let complete = rows.iter().all(|row| row["outcome"]["kind"] != "unknown");
    let comparisons: BTreeSet<_> = rows
        .iter()
        .filter_map(|row| row["outcome"]["operation"].as_str())
        .collect();
    let operation = (complete && comparisons.len() == 1).then(|| *comparisons.first().unwrap());
    let evidence = proof.methods.get(&id);
    Ok(
        json!({"status":if complete {"proven"}else{"unknown"},"method_id":id,"operation":operation,
        "dex_evidence_sha256":evidence.map(|row| &row["dex_evidence_sha256"]),
        "dex_entry":class.symbols.origin.get(),"call_site":call,"cases":rows,
        "methods":proof.methods.values().collect::<Vec<_>>(),"scope":"Bounded String/boolean comparison semantics once the selected helper body executes. Null exception cases are explicit. Entry, linkage, class initialization and runtime input require separate evidence."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use crate::resource_table::{Resource, ResourceTable, Variant};
    use std::sync::Arc;

    const OWNER: &str = "Lexample/Guard;";
    const WRAPPER: &str = "example.Guard.run(Ljava/lang/String;Ljava/lang/String;Z)Z";
    const CALLER: &str = "example.Caller.check(Ljava/lang/String;Landroid/content/Context;)Z";
    const RESOURCE: u32 = 0x7f1402f9;

    #[test]
    #[ignore = "requires RDX_GUARD_APK for exact Zappos regression"]
    fn guard_summary_exact_zappos_call_site() {
        const APK_SHA256: &str = "2c2b51eb7a08762c8a197e54280f17c772a982ac2a50838d398d5bb300442344";
        const HELPER: &str = "rd.E.l(Ljava/lang/String;Ljava/lang/String;Z)Z";
        const ZAPPOS_CALLER: &str = "com.zappos.android.bag.navigation.AuthenticatedWebViewKt$AuthenticatedWebView$1$1$1$1$1.shouldInterceptRequest(Landroid/webkit/WebView;Landroid/webkit/WebResourceRequest;)Landroid/webkit/WebResourceResponse;";
        let path =
            std::path::PathBuf::from(std::env::var_os("RDX_GUARD_APK").expect("Set RDX_GUARD_APK"));
        let snapshot = crate::project_snapshot::ProjectSnapshot::open(&path).unwrap();
        assert_eq!(snapshot.apk_sha256(), APK_SHA256);
        let cancel = AtomicBool::new(false);
        let helper = query(snapshot.engine(), &json!({"method_id":HELPER}), &cancel).unwrap();
        let caller = query(
            snapshot.engine(),
            &json!({"caller_method_id":ZAPPOS_CALLER,"call_site_offset":0x26}),
            &cancel,
        )
        .unwrap();
        if let Some(output) = std::env::var_os("RDX_GUARD_OUTPUT") {
            let report = json!({"apk_sha256":snapshot.apk_sha256(),"apk_path":path,"helper":helper,"caller":caller});
            std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }
        assert_eq!(helper["status"], "proven", "{helper}");
        assert_eq!(helper["cases"].as_array().unwrap().len(), 8);
        assert_eq!(caller["status"], "proven", "{caller}");
        assert_eq!(caller["operation"], "equals");
        assert_eq!(caller["method_id"], HELPER);
        assert_eq!(caller["dex_evidence_sha256"], helper["dex_evidence_sha256"]);
        assert_eq!(caller["call_site"]["code_unit_offset"], 0x26);
        let arguments = caller["call_site"]["arguments"].as_array().unwrap();
        assert_eq!(arguments[0]["kind"], "uri_host", "{arguments:?}");
        assert_eq!(arguments[0]["producer_code_unit_offset"], 0x18);
        assert_eq!(arguments[1]["kind"], "resource_string");
        assert_eq!(arguments[1]["resource_id"], RESOURCE);
        assert_eq!(arguments[1]["producer_code_unit_offset"], 0x21);
        assert_eq!(arguments[1]["resource"]["status"], "resolved");
        assert_eq!(arguments[1]["resource"]["value"], ".zappos.com");
        assert_eq!(arguments[2]["descriptor"], "Z");
        assert_eq!(arguments[2]["status"], "constant");
        assert_eq!(arguments[2]["value"], 0);
        let cases = caller["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 4);
        for (left, right, expected) in [
            ("null", "null", true),
            ("null", "non_null", false),
            ("non_null", "null", false),
        ] {
            let row = cases
                .iter()
                .find(|case| {
                    case["when"][0]["equals"] == left && case["when"][1]["equals"] == right
                })
                .unwrap();
            assert_eq!(row["outcome"]["kind"], "constant");
            assert_eq!(row["outcome"]["value"], expected);
        }
        let comparison = cases
            .iter()
            .find(|case| case["outcome"]["kind"] == "comparison")
            .unwrap();
        assert_eq!(
            comparison["outcome"]["platform_method_id"],
            "java.lang.String.equals(Ljava/lang/Object;)Z"
        );
        assert_eq!(comparison["outcome"]["receiver"]["argument_index"], 0);
        assert_eq!(comparison["outcome"]["operand"]["argument_index"], 1);
    }

    fn method(
        owner: &str,
        name: &str,
        params: &[&str],
        registers: u16,
        words: Vec<u16>,
    ) -> DexMethod {
        DexMethod {
            declaring_type: owner.into(),
            name: name.into(),
            return_type: "Z".into(),
            parameters: params.iter().map(|p| (*p).into()).collect(),
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers,
                ins: params.len() as u16,
                outs: 4,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 24,
            }),
        }
    }

    fn class(descriptor: &str, method: DexMethod, symbols: DexSymbols) -> (String, DexClass) {
        symbols.origin.set("classes.dex".into()).unwrap();
        (
            crate::native_engine::disassembly::type_name(descriptor),
            DexClass {
                descriptor: descriptor.into(),
                superclass: Some("Ljava/lang/Object;".into()),
                interfaces: vec![],
                access_flags: 1,
                symbols: Arc::new(symbols),
                fields: vec![],
                methods: vec![method],
                annotations_offset: 0,
                static_values_offset: 0,
                static_values: vec![],
            },
        )
    }

    fn equality_fixture() -> (String, DexClass) {
        let words = vec![
            0x0039, 0x0008, 0x0139, 0x0004, 0x1012, 0x000f, 0x0012, 0x000f, 0x0239, 0x0007, 0x206e,
            0, 0x0010, 0x000a, 0x000f, 0x206e, 1, 0x0010, 0x000a, 0x000f,
        ];
        let method = method(OWNER, "run", &[STRING, STRING, "Z"], 3, words);
        class(
            OWNER,
            method,
            DexSymbols {
                types: vec![STRING.into()],
                strings: vec!["equals".into(), "equalsIgnoreCase".into()],
                protos: vec![
                    ("Z".into(), vec!["Ljava/lang/Object;".into()]),
                    ("Z".into(), vec![STRING.into()]),
                ],
                methods: vec![(0, 0, 0), (0, 1, 1)],
                ..Default::default()
            },
        )
    }

    #[test]
    fn guard_summaries_prove_null_checks_and_flag_selected_equality() {
        let caller = "Lexample/Caller;";
        let caller_method = method(
            caller,
            "check",
            &[STRING, "Landroid/content/Context;"],
            4,
            vec![
                0x0014,
                RESOURCE as u16,
                (RESOURCE >> 16) as u16,
                0x206e,
                0,
                0x0003,
                0x000c,
                0x0112,
                0x3071,
                1,
                0x0102,
                0x000a,
                0x000f,
            ],
        );
        let caller = class(
            caller,
            caller_method,
            DexSymbols {
                types: vec!["Landroid/content/Context;".into(), OWNER.into()],
                strings: vec!["getString".into(), "run".into()],
                protos: vec![
                    (STRING.into(), vec!["I".into()]),
                    ("Z".into(), vec![STRING.into(), STRING.into(), "Z".into()]),
                ],
                methods: vec![(0, 0, 0), (1, 1, 1)],
                ..Default::default()
            },
        );
        let engine = NativeEngine::from_test_classes(BTreeMap::from([equality_fixture(), caller]));
        let cancel = AtomicBool::new(false);
        let proof = query(&engine, &json!({"method_id":WRAPPER}), &cancel).unwrap();
        assert_eq!(proof["status"], "proven", "{proof}");
        assert!(proof["operation"].is_null());
        assert_eq!(proof["dex_evidence_sha256"].as_str().unwrap().len(), 64);
        let rows = proof["cases"].as_array().unwrap();
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().any(|row| row["when"][0]["equals"] == "null"
            && row["when"][1]["equals"] == "null"
            && row["outcome"]["value"] == true));
        for (flag, operation, pc) in [(false, "equals", 10), (true, "equals_ignore_case", 15)] {
            let row = rows
                .iter()
                .find(|row| {
                    row["when"][0]["equals"] == "non_null"
                        && row["when"][1]["equals"] == "non_null"
                        && row["when"][2]["equals"] == flag
                })
                .unwrap();
            assert_eq!(row["outcome"]["operation"], operation);
            assert_eq!(row["outcome"]["code_unit_offset"], pc);
            assert_eq!(row["outcome"]["receiver"]["argument_index"], 0);
            assert_eq!(row["outcome"]["operand"]["argument_index"], 1);
        }
        let specialized = query(
            &engine,
            &json!({"caller_method_id":CALLER,"call_site_offset":8}),
            &cancel,
        )
        .unwrap();
        assert_eq!(specialized["operation"], "equals", "{specialized}");
        assert_eq!(specialized["call_site"]["arguments"][2]["value"], 0);
        assert_eq!(
            specialized["call_site"]["arguments"][1]["resource"]["status"],
            "unresolved"
        );
        assert_eq!(
            specialized["call_site"]["arguments"][1]["resource_id"],
            RESOURCE
        );
        assert_eq!(
            specialized["call_site"]["arguments"][1]["producer_code_unit_offset"],
            3
        );
        for offset in [9, 10, 12] {
            assert!(
                query(
                    &engine,
                    &json!({"caller_method_id":CALLER,"call_site_offset":offset}),
                    &cancel
                )
                .is_err()
            );
        }
    }

    #[test]
    fn guard_summaries_preserve_operand_direction_null_exceptions_and_hash_symbols() {
        let caller = |api: &str| {
            let method = method(
                OWNER,
                "reversed",
                &[STRING, STRING],
                2,
                vec![0x206e, 0, 0x0001, 0x000a, 0x000f],
            );
            let parameter = if api == "contains" {
                "Ljava/lang/CharSequence;"
            } else {
                STRING
            };
            let class = class(
                OWNER,
                method,
                DexSymbols {
                    types: vec![STRING.into()],
                    strings: vec![api.into()],
                    protos: vec![("Z".into(), vec![parameter.into()])],
                    methods: vec![(0, 0, 0)],
                    ..Default::default()
                },
            );
            NativeEngine::from_test_classes(BTreeMap::from([class]))
        };
        let cancel = AtomicBool::new(false);
        let args =
            json!({"method_id":"example.Guard.reversed(Ljava/lang/String;Ljava/lang/String;)Z"});
        let mut hashes = BTreeSet::new();
        for (api, operation) in [
            ("endsWith", "ends_with"),
            ("startsWith", "starts_with"),
            ("contains", "contains"),
        ] {
            let proof = query(&caller(api), &args, &cancel).unwrap();
            assert_eq!(proof["status"], "proven", "{proof}");
            assert_eq!(proof["operation"], operation);
            let cases = proof["cases"].as_array().unwrap();
            assert_eq!(cases[0]["outcome"]["kind"], "throws_null_pointer");
            assert_eq!(cases[3]["outcome"]["receiver"]["argument_index"], 1);
            assert_eq!(cases[3]["outcome"]["operand"]["argument_index"], 0);
            hashes.insert(proof["dex_evidence_sha256"].as_str().unwrap().to_owned());
        }
        assert_eq!(hashes.len(), 3);
    }

    #[test]
    fn guard_summaries_keep_effects_cycles_and_configured_resource_values_unknown() {
        let params = [STRING, STRING, "Z"];
        let cancel = AtomicBool::new(false);
        for words in [
            vec![0x0028],
            vec![0x3071, 0, 0x0210, 0x000a, 0x000f],
            vec![0x0022, 0, 0x0012, 0x000f],
        ] {
            let method = method(OWNER, "run", &params, 3, words);
            let class = class(
                OWNER,
                method,
                DexSymbols {
                    types: vec![OWNER.into()],
                    strings: vec!["run".into()],
                    protos: vec![("Z".into(), params.iter().map(|p| (*p).into()).collect())],
                    methods: vec![(0, 0, 0)],
                    ..Default::default()
                },
            );
            let engine = NativeEngine::from_test_classes(BTreeMap::from([class]));
            let proof = query(&engine, &json!({"method_id":WRAPPER}), &cancel).unwrap();
            assert_eq!(proof["status"], "unknown", "{proof}");
            assert!(proof["operation"].is_null());
        }
        let mut table = ResourceTable::default();
        table.entries.insert(
            RESOURCE,
            Resource {
                id: RESOURCE,
                package: "example".into(),
                kind: "string".into(),
                name: "comparison_value".into(),
                variants: vec![
                    Variant {
                        configuration: "default".into(),
                        value: "one.example".into(),
                        file: None,
                        reference: None,
                    },
                    Variant {
                        configuration: "fr".into(),
                        value: "other.example".into(),
                        file: None,
                        reference: None,
                    },
                ],
            },
        );
        assert_eq!(resource_evidence(&table, RESOURCE)["status"], "unresolved");
        table.entries.get_mut(&RESOURCE).unwrap().variants[1].value = "one.example".into();
        let proof = resource_evidence(&table, RESOURCE);
        assert_eq!(proof["status"], "resolved");
        assert_eq!(proof["value"], "one.example");
    }

    #[test]
    fn guard_summaries_keep_cross_class_initialization_unknown() {
        const OTHER: &str = "Lexample/Other;";
        let (_, mut comparison) = equality_fixture();
        comparison.descriptor = OTHER.into();
        comparison.methods[0].declaring_type = OTHER.into();
        let mut initializer = method(OTHER, "<clinit>", &[], 1, vec![0x1012, 0x0067, 0, 0x000e]);
        initializer.return_type = "V".into();
        comparison.methods.push(initializer);
        let wrapper = class(
            OWNER,
            method(
                OWNER,
                "run",
                &[STRING, STRING, "Z"],
                3,
                vec![0x3071, 0, 0x0210, 0x000a, 0x000f],
            ),
            DexSymbols {
                types: vec![OTHER.into()],
                strings: vec!["run".into()],
                protos: vec![("Z".into(), vec![STRING.into(), STRING.into(), "Z".into()])],
                methods: vec![(0, 0, 0)],
                ..Default::default()
            },
        );
        let engine = NativeEngine::from_test_classes(BTreeMap::from([
            wrapper,
            ("example.Other".into(), comparison),
        ]));
        let proof = query(
            &engine,
            &json!({"method_id":WRAPPER}),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(proof["status"], "unknown");
        assert!(
            proof["cases"]
                .as_array()
                .unwrap()
                .iter()
                .all(|case| case["outcome"]["reason"]
                    == "cross-class static call may run a class initializer")
        );
    }
}
