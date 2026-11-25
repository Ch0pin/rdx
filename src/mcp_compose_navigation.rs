//! DEX-backed inventory of typed AndroidX Navigation registrations.
use super::*;
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind, instruction_width},
    native_dex::{DexClass, DexMethod, DexSymbols},
    native_engine::disassembly::{method_id, type_name},
    native_ir::{DecodedMethod, Instruction, PoolKind},
};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

const MAP: &str = "Ljava/util/Map;";
const SERIALIZER: &str = "Lkotlinx/serialization/KSerializer;";
const DESCRIPTOR: &str = "Lkotlinx/serialization/descriptors/SerialDescriptor;";
const BUNDLE: &str = "Landroid/os/Bundle;";
const INTENT: &str = "Landroid/content/Intent;";
const STRING: &str = "Ljava/lang/String;";
const CLASS: &str = "Ljava/lang/Class;";
const LIST: &str = "Ljava/util/ArrayList;";
const COMPONENT: &str = "Landroid/content/ComponentName;";
const CONTEXT: &str = "Landroid/content/Context;";
const MAX_METHOD_UNITS: usize = 8192;
const MAX_SCAN_UNITS: usize = 150_000_000;
const MAX_FULL_FLOWS: usize = 4096;
const MAX_MODEL_CANDIDATES: usize = 2048;
const MAX_RECORDS: usize = 8192;
const MAX_RETAINED_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_GRAPH_ASSOCIATIONS: usize = 4096;
const MAX_GRAPH_ASSOCIATION_BYTES: usize = 8 * 1024 * 1024;
const MAX_DRAFT_ROW_BYTES: usize = 192 * 1024;
const MAX_GRAPH_EDGE_BYTES: usize = 16 * 1024;
const MAX_DESCRIPTOR_TEXT_BYTES: usize = 32 * 1024;
const MAX_EVIDENCE_ID_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Call<'a> {
    owner: &'a str,
    name: &'a str,
    args: &'a [Arc<str>],
    ret: &'a str,
}

impl<'a> Call<'a> {
    fn pool(s: &'a DexSymbols, index: usize) -> Option<Self> {
        let &(owner, proto, name) = s.methods.get(index)?;
        let (ret, args) = s.protos.get(proto as usize)?;
        Some(Self {
            owner: s.types.get(owner as usize)?,
            name: s.strings.get(name as usize)?,
            args,
            ret,
        })
    }

    fn at(c: &'a DexClass, i: &Instruction) -> Option<Self> {
        let r = i.reference.filter(|r| r.kind == PoolKind::Method)?;
        Self::pool(&c.symbols, r.index as usize)
    }

    fn definition(m: &'a DexMethod) -> Self {
        Self {
            owner: &m.declaring_type,
            name: &m.name,
            args: &m.parameters,
            ret: &m.return_type,
        }
    }

    fn id(self) -> String {
        format!(
            "{}.{}({}){}",
            type_name(self.owner),
            self.name,
            self.args.join(""),
            self.ret
        )
    }

    fn is(self, owner: &str, name: &str, args: &[&str], ret: &str) -> bool {
        self.owner == owner && self.name == name && self.has_args(args) && self.ret == ret
    }

    fn has_args(self, args: &[&str]) -> bool {
        self.args.iter().map(AsRef::as_ref).eq(args.iter().copied())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Field<'a> {
    owner: &'a str,
    name: &'a str,
    ty: &'a str,
}

impl<'a> Field<'a> {
    fn at(c: &'a DexClass, i: &Instruction) -> Option<Self> {
        let r = i.reference.filter(|r| r.kind == PoolKind::Field)?;
        let &(owner, ty, name) = c.symbols.fields.get(r.index as usize)?;
        Some(Self {
            owner: c.symbols.types.get(owner as usize)?,
            name: c.symbols.strings.get(name as usize)?,
            ty: c.symbols.types.get(ty as usize)?,
        })
    }

    fn id(self) -> String {
        format!("{}.{}:{}", type_name(self.owner), self.name, self.ty)
    }
}

fn definition<'a>(
    engine: &'a NativeEngine,
    call: Call<'_>,
) -> Option<(&'a DexClass, &'a DexMethod)> {
    let class = engine.dex_class(&type_name(call.owner))?;
    let method = class.methods.iter().find(|m| Call::definition(m) == call)?;
    Some((class, method))
}

fn classes<'a>(engine: &'a NativeEngine, ty: &'a str) -> impl Iterator<Item = &'a str> {
    std::iter::successors(Some(ty), |ty| {
        engine.dex_class(&type_name(ty))?.superclass.as_deref()
    })
    .take(32)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Atom<'a> {
    #[default]
    Unknown,
    Input(usize),
    Int(i64),
    Text(&'a str),
    Class(&'a str, usize),
    KClass(&'a str, usize),
    Allocation(&'a str, usize),
    Static(Field<'a>),
    Field(usize),
    Call(usize),
    ArrayGet(usize),
}

struct Flow<'a> {
    class: &'a DexClass,
    method: &'a DexMethod,
    ir: DecodedMethod,
    slots: BTreeMap<usize, usize>,
    before: Vec<Option<Vec<Atom<'a>>>>,
    cfg: ControlFlowGraph,
    forced_successors: Vec<Option<usize>>,
}

fn packed_switch_target(
    code: &crate::native_dex::DexCode,
    i: &Instruction,
    key: i64,
) -> Option<usize> {
    let words = &code.instructions;
    let table = i.payload_target?;
    let int_at =
        |i: usize| Some((u32::from(*words.get(i)?) | (u32::from(*words.get(i + 1)?) << 16)) as i32);
    if words.get(table) != Some(&0x100) {
        return None;
    }
    let count = i64::from(*words.get(table + 1)?);
    let entry = key - i64::from(int_at(table + 2)?);
    if (0..count).contains(&entry) {
        i.pc.checked_add_signed(int_at(table + 4 + 2 * entry as usize)? as isize)
    } else {
        Some(i.pc + i.width)
    }
}

fn kclass_factory(call: Call<'_>) -> bool {
    call.owner.starts_with("Lkotlin/jvm/internal/")
        && call.name == "getOrCreateKotlinClass"
        && call.has_args(&[CLASS])
        && call.ret.starts_with('L')
}

fn is_static(i: &Instruction) -> bool {
    matches!(i.opcode, 0x71 | 0x77)
}

fn value<'a>(i: &Instruction, state: &[Atom<'a>], word: usize) -> Atom<'a> {
    i.reads
        .get(word)
        .map(|r| state[r.register as usize])
        .unwrap_or_default()
}

fn apply<'a>(class: &'a DexClass, i: &Instruction, state: &mut [Atom<'a>]) {
    let next = match i.opcode {
        0x01..=0x09 | 0x1f => value(i, state, 0),
        0x0a..=0x0c => state[state.len() - 1],
        0x12..=0x15 => i.literal.map(Atom::Int).unwrap_or_default(),
        0x1a | 0x1b => i
            .reference
            .and_then(|r| class.symbols.strings.get(r.index as usize))
            .map(|t| Atom::Text(t))
            .unwrap_or_default(),
        0x1c | 0x22 | 0x23 => i
            .reference
            .and_then(|r| class.symbols.types.get(r.index as usize))
            .map(|t| {
                if i.opcode == 0x1c {
                    Atom::Class(t, i.pc)
                } else {
                    Atom::Allocation(t, i.pc)
                }
            })
            .unwrap_or_default(),
        0x44..=0x4a => Atom::ArrayGet(i.pc),
        0x52..=0x58 => Atom::Field(i.pc),
        0x60..=0x66 => Field::at(class, i).map(Atom::Static).unwrap_or_default(),
        _ => Atom::Unknown,
    };
    let pending = if let Some(call) = Call::at(class, i) {
        let class_arg = value(i, state, usize::from(!is_static(i)));
        if kclass_factory(call)
            && let Atom::Class(ty, offset) = class_arg
        {
            Atom::KClass(ty, offset)
        } else {
            Atom::Call(i.pc)
        }
    } else {
        Atom::Unknown
    };
    for w in &i.writes {
        let start = w.register as usize;
        state[start..start + w.kind.word_count()].fill(next);
    }
    *state.last_mut().unwrap() = pending;
}

impl<'a> Flow<'a> {
    fn new(
        class: &'a DexClass,
        method: &'a DexMethod,
        non_null_input: Option<usize>,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        Self::with_captures(class, method, non_null_input, &[], cancel)
    }

    fn with_captures(
        class: &'a DexClass,
        method: &'a DexMethod,
        non_null_input: Option<usize>,
        captured_fields: &[(Field<'a>, Atom<'a>)],
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let code = method.code.as_ref().context("method has no body")?;
        let words = code.instructions.len();
        ensure!(
            words <= MAX_METHOD_UNITS
                && code.registers <= 192
                && words.saturating_mul(code.registers as usize + 1) <= 350_000,
            "Compose navigation method budget exceeded"
        );
        let id_bytes = method
            .declaring_type
            .len()
            .saturating_add(method.name.len())
            .saturating_add(method.return_type.len());
        let id_bytes = method.parameters.iter().fold(id_bytes, |bytes, parameter| {
            bytes.saturating_add(parameter.len())
        });
        ensure!(
            id_bytes <= MAX_EVIDENCE_ID_BYTES,
            "Compose evidence identifier budget exceeded"
        );
        let ir = DecodedMethod::decode(code)?;
        let cfg = ControlFlowGraph::build(code)?;
        let slots: BTreeMap<_, _> = ir
            .instructions
            .iter()
            .enumerate()
            .map(|(n, i)| (i.pc, n))
            .collect();
        let entry = *cfg.block_at.get(&0).context("missing entry block")?;
        let register_count = code.registers as usize;
        let first = code
            .registers
            .checked_sub(code.ins)
            .context("invalid inputs")? as usize;
        let mut start = vec![Atom::Unknown; register_count + 1];
        for (word, cell) in start[first..register_count].iter_mut().enumerate() {
            *cell = Atom::Input(word);
        }
        let mut inputs = vec![None; cfg.blocks.len()];
        inputs[entry] = Some(start);
        let mut before = vec![None; ir.instructions.len()];
        let mut forced_successors = vec![None; cfg.blocks.len()];
        let mut queue = VecDeque::from([entry]);
        let mut queued = vec![false; cfg.blocks.len()];
        queued[entry] = true;
        let mut work = 0usize;
        while let Some(block) = queue.pop_front() {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            queued[block] = false;
            let mut state = inputs[block].clone().unwrap();
            let block_index = block;
            let block = &cfg.blocks[block_index];
            work += block.instructions.len();
            ensure!(work <= 50_000, "Compose navigation flow budget exceeded");
            let mut forced = None;
            for pc in &block.instructions {
                let index = slots[pc];
                let i = &ir.instructions[index];
                before[index] = Some(state.clone());
                if matches!(i.opcode, 0x38 | 0x39) {
                    let nonzero = match value(i, &state, 0) {
                        Atom::Input(n) if non_null_input == Some(n) => Some(true),
                        Atom::Int(n) => Some(n != 0),
                        Atom::Allocation(..) | Atom::KClass(..) | Atom::Class(..) => Some(true),
                        _ => None,
                    };
                    forced = nonzero.map(|n| {
                        if n == (i.opcode == 0x39) {
                            i.branch_target.unwrap_or(i.pc + i.width)
                        } else {
                            i.pc + i.width
                        }
                    });
                }
                if i.opcode == 0x2b
                    && let Atom::Int(key) = value(i, &state, 0)
                {
                    forced = packed_switch_target(code, i, key);
                }
                let capture =
                    if matches!(i.opcode, 0x52..=0x58) && value(i, &state, 0) == Atom::Input(0) {
                        Field::at(class, i)
                            .and_then(|f| captured_fields.iter().find(|(field, _)| *field == f))
                            .map(|(_, atom)| *atom)
                    } else {
                        None
                    };
                apply(class, i, &mut state);
                if let Some(atom) = capture {
                    state[i.writes[0].register as usize] = atom;
                }
            }
            forced_successors[block_index] = forced;
            for edge in &block.successors {
                if edge.kind == EdgeKind::Normal
                    && forced.is_some_and(|pc| pc != cfg.blocks[edge.target].start)
                {
                    continue;
                }
                let incoming = if edge.kind == EdgeKind::Exceptional {
                    vec![Atom::Unknown; state.len()]
                } else {
                    state.clone()
                };
                let changed = match &mut inputs[edge.target] {
                    Some(old) => {
                        let mut changed = false;
                        for (prev, next) in old.iter_mut().zip(incoming) {
                            if *prev != next && *prev != Atom::Unknown {
                                *prev = Atom::Unknown;
                                changed = true;
                            }
                        }
                        changed
                    }
                    slot => {
                        *slot = Some(incoming);
                        true
                    }
                };
                if changed && !queued[edge.target] {
                    queue.push_back(edge.target);
                    queued[edge.target] = true;
                }
            }
        }
        Ok(Self {
            class,
            method,
            ir,
            slots,
            before,
            cfg,
            forced_successors,
        })
    }

    fn at(&self, pc: usize) -> Option<&Instruction> {
        self.ir.instructions.get(*self.slots.get(&pc)?)
    }

    fn atom(&self, i: &Instruction, word: usize) -> Atom<'a> {
        self.before[self.slots[&i.pc]]
            .as_deref()
            .map(|state| value(i, state, word))
            .unwrap_or_default()
    }

    fn instructions(&self) -> impl DoubleEndedIterator<Item = &Instruction> {
        self.ir
            .instructions
            .iter()
            .zip(&self.before)
            .filter_map(|(i, state)| state.is_some().then_some(i))
    }

    fn calls(&self) -> impl Iterator<Item = (&Instruction, Call<'a>)> {
        self.instructions()
            .filter_map(|i| Some((i, Call::at(self.class, i)?)))
    }

    fn origin_call(&self, atom: Atom<'a>) -> Option<(&Instruction, Call<'a>)> {
        let Atom::Call(pc) = atom else { return None };
        let i = self.at(pc)?;
        Some((i, Call::at(self.class, i)?))
    }

    fn origin_field(&self, atom: Atom<'a>) -> Option<(&Instruction, Field<'a>)> {
        let Atom::Field(pc) = atom else { return None };
        let i = self.at(pc)?;
        Some((i, Field::at(self.class, i)?))
    }

    fn dominates(&self, earlier: usize, later: usize) -> bool {
        let Some(&from) = self.cfg.block_at.get(&earlier) else {
            return false;
        };
        let Some(&to) = self.cfg.block_at.get(&later) else {
            return false;
        };
        if from == to {
            return earlier <= later;
        }
        let mut pending = vec![self.cfg.block_at[&0]];
        let mut seen = BTreeSet::new();
        while let Some(index) = pending.pop() {
            if index == from || !seen.insert(index) {
                continue;
            }
            if index == to {
                return false;
            }
            pending.extend(self.cfg.blocks[index].successors.iter().filter_map(|edge| {
                let start = self.cfg.blocks[edge.target].start;
                (edge.kind == EdgeKind::Exceptional
                    || self.forced_successors[index].is_none_or(|pc| pc == start))
                .then_some(edge.target)
            }));
        }
        true
    }

    fn evidence(&self, pc: usize) -> Value {
        json!({
            "method_id": method_id(self.method),
            "dex_origin": self.class.symbols.origin.get(),
            "offset": pc,
        })
    }
}

// Register/test topology is part of this proof. Matching names or literals
// alone would accept unrelated loops or the old object hash implementation.
fn proves_descriptor_hash(class: &DexClass, method: &DexMethod) -> bool {
    let Some(code) = &method.code else {
        return false;
    };
    if code.instructions.len() > 128
        || !code.try_regions.is_empty()
        || method.access_flags & 8 == 0
        || !Call::definition(method).has_args(&[SERIALIZER])
        || code.ins != 1
        || method.return_type.as_ref() != "I"
    {
        return false;
    }
    let Ok(ir) = DecodedMethod::decode(code) else {
        return false;
    };
    let ins: Vec<_> = ir.instructions.iter().filter(|i| i.opcode != 0).collect();
    let [
        a,
        ar,
        b,
        br,
        h,
        hr,
        c,
        cr,
        n,
        nr,
        z,
        test,
        mul,
        d,
        dr,
        e,
        er,
        eh,
        ehr,
        add,
        inc,
        back,
        ret,
    ] = ins.as_slice()
    else {
        return false;
    };
    let read = |i: &Instruction, word: usize| i.reads.get(word).map(|r| r.register);
    let write = |i: &Instruction| i.writes.first().map(|r| r.register);
    let desc = |i: &Instruction| {
        Call::at(class, i)
            .is_some_and(|c| c.name == "getDescriptor" && c.args.is_empty() && c.ret == DESCRIPTOR)
            && read(i, 0) == code.registers.checked_sub(1)
            && i.reads.len() == 1
    };
    let invoked = |i: &Instruction, owner, name, args: &[&str], ret| {
        Call::at(class, i).is_some_and(|c| c.is(owner, name, args, ret))
    };
    let adjacency = |call: &Instruction, mov: &Instruction, op| {
        call.pc + call.width == mov.pc && mov.opcode == op
    };
    let acc = write(hr);
    let index = write(z);
    desc(a)
        && desc(c)
        && desc(d)
        && b.opcode == 0x72
        && n.opcode == 0x72
        && e.opcode == 0x72
        && a.opcode == 0x72
        && c.opcode == 0x72
        && d.opcode == 0x72
        && h.opcode == 0x6e
        && eh.opcode == 0x6e
        && invoked(b, DESCRIPTOR, "getSerialName", &[], STRING)
        && invoked(n, DESCRIPTOR, "getElementsCount", &[], "I")
        && invoked(e, DESCRIPTOR, "getElementName", &["I"], STRING)
        && invoked(h, STRING, "hashCode", &[], "I")
        && invoked(eh, STRING, "hashCode", &[], "I")
        && [
            (a, ar, 0x0c),
            (b, br, 0x0c),
            (h, hr, 0x0a),
            (c, cr, 0x0c),
            (n, nr, 0x0a),
            (d, dr, 0x0c),
            (e, er, 0x0c),
            (eh, ehr, 0x0a),
        ]
        .into_iter()
        .all(|(call, mov, op)| adjacency(call, mov, op))
        && read(b, 0) == write(ar)
        && read(h, 0) == write(br)
        && read(n, 0) == write(cr)
        && read(e, 0) == write(dr)
        && read(e, 1) == index
        && read(eh, 0) == write(er)
        && acc.is_some()
        && index.is_some()
        && acc != index
        && matches!(z.opcode, 0x12..=0x15)
        && z.literal == Some(0)
        && test.opcode == 0x35
        && read(test, 0) == index
        && read(test, 1) == write(nr)
        && test.branch_target == Some(ret.pc)
        && matches!(mul.opcode, 0xda | 0xd2)
        && mul.literal == Some(31)
        && read(mul, 0) == acc
        && write(mul) == acc
        && matches!(add.opcode, 0x90 | 0xb0)
        && read(add, 0) == acc
        && read(add, 1) == write(ehr)
        && write(add) == acc
        && matches!(inc.opcode, 0xd0 | 0xd8)
        && inc.literal == Some(1)
        && read(inc, 0) == index
        && write(inc) == index
        && matches!(back.opcode, 0x28..=0x2a)
        && back.branch_target == Some(test.pc)
        && ret.opcode == 0x0f
        && read(ret, 0) == acc
        && [write(cr), write(nr), write(dr), write(er), write(ehr)]
            .into_iter()
            .all(|r| r != acc)
        && [
            write(ar),
            write(br),
            write(cr),
            write(nr),
            write(dr),
            write(er),
            write(ehr),
        ]
        .into_iter()
        .all(|r| r != code.registers.checked_sub(1))
        && [write(dr), write(er), write(ehr), write(nr)]
            .into_iter()
            .all(|r| r != index)
        && [write(dr), write(er), write(ehr)]
            .into_iter()
            .all(|r| r != write(nr))
        && [acc, index]
            .into_iter()
            .all(|r| r != code.registers.checked_sub(1))
}

fn destination_id(serial_name: &str, names: impl IntoIterator<Item = impl AsRef<str>>) -> i32 {
    let java_hash = |s: &str| {
        s.encode_utf16().fold(0i32, |hash, code_unit| {
            hash.wrapping_mul(31).wrapping_add(i32::from(code_unit))
        })
    };
    names
        .into_iter()
        .fold(java_hash(serial_name), |hash, element| {
            hash.wrapping_mul(31)
                .wrapping_add(java_hash(element.as_ref()))
        })
}

fn verified_serializer_lookup<'a>(
    engine: &'a NativeEngine,
    call: Call<'a>,
    remaining: usize,
    cancel: &AtomicBool,
) -> Option<Value> {
    if remaining == 0 || call.args.len() != 1 || call.ret != SERIALIZER {
        return None;
    }
    let (class, method) = definition(engine, call)?;
    let flow = Flow::new(class, method, None, cancel).ok()?;
    let returned: Vec<_> = flow.instructions().filter(|i| i.opcode == 0x11).collect();
    for ret in returned {
        let Some((lookup, callee)) = flow.origin_call(flow.atom(ret, 0)) else {
            continue;
        };
        if !is_static(lookup) || callee.ret != SERIALIZER {
            continue;
        }
        if callee != call && callee.args == call.args && flow.atom(lookup, 0) == Atom::Input(0) {
            if let Some(evidence) =
                verified_serializer_lookup(engine, callee, remaining - 1, cancel)
            {
                return Some(evidence);
            }
            continue;
        }
        if !callee.has_args(&[CLASS, "[Lkotlinx/serialization/KSerializer;"]) {
            continue;
        }
        let Some((unwrap, java_class)) = flow.origin_call(flow.atom(lookup, 0)) else {
            continue;
        };
        if !is_static(unwrap)
            || flow.atom(unwrap, 0) != Atom::Input(0)
            || java_class.args != call.args
            || java_class.ret != CLASS
            || !verified_java_class(engine, java_class, cancel)
        {
            continue;
        }
        let Some((class, method)) = definition(engine, callee) else {
            continue;
        };
        let Ok(backend) = Flow::new(class, method, None, cancel) else {
            continue;
        };
        let Some((companion_get, _)) = backend.calls().find(|(get, target)| {
            target.is(
                "Ljava/lang/reflect/Field;",
                "get",
                &["Ljava/lang/Object;"],
                "Ljava/lang/Object;",
            ) && backend.atom(get, 1) == Atom::Int(0)
                && backend
                    .origin_call(backend.atom(get, 0))
                    .is_some_and(|(field, target)| {
                        target.is(
                            CLASS,
                            "getDeclaredField",
                            &[STRING],
                            "Ljava/lang/reflect/Field;",
                        ) && backend.atom(field, 0) == Atom::Input(0)
                            && backend.atom(field, 1) == Atom::Text("Companion")
                    })
        }) else {
            continue;
        };
        if backend.calls().any(|(_, target)| {
            target.is(
                "Ljava/lang/reflect/Method;",
                "invoke",
                &["Ljava/lang/Object;", "[Ljava/lang/Object;"],
                "Ljava/lang/Object;",
            )
        }) {
            return Some(json!({
                "kclass_unwrap": flow.evidence(unwrap.pc),
                "serializer_lookup": flow.evidence(lookup.pc),
                "companion_field_read": backend.evidence(companion_get.pc),
            }));
        }
    }
    None
}

struct Base<'a> {
    ctor: Call<'a>,
    build: Call<'a>,
    id_evidence: Value,
}

fn linked_to<'a>(flow: &Flow<'a>, mut atom: Atom<'a>, root: Atom<'a>) -> bool {
    for _ in 0..5 {
        if atom == root && root != Atom::Unknown {
            return true;
        }
        atom = match atom {
            Atom::Field(pc) => flow.at(pc).map(|i| flow.atom(i, 0)).unwrap_or_default(),
            _ => return false,
        };
    }
    false
}

fn model_base<'a>(
    engine: &'a NativeEngine,
    class: &'a DexClass,
    ctor: &'a DexMethod,
    cancel: &AtomicBool,
) -> Option<Base<'a>> {
    let flow = Flow::new(class, ctor, Some(2), cancel).ok()?;
    let (hash_ins, hash, serializer_lookup) = flow.calls().find_map(|(i, hash)| {
        if !is_static(i)
            || !hash.has_args(&[SERIALIZER])
            || hash.ret != "I"
            || !definition(engine, hash)
                .is_some_and(|(class, method)| proves_descriptor_hash(class, method))
        {
            return None;
        }
        let (get, serializer) = flow.origin_call(flow.atom(i, 0))?;
        if !is_static(get)
            || !serializer.has_args(&[ctor.parameters[1].as_ref()])
            || serializer.ret != SERIALIZER
            || flow.atom(get, 0) != Atom::Input(2)
        {
            return None;
        }
        Some((
            i,
            hash,
            verified_serializer_lookup(engine, serializer, 4, cancel)?,
        ))
    })?;
    let (assign, id_field) = flow.instructions().find_map(|i| {
        if i.opcode == 0x59
            && flow.atom(i, 0) == Atom::Input(0)
            && flow.atom(i, 1) == Atom::Call(hash_ins.pc)
        {
            Some((i.pc, Field::at(class, i)?))
        } else {
            None
        }
    })?;
    for method in &class.methods {
        if method.access_flags & 8 != 0
            || !method.parameters.is_empty()
            || !method.return_type.starts_with('L')
        {
            continue;
        }
        let Ok(build) = Flow::new(class, method, None, cancel) else {
            continue;
        };
        let mut returned = build.instructions().filter(|i| i.opcode == 0x11);
        let Some(ret) = returned.next() else { continue };
        let destination = build.atom(ret, 0);
        if returned.next().is_some() || !matches!(destination, Atom::Call(_) | Atom::Allocation(..))
        {
            continue;
        }
        let writes_id = |i: &Instruction, receiver, id| {
            linked_to(&build, build.atom(i, receiver), destination)
                && build
                    .origin_field(build.atom(i, id))
                    .is_some_and(|(get, field)| {
                        field == id_field && build.atom(get, 0) == Atom::Input(0)
                    })
        };
        let same_id = build.instructions().find(|i| {
            (i.opcode == 0x59 && writes_id(i, 0, 1))
                || (Call::at(class, i).is_some_and(|call| {
                    call.is("Landroidx/navigation/NavDestination;", "setId", &["I"], "V")
                }) && writes_id(i, 0, 1))
        });
        if let Some(write) = same_id {
            return Some(Base {
                ctor: Call::definition(ctor),
                build: Call::definition(method),
                id_evidence: json!({
                    "serializer_lookup_and_hash": flow.evidence(hash_ins.pc),
                    "kotlin_serializer_lookup": serializer_lookup,
                    "builder_id_assignment": flow.evidence(assign),
                    "destination_id_assignment": build.evidence(write.pc),
                    "hash_method_id": hash.id(),
                }),
            });
        }
    }
    None
}

struct Graph<'a> {
    nodes: Field<'a>,
    evidence: Value,
}

fn model_graph<'a>(class: &'a DexClass, base: &Base<'_>, cancel: &AtomicBool) -> Option<Graph<'a>> {
    for method in &class.methods {
        if method.access_flags & 8 != 0
            || !method.parameters.is_empty()
            || !method.return_type.starts_with('L')
        {
            continue;
        }
        let Ok(flow) = Flow::new(class, method, None, cancel) else {
            continue;
        };
        let Some((build, _)) = flow
            .calls()
            .find(|(i, call)| *call == base.build && flow.atom(i, 0) == Atom::Input(0))
        else {
            continue;
        };
        for (add, call) in flow.calls() {
            if !call.has_args(&[base.build.ret])
                || call.ret != "V"
                || !linked_to(&flow, flow.atom(add, 0), Atom::Call(build.pc))
            {
                continue;
            }
            let Some((next, _)) = flow.origin_call(flow.atom(add, 1)).filter(|(_, call)| {
                call.is("Ljava/util/Iterator;", "next", &[], "Ljava/lang/Object;")
            }) else {
                continue;
            };
            let Some((iter, _)) = flow.origin_call(flow.atom(next, 0)).filter(|(_, call)| {
                call.name == "iterator"
                    && call.args.is_empty()
                    && call.ret == "Ljava/util/Iterator;"
            }) else {
                continue;
            };
            let Some((get, nodes)) = flow.origin_field(flow.atom(iter, 0)).filter(|(_, nodes)| {
                nodes.owner == class.descriptor.as_ref()
                    && matches!(
                        nodes.ty,
                        LIST | "Ljava/util/List;" | "Ljava/util/Collection;"
                    )
            }) else {
                continue;
            };
            if flow.atom(get, 0) == Atom::Input(0) {
                return Some(Graph {
                    nodes,
                    evidence: flow.evidence(add.pc),
                });
            }
        }
    }
    None
}

struct Activity<'a> {
    template: Field<'a>,
    evidence: Value,
}

fn model_activity<'a>(
    navigator: &'a DexClass,
    base: &Base<'_>,
    cancel: &AtomicBool,
) -> Option<Activity<'a>> {
    for method in &navigator.methods {
        if method.access_flags & 8 != 0
            || method.parameters.len() < 2
            || method.parameters[0].as_ref() != base.build.ret
            || method.parameters[1].as_ref() != BUNDLE
        {
            continue;
        }
        let Ok(flow) = Flow::new(navigator, method, Some(2), cancel) else {
            continue;
        };
        for (put, put_call) in flow.calls() {
            let intent = flow.atom(put, 0);
            if !put_call.is(INTENT, "putExtras", &[BUNDLE], INTENT)
                || flow.atom(put, 1) != Atom::Input(2)
                || !matches!(intent, Atom::Allocation(INTENT, _))
            {
                continue;
            }
            let Some((copy, _)) = flow.calls().find(|(i, call)| {
                i.pc < put.pc
                    && call.is(INTENT, "<init>", &[INTENT], "V")
                    && flow.atom(i, 0) == intent
            }) else {
                continue;
            };
            let Some((load, template)) = flow.origin_field(flow.atom(copy, 1)) else {
                continue;
            };
            if flow.atom(load, 0) != Atom::Input(1) {
                continue;
            }
            let Some((launch, _)) = flow.calls().find(|(i, call)| {
                i.pc > put.pc
                    && matches!(call.owner, CONTEXT | "Landroid/app/Activity;")
                    && call.name == "startActivity"
                    && call.args.first().is_some_and(|a| a.as_ref() == INTENT)
                    && flow.atom(i, 1) == intent
            }) else {
                continue;
            };
            if !flow.dominates(copy.pc, put.pc) || !flow.dominates(put.pc, launch.pc) {
                continue;
            }
            let later_intent_calls = flow
                .calls()
                .filter(|(i, _)| {
                    i.pc > put.pc
                        && i.pc < launch.pc
                        && (0..i.reads.len()).any(|word| flow.atom(i, word) == intent)
                })
                .take(16)
                .map(|(i, call)| {
                    json!({
                        "site": flow.evidence(i.pc), "callee": call.id(),
                    })
                })
                .collect::<Vec<_>>();
            return Some(Activity {
                template,
                evidence: json!({
                    "status": "observed_bundle_copy_and_launch",
                    "scope": "Non-null navigator Bundle reaches Intent.putExtras before launch of the same Intent",
                    "argument_values_at_launch": "unresolved",
                    "later_intent_call_samples": later_intent_calls,
                    "bundle_input_word": 2,
                    "destination_intent_template": flow.evidence(load.pc),
                    "intent_copy": flow.evidence(copy.pc),
                    "intent_put_extras": flow.evidence(put.pc),
                    "start_activity": flow.evidence(launch.pc),
                }),
            });
        }
    }
    None
}

struct TargetField<'a> {
    field: Field<'a>,
    evidence: Value,
}

fn model_activity_target<'a>(
    engine: &NativeEngine,
    class: &'a DexClass,
    activity: &Activity<'_>,
    route_type: &str,
    cancel: &AtomicBool,
) -> Option<TargetField<'a>> {
    for method in &class.methods {
        if method.access_flags & 8 != 0
            || !method.parameters.is_empty()
            || !method.return_type.starts_with('L')
        {
            continue;
        }
        let Ok(flow) = Flow::new(class, method, None, cancel) else {
            continue;
        };
        for (set, call) in flow.calls() {
            if !call.is(INTENT, "setComponent", &[COMPONENT], INTENT) {
                continue;
            }
            let Some((template_load, template)) = flow.origin_field(flow.atom(set, 0)) else {
                continue;
            };
            if template != activity.template {
                continue;
            }
            let dest = flow.atom(template_load, 0);
            if !flow
                .instructions()
                .any(|i| i.opcode == 0x11 && flow.atom(i, 0) == dest)
                || !matches!(dest, Atom::Call(_) | Atom::Allocation(..))
            {
                continue;
            }
            let component = flow.atom(set, 1);
            let Some((make, _)) = flow.calls().find(|(i, call)| {
                i.pc < set.pc
                    && call.is(COMPONENT, "<init>", &[CONTEXT, CLASS], "V")
                    && matches!(component, Atom::Allocation(COMPONENT, _))
                    && flow.atom(i, 0) == component
            }) else {
                continue;
            };
            let (field_load, field) =
                if let Some((convert, java_class)) = flow.origin_call(flow.atom(make, 2)) {
                    if !is_static(convert)
                        || !java_class.has_args(&[route_type])
                        || java_class.ret != CLASS
                        || !verified_java_class(engine, java_class, cancel)
                    {
                        continue;
                    }
                    let Some(resolved) = flow.origin_field(flow.atom(convert, 0)) else {
                        continue;
                    };
                    resolved
                } else {
                    let Some(resolved) = flow.origin_field(flow.atom(make, 2)) else {
                        continue;
                    };
                    resolved
                };
            if field.owner == class.descriptor.as_ref()
                && flow.atom(field_load, 0) == Atom::Input(0)
            {
                return Some(TargetField {
                    field,
                    evidence: json!({
                        "builder_field": field.id(),
                        "builder_field_read": flow.evidence(field_load.pc),
                        "component_constructor": flow.evidence(make.pc),
                        "set_component": flow.evidence(set.pc),
                    }),
                });
            }
        }
    }
    None
}

struct Builder<'a> {
    base: usize,
    route_word: usize,
    start_word: Option<usize>,
    target_word: Option<usize>,
    target_field: Option<TargetField<'a>>,
    kind: &'static str,
    extras: Option<Value>,
}

fn navigator_name(class: &DexClass, allow: impl Fn(&str) -> bool) -> Option<(&str, &'static str)> {
    let annotations = class
        .symbols
        .annotations
        .get(&class.annotations_offset)?
        .class
        .as_ref()?;
    let mut kind = None;
    for annotation in annotations.iter() {
        if annotation.visibility != 1 || annotation.elements.len() != 1 {
            continue;
        }
        let ty = class
            .symbols
            .types
            .get(annotation.type_idx as usize)?
            .as_ref();
        if !allow(ty) {
            continue;
        }
        let (key, val) = &annotation.elements[0];
        if class.symbols.strings.get(*key as usize)?.as_str() != "value" {
            continue;
        }
        let crate::native_dex::DexValue::String(index) = val else {
            continue;
        };
        let found = match class.symbols.strings.get(*index as usize)?.as_str() {
            "activity" => "activity",
            "fragment" => "fragment",
            "composable" => "composable",
            "dialog" => "dialog",
            _ => continue,
        };
        if kind.replace((ty, found)).is_some() {
            return None;
        }
    }
    kind
}

fn builder_model<'a>(
    engine: &'a NativeEngine,
    class: &'a DexClass,
    ctor: &'a DexMethod,
    bases: &[Base<'a>],
    names: &BTreeSet<&str>,
    cancel: &AtomicBool,
) -> Option<Builder<'a>> {
    let flow = Flow::new(class, ctor, None, cancel).ok()?;
    let (base, access) = flow.calls().find_map(|(i, call)| {
        if flow.atom(i, 0) != Atom::Input(0) || !matches!(i.opcode, 0x70 | 0x76) {
            return None;
        }
        Some((bases.iter().position(|b| b.ctor == call)?, i))
    })?;
    let Atom::Input(route_word) = flow.atom(access, 2) else {
        return None;
    };
    let route = ctor.parameters.get(route_word.checked_sub(1)?)?.as_ref();
    if route != bases[base].ctor.args[1].as_ref() {
        return None;
    }
    let navigator = engine.dex_class(&type_name(ctor.parameters.first()?));
    let mut extras = None;
    let mut target_field = None;
    let mut target_word = None;
    let mut start_word = None;
    let kind = if model_graph(class, &bases[base], cancel).is_some() {
        start_word = (route_word == 3).then_some(2);
        "graph"
    } else if let Some(activity) = navigator.and_then(|n| model_activity(n, &bases[base], cancel)) {
        target_field = model_activity_target(engine, class, &activity, route, cancel);
        extras = Some(activity.evidence);
        "activity"
    } else if let Some((_, kind)) = navigator
        .filter(|nav| {
            classes(engine, &nav.descriptor).any(|ty| ty == bases[base].ctor.args[0].as_ref())
        })
        .and_then(|nav| navigator_name(nav, |ty| names.contains(ty)))
    {
        if (kind == "fragment"
            || class
                .descriptor
                .contains("DialogFragmentNavigatorDestinationBuilder"))
            && class
                .descriptor
                .starts_with("Landroidx/navigation/fragment/")
            && ctor.parameters.get(3).is_some_and(|t| t.as_ref() == route)
        {
            target_word = Some(4);
        }
        kind
    } else {
        "navigation_destination"
    };
    Some(Builder {
        base,
        route_word,
        start_word,
        target_word,
        target_field,
        kind,
        extras,
    })
}

fn method_named<'a>(
    c: &'a DexClass,
    name: &str,
    args: &[&str],
    ret: &str,
) -> Option<&'a DexMethod> {
    c.methods
        .iter()
        .find(|m| Call::definition(m).is(&c.descriptor, name, args, ret))
}

fn one_return<'a>(flow: &Flow<'a>) -> Option<Atom<'a>> {
    let mut exits = flow
        .instructions()
        .filter(|i| matches!(i.opcode, 0x0f..=0x11));
    let value = flow.atom(exits.next()?, 0);
    exits.next().is_none().then_some(value)
}

fn verified_java_class(engine: &NativeEngine, call: Call<'_>, cancel: &AtomicBool) -> bool {
    let Some((class, m)) = definition(engine, call) else {
        return false;
    };
    let Ok(flow) = Flow::new(class, m, None, cancel) else {
        return false;
    };
    one_return(&flow)
        .and_then(|atom| flow.origin_call(atom))
        .is_some_and(|(i, call)| {
            call.owner.starts_with("Lkotlin/jvm/internal/")
                && call.name == "getJClass"
                && call.args.is_empty()
                && call.ret == CLASS
                && flow.atom(i, 0) == Atom::Input(0)
        })
}

fn returned_field<'a>(
    class: &'a DexClass,
    method: &'a DexMethod,
    cancel: &AtomicBool,
) -> Option<Field<'a>> {
    let flow = Flow::new(class, method, None, cancel).ok()?;
    let (i, field) = flow.origin_field(one_return(&flow)?)?;
    (flow.atom(i, 0) == Atom::Input(0)).then_some(field)
}

// Generated descriptors expose their actual stored serial name, count, and
// ordered names. Check both getters and the mutation feeding those getters.
fn generated_descriptor<'a>(
    class: &'a DexClass,
    ctor: Call<'_>,
    cancel: &AtomicBool,
) -> Option<Call<'a>> {
    let name = returned_field(
        class,
        method_named(class, "getSerialName", &[], STRING)?,
        cancel,
    )?;
    let count = returned_field(
        class,
        method_named(class, "getElementsCount", &[], "I")?,
        cancel,
    )?;
    let getter = Flow::new(
        class,
        method_named(class, "getElementName", &["I"], STRING)?,
        None,
        cancel,
    )
    .ok()?;
    let Atom::ArrayGet(pc) = one_return(&getter)? else {
        return None;
    };
    let get = getter.at(pc)?;
    let (load, names) = getter.origin_field(getter.atom(get, 0))?;
    if get.opcode != 0x46
        || getter.atom(get, 1) != Atom::Input(1)
        || getter.atom(load, 0) != Atom::Input(0)
    {
        return None;
    }
    let ctor = Flow::new(
        class,
        class.methods.iter().find(|m| Call::definition(m) == ctor)?,
        None,
        cancel,
    )
    .ok()?;
    let sets = |field, op, from| {
        ctor.instructions().any(|i| {
            i.opcode == op
                && Field::at(class, i) == Some(field)
                && ctor.atom(i, 0) == Atom::Input(0)
                && ctor.atom(i, 1) == from
        })
    };
    if !sets(name, 0x5b, Atom::Input(1)) || !sets(count, 0x59, Atom::Input(3)) {
        return None;
    }
    for m in &class.methods {
        if m.access_flags & 8 != 0
            || !Call::definition(m).has_args(&[STRING, "Z"])
            || m.return_type.as_ref() != "V"
        {
            continue;
        }
        let Ok(flow) = Flow::new(class, m, None, cancel) else {
            continue;
        };
        for i in flow.instructions().filter(|i| i.opcode == 0x4d) {
            if flow.atom(i, 2) != Atom::Input(1)
                || !flow
                    .origin_field(flow.atom(i, 0))
                    .is_some_and(|(get, field)| {
                        field == names && flow.atom(get, 0) == Atom::Input(0)
                    })
            {
                continue;
            }
            let idx = i.reads[1].register;
            let Some(inc) = flow
                .instructions()
                .rev()
                .find(|j| j.pc < i.pc && j.writes.iter().any(|w| w.register == idx))
            else {
                continue;
            };
            if !matches!(inc.opcode, 0xd8 | 0xd0) || inc.literal != Some(1) {
                continue;
            }
            let Some((load, cursor)) = flow
                .origin_field(flow.atom(inc, 0))
                .filter(|(get, cursor)| cursor.ty == "I" && flow.atom(get, 0) == Atom::Input(0))
            else {
                continue;
            };
            if load.pc >= inc.pc || !sets(cursor, 0x59, Atom::Int(-1)) {
                continue;
            }
            let stores_cursor = flow.instructions().any(|s| {
                s.pc > inc.pc
                    && s.pc < i.pc
                    && s.opcode == 0x59
                    && Field::at(class, s) == Some(cursor)
                    && flow.atom(s, 0) == Atom::Input(0)
                    && s.reads.get(1).is_some_and(|value| value.register == idx)
            });
            let optional = flow.instructions().any(|s| {
                s.opcode == 0x4e
                    && s.reads.get(1).is_some_and(|index| index.register == idx)
                    && flow.atom(s, 2) == Atom::Input(2)
            });
            if stores_cursor && optional {
                return Some(Call::definition(m));
            }
        }
    }
    None
}

struct RouteDescriptor<'a> {
    serial_name: &'a str,
    arguments: Vec<(&'a str, bool, usize)>,
    source: Value,
}

fn generated_serializer<'a>(
    engine: &'a NativeEngine,
    route: &'a DexClass,
    cancel: &AtomicBool,
) -> Option<RouteDescriptor<'a>> {
    let companion_class = engine.dex_class(&format!("{}$Companion", type_name(&route.descriptor)));
    let (owner, serializer) = [Some(route), companion_class]
        .into_iter()
        .flatten()
        .find_map(|owner| {
            let m = method_named(owner, "serializer", &[], SERIALIZER)?;
            Some((owner, m))
        })?;
    if owner.descriptor != route.descriptor
        && !route.fields.iter().any(|f| {
            f.is_static && f.name.as_ref() == "Companion" && f.field_type == owner.descriptor
        })
    {
        return None;
    }
    let accessor = Flow::new(owner, serializer, None, cancel).ok()?;
    let Atom::Static(singleton) = one_return(&accessor)? else {
        return None;
    };
    let implementation = engine.dex_class(&type_name(singleton.owner))?;
    if singleton.owner != singleton.ty
        || singleton.owner.strip_suffix("$$serializer;") != route.descriptor.strip_suffix(';')
    {
        return None;
    }
    let get = Flow::new(
        implementation,
        method_named(implementation, "getDescriptor", &[], DESCRIPTOR)?,
        None,
        cancel,
    )
    .ok()?;
    let Atom::Static(descriptor_field) = one_return(&get)? else {
        return None;
    };
    let init = Flow::new(
        implementation,
        method_named(implementation, "<clinit>", &[], "V")?,
        None,
        cancel,
    )
    .ok()?;
    if init
        .instructions()
        .any(|i| matches!(i.opcode, 0x27..=0x2c | 0x32..=0x3d))
        || descriptor_field.owner != implementation.descriptor.as_ref()
        || descriptor_field.ty != DESCRIPTOR
    {
        return None;
    }
    let mut singleton_writes = init
        .instructions()
        .filter(|i| i.opcode == 0x69 && Field::at(implementation, i) == Some(singleton));
    let singleton_write = singleton_writes.next()?;
    if singleton_writes.next().is_some()
        || allocation_constructor(&init, init.atom(singleton_write, 0), singleton_write.pc)
            .is_none()
    {
        return None;
    }
    let mut stores = init
        .instructions()
        .filter(|i| i.opcode == 0x69 && Field::at(implementation, i) == Some(descriptor_field));
    let store = stores.next()?;
    if stores.next().is_some() {
        return None;
    }
    let descriptor = init.atom(store, 0);
    let Atom::Allocation(descriptor_type, _) = descriptor else {
        return None;
    };
    let (construct, ctor) = init.calls().find(|(i, call)| {
        call.owner == descriptor_type
            && call.name == "<init>"
            && call.args.len() == 3
            && call.args[0].as_ref() == STRING
            && call.args[2].as_ref() == "I"
            && init.atom(i, 0) == descriptor
    })?;
    let Atom::Text(serial_name) = init.atom(construct, 1) else {
        return None;
    };
    let Atom::Int(count @ 0..=128) = init.atom(construct, 3) else {
        return None;
    };
    let desc_class = engine.dex_class(&type_name(descriptor_type))?;
    let add = generated_descriptor(desc_class, ctor, cancel)?;
    let mut arguments = Vec::new();
    let mut text_bytes = serial_name.len();
    for (i, call) in init.calls() {
        if i.pc <= construct.pc || !(0..i.reads.len()).any(|word| init.atom(i, word) == descriptor)
        {
            continue;
        }
        if call != add || init.atom(i, 0) != descriptor {
            return None;
        }
        let (Atom::Text(name), Atom::Int(optional @ 0..=1)) = (init.atom(i, 1), init.atom(i, 2))
        else {
            return None;
        };
        text_bytes = text_bytes.saturating_add(name.len());
        arguments.push((name, optional == 1, i.pc));
    }
    if count as usize != arguments.len() || text_bytes > MAX_DESCRIPTOR_TEXT_BYTES {
        return None;
    }
    Some(RouteDescriptor {
        serial_name,
        arguments,
        source: json!({
            "mode": "generated_serializer",
            "binding_model": "Bundled Kotlin serialization companion accessor",
            "serializer_accessor": accessor.evidence(0),
            "descriptor_constructor": init.evidence(construct.pc),
            "published_descriptor": init.evidence(store.pc),
            "descriptor_add_element_method_id": add.id(),
        }),
    })
}

fn allocation_constructor<'a, 'f>(
    flow: &'f Flow<'a>,
    object: Atom<'a>,
    use_offset: usize,
) -> Option<(&'f Instruction, Call<'a>)> {
    let Atom::Allocation(ty, allocation) = object else {
        return None;
    };
    flow.calls().find(|(i, call)| {
        i.pc > allocation
            && i.pc < use_offset
            && call.name == "<init>"
            && call.owner == ty
            && matches!(i.opcode, 0x70 | 0x76)
            && flow.atom(i, 0) == object
    })
}

fn captured_function<'a>(
    engine: &'a NativeEngine,
    parent: &Flow<'a>,
    function: Atom<'a>,
    use_offset: usize,
    invoke_args: &[&str],
    cancel: &AtomicBool,
) -> Option<Flow<'a>> {
    let (site, call) = allocation_constructor(parent, function, use_offset)?;
    let (class, ctor) = definition(engine, call)?;
    if class.access_flags & 0x10 == 0 {
        return None;
    }
    let init = Flow::new(class, ctor, None, cancel).ok()?;
    if init
        .instructions()
        .any(|i| matches!(i.opcode, 0x27..=0x2c | 0x32..=0x3d))
    {
        return None;
    }
    let mut fields = Vec::new();
    for i in init
        .instructions()
        .filter(|i| matches!(i.opcode, 0x59 | 0x5b))
    {
        let field = Field::at(class, i)?;
        if field.owner != class.descriptor.as_ref()
            || init.atom(i, 0) != Atom::Input(0)
            || !class
                .fields
                .iter()
                .any(|f| f.name.as_ref() == field.name && f.access_flags & 0x10 != 0)
        {
            return None;
        }
        let captured = match init.atom(i, 1) {
            Atom::Input(word) if word != 0 => parent.atom(site, word),
            v @ (Atom::Text(_) | Atom::Int(_)) => v,
            _ => return None,
        };
        if fields.iter().any(|(f, _)| *f == field) {
            return None;
        }
        fields.push((field, captured));
    }
    if parent.instructions().any(|i| {
        i.pc > site.pc
            && i.pc < use_offset
            && matches!(i.opcode, 0x59..=0x5f)
            && parent.atom(i, 0) == function
    }) {
        return None;
    }
    let method = method_named(class, "invoke", invoke_args, "Ljava/lang/Object;")?;
    Flow::with_captures(class, method, None, &fields, cancel).ok()
}

fn kotlin_lazy_factory(engine: &NativeEngine, call: Call<'_>, cancel: &AtomicBool) -> bool {
    if matches!(call.owner, "Lkotlin/LazyKt;" | "Lkotlin/LazyKt__LazyJVMKt;")
        && call.name == "lazy"
        && call.ret == "Lkotlin/Lazy;"
    {
        return true;
    }
    let Some((class, m)) = definition(engine, call) else {
        return false;
    };
    if call.args.len() != 2 {
        return false;
    }
    let Ok(flow) = Flow::new(class, m, None, cancel) else {
        return false;
    };
    // Kotlin's mode overload selects a Lazy implementation by Enum.ordinal.
    if !flow.calls().any(|(i, c)| {
        c.is("Ljava/lang/Enum;", "ordinal", &[], "I") && flow.atom(i, 0) == Atom::Input(0)
    }) {
        return false;
    }
    let mut returns = flow.instructions().filter(|i| i.opcode == 0x11).peekable();
    returns.peek().is_some()
        && returns.all(|i| {
            let instance = flow.atom(i, 0);
            let Atom::Allocation(ty, _) = instance else {
                return false;
            };
            let Some(impl_class) = engine.dex_class(&type_name(ty)) else {
                return false;
            };
            impl_class.interfaces.iter().any(|t| t.as_ref() == call.ret)
                && (flow.calls().any(|(ctor, call)| {
                    ctor.pc < i.pc
                        && call.owner == ty
                        && call.name == "<init>"
                        && flow.atom(ctor, 0) == instance
                        && flow.atom(ctor, 1) == Atom::Input(1)
                }) || flow.instructions().any(|store| {
                    store.pc < i.pc
                        && store.opcode == 0x5b
                        && flow.atom(store, 0) == instance
                        && flow.atom(store, 1) == Atom::Input(1)
                }))
        })
}

fn lazy_initializer<'a, 'f>(
    engine: &'a NativeEngine,
    init: &'f Flow<'a>,
    atom: Atom<'a>,
    lazy_type: &str,
    cancel: &AtomicBool,
) -> Option<(&'f Instruction, Atom<'a>)> {
    let (i, call) = init.origin_call(atom)?;
    if !is_static(i) || call.ret != lazy_type || !kotlin_lazy_factory(engine, call, cancel) {
        return None;
    }
    Some((i, init.atom(i, call.args.len().checked_sub(1)?)))
}

fn class_init<'a>(class: &'a DexClass, cancel: &AtomicBool) -> Option<Flow<'a>> {
    Flow::new(
        class,
        method_named(class, "<clinit>", &[], "V")?,
        None,
        cancel,
    )
    .ok()
}

fn selected_serializer_initializer<'a>(
    lambda: &Flow<'a>,
    route: &str,
) -> Option<(usize, Call<'a>)> {
    let (i, call) = lambda.origin_call(one_return(lambda)?)?;
    (is_static(i) && call.owner == route && call.ret == SERIALIZER && call.args.is_empty())
        .then_some((i.pc, call))
}

fn object_serializer_accessor<'a>(
    engine: &'a NativeEngine,
    route: &'a DexClass,
    cancel: &AtomicBool,
) -> Option<(Flow<'a>, usize, Call<'a>)> {
    let mut flow = Flow::new(
        route,
        method_named(route, "serializer", &[], SERIALIZER)?,
        None,
        cancel,
    )
    .ok()?;
    for _ in 0..4 {
        let (i, call) = flow.origin_call(one_return(&flow)?)?;
        if call.owner == route.descriptor.as_ref() && call.args.is_empty() && call.ret == SERIALIZER
        {
            let (class, m) = definition(engine, call)?;
            flow = Flow::new(class, m, None, cancel).ok()?;
            continue;
        }
        if call.name != "getValue" || !call.args.is_empty() || call.ret != "Ljava/lang/Object;" {
            return None;
        }
        let Atom::Static(field) = flow.atom(i, 0) else {
            return None;
        };
        if field.owner != route.descriptor.as_ref() || field.ty != call.owner {
            return None;
        }
        let init = class_init(route, cancel)?;
        let mut writes = init
            .instructions()
            .filter(|i| i.opcode == 0x69 && Field::at(route, i) == Some(field));
        let store = writes.next()?;
        if writes.next().is_some() {
            return None;
        }
        let (factory, function) =
            lazy_initializer(engine, &init, init.atom(store, 0), call.owner, cancel)?;
        let lambda = captured_function(engine, &init, function, factory.pc, &[], cancel)?;
        let (pc, called) = selected_serializer_initializer(&lambda, &route.descriptor)?;
        return Some((lambda, pc, called));
    }
    None
}

fn stored_parameter(flow: &Flow<'_>, field: Field<'_>, op: u8, value: usize) -> bool {
    let mut stores = flow.instructions().filter(|i| {
        i.opcode == op
            && Field::at(flow.class, i) == Some(field)
            && flow.atom(i, 0) == Atom::Input(0)
    });
    let Some(store) = stores.next() else {
        return false;
    };
    stores.next().is_none() && flow.atom(store, 1) == Atom::Input(value)
}

fn empty_descriptor_factory(
    engine: &NativeEngine,
    caller: &Flow<'_>,
    use_site: &Instruction,
    factory: Call<'_>,
    cancel: &AtomicBool,
) -> Option<Value> {
    let (class, m) = definition(engine, factory)?;
    let flow = Flow::new(class, m, None, cancel).ok()?;
    let descriptor = one_return(&flow)?;
    let (ctor_ins, ctor) = allocation_constructor(&flow, descriptor, usize::MAX)?;
    let (descriptor_class, constructor) = definition(engine, ctor)?;
    if flow.atom(ctor_ins, 1) != Atom::Input(0) || ctor.owner != factory.ret {
        return None;
    }
    let (count, size) = flow.origin_call(flow.atom(ctor_ins, 3))?;
    if !size.is(LIST, "size", &[], "I") {
        return None;
    }
    let (load, elements) = flow.origin_field(flow.atom(count, 0))?;
    let builder = flow.atom(load, 0);
    let (builder_init, builder_ctor) = allocation_constructor(&flow, builder, load.pc)?;
    let (builder_class, builder_method) = definition(engine, builder_ctor)?;
    let builder_ctor_flow = Flow::new(builder_class, builder_method, None, cancel).ok()?;
    let (list_store, list) = builder_ctor_flow.instructions().find_map(|i| {
        let value = builder_ctor_flow.atom(i, 1);
        (i.opcode == 0x5b
            && Field::at(builder_class, i) == Some(elements)
            && builder_ctor_flow.atom(i, 0) == Atom::Input(0)
            && matches!(value, Atom::Allocation(LIST, _)))
        .then_some((i, value))
    })?;
    let (_, list_ctor) = allocation_constructor(&builder_ctor_flow, list, list_store.pc)?;
    if !list_ctor.is(LIST, "<init>", &[], "V") {
        return None;
    }
    let mut uses = flow.calls().filter(|(i, _)| {
        i.pc > builder_init.pc
            && i.pc < count.pc
            && (0..i.reads.len()).any(|word| flow.atom(i, word) == builder)
    });
    let (invoke, operator) = uses.next()?;
    if uses.next().is_some()
        || operator.name != "invoke"
        || !operator.has_args(&["Ljava/lang/Object;"])
        || flow.atom(invoke, 0) != Atom::Input(3)
        || flow.atom(invoke, 1) != builder
    {
        return None;
    }
    let callback = caller.atom(use_site, 3);
    let Atom::Allocation(callback_type, _) = callback else {
        return None;
    };
    let callback_class = engine.dex_class(&type_name(callback_type))?;
    if !callback_class
        .interfaces
        .iter()
        .any(|t| t.as_ref() == operator.owner)
    {
        return None;
    }
    let body = captured_function(
        engine,
        caller,
        callback,
        use_site.pc,
        &["Ljava/lang/Object;"],
        cancel,
    )?;
    // This supported object descriptor callback only assigns annotations.
    if !matches!(one_return(&body), Some(Atom::Static(_)))
        || body.instructions().any(|i| {
            let writes_array = matches!(i.opcode, 0x4b..=0x51);
            let changes_elements = matches!(i.opcode, 0x59..=0x5f)
                && (!Field::at(body.class, i).is_some_and(|field| {
                    field.owner == elements.owner
                        && field != elements
                        && field.ty == "Ljava/util/List;"
                }) || body.atom(i, 0) != Atom::Input(1));
            let invokes_outside_kotlin_null_check = Call::at(body.class, i).is_some_and(|call| {
                !is_static(i)
                    || !call.owner.starts_with("Lkotlin/jvm/internal/")
                    || !call.has_args(&["Ljava/lang/Object;", STRING])
                    || call.ret != "V"
            });
            writes_array || changes_elements || invokes_outside_kotlin_null_check
        })
    {
        return None;
    }
    let name_field = returned_field(
        descriptor_class,
        method_named(descriptor_class, "getSerialName", &[], STRING)?,
        cancel,
    )?;
    let count_field = returned_field(
        descriptor_class,
        method_named(descriptor_class, "getElementsCount", &[], "I")?,
        cancel,
    )?;
    let constructor_flow = Flow::new(descriptor_class, constructor, None, cancel).ok()?;
    if !stored_parameter(&constructor_flow, name_field, 0x5b, 1)
        || !stored_parameter(&constructor_flow, count_field, 0x59, 3)
    {
        return None;
    }
    Some(json!({
        "descriptor_constructor": flow.evidence(ctor_ins.pc),
        "descriptor_element_count": flow.evidence(count.pc),
        "empty_name_list_initialization": builder_ctor_flow.evidence(list_store.pc),
        "annotations_only_callback": method_id(body.method),
    }))
}

fn object_serializer<'a>(
    engine: &'a NativeEngine,
    route: &'a DexClass,
    cancel: &AtomicBool,
) -> Option<RouteDescriptor<'a>> {
    let (accessor, accessor_pc, mut call) = object_serializer_accessor(engine, route, cancel)?;
    let object = loop {
        let (class, method) = definition(engine, call)?;
        let flow = Flow::new(class, method, None, cancel).ok()?;
        let returned = one_return(&flow)?;
        if matches!(returned, Atom::Allocation(..)) {
            break flow;
        }
        let (i, next) = flow.origin_call(returned)?;
        if next.owner != route.descriptor.as_ref()
            || next == call
            || !is_static(i)
            || !next.args.is_empty()
            || next.ret != SERIALIZER
        {
            return None;
        }
        // Generated accessor adapters have a single delegation layer.
        let (class, method) = definition(engine, next)?;
        let next_flow = Flow::new(class, method, None, cancel).ok()?;
        if !matches!(one_return(&next_flow), Some(Atom::Allocation(..))) {
            return None;
        }
        call = next;
    };
    let (ctor_site, ctor) = allocation_constructor(&object, one_return(&object)?, usize::MAX)?;
    let (class, m) = definition(engine, ctor)?;
    if ctor.args.len() < 2
        || ctor.args[0].as_ref() != STRING
        || ctor.args[1].as_ref() != "Ljava/lang/Object;"
        || !class.interfaces.iter().any(|ty| ty.as_ref() == SERIALIZER)
    {
        return None;
    }
    let Atom::Text(serial_name) = object.atom(ctor_site, 1) else {
        return None;
    };
    if serial_name.len() > MAX_DESCRIPTOR_TEXT_BYTES {
        return None;
    }
    let Atom::Static(instance) = object.atom(ctor_site, 2) else {
        return None;
    };
    if instance.owner != route.descriptor.as_ref()
        || instance.owner != instance.ty
        || instance.name != "INSTANCE"
    {
        return None;
    }
    let desc_getter = Flow::new(
        class,
        method_named(class, "getDescriptor", &[], DESCRIPTOR)?,
        None,
        cancel,
    )
    .ok()?;
    let (value_call, lazy_get) = desc_getter.origin_call(one_return(&desc_getter)?)?;
    if lazy_get.name != "getValue"
        || !lazy_get.args.is_empty()
        || lazy_get.ret != "Ljava/lang/Object;"
    {
        return None;
    }
    let (load, property) = desc_getter.origin_field(desc_getter.atom(value_call, 0))?;
    if desc_getter.atom(load, 0) != Atom::Input(0) {
        return None;
    }
    let mut init = Flow::new(class, m, None, cancel).ok()?;
    let delegate = init
        .calls()
        .find(|(i, call)| {
            call.is(
                &class.descriptor,
                "<init>",
                &[STRING, "Ljava/lang/Object;"],
                "V",
            ) && init.atom(i, 0) == Atom::Input(0)
                && init.atom(i, 1) == Atom::Input(1)
                && init.atom(i, 2) == Atom::Input(2)
        })
        .map(|(_, call)| call);
    if let Some(delegate) = delegate {
        let (_, method) = definition(engine, delegate)?;
        init = Flow::new(class, method, None, cancel).ok()?;
    }
    let store = init.instructions().find(|i| {
        i.opcode == 0x5b
            && Field::at(class, i) == Some(property)
            && init.atom(i, 0) == Atom::Input(0)
    })?;
    let (lazy, initializer) =
        lazy_initializer(engine, &init, init.atom(store, 1), lazy_get.owner, cancel)?;
    let closure = captured_function(engine, &init, initializer, lazy.pc, &[], cancel)?;
    let (create, factory) = closure.origin_call(one_return(&closure)?)?;
    if !is_static(create)
        || factory.args.len() != 4
        || factory.args[0].as_ref() != STRING
        || factory.args[2].as_ref() != format!("[{DESCRIPTOR}")
        || closure.atom(create, 0) != Atom::Input(1)
    {
        return None;
    }
    let factory_evidence = empty_descriptor_factory(engine, &closure, create, factory, cancel)?;
    Some(RouteDescriptor {
        serial_name,
        arguments: Vec::new(),
        source: json!({
            "mode": "object_serializer",
            "binding_model": "Bundled Kotlin Lazy initializer contract",
            "serializer_lazy_callback": accessor.evidence(accessor_pc),
            "serializer_allocation": object.evidence(ctor_site.pc),
            "descriptor_lazy_callback": closure.evidence(create.pc),
            "descriptor_factory": factory_evidence,
        }),
    })
}

struct Site<'a, 'f> {
    builder: &'f Builder<'a>,
    route: Atom<'a>,
    start: Atom<'a>,
    graph: Atom<'a>,
    allocation: Atom<'a>,
    target: Atom<'a>,
    target_write: Option<usize>,
    construct: usize,
    build: usize,
    add: usize,
    template: Option<Value>,
}

fn proven_graph<'a>(engine: &'a NativeEngine, flow: &Flow<'a>, graph: Atom<'a>, ty: &str) -> bool {
    let graph_type = match graph {
        Atom::Allocation(class, _) => Some(class),
        Atom::Input(word) if flow.method.access_flags & 8 != 0 => input_type(flow.method, word),
        Atom::Input(0) => Some(flow.class.descriptor.as_ref()),
        Atom::Input(word) => input_type(flow.method, word),
        _ => None,
    };
    graph_type.is_some_and(|graph| classes(engine, graph).any(|t| t == ty))
}

fn input_type(m: &DexMethod, input: usize) -> Option<&str> {
    let mut word = usize::from(m.access_flags & 8 == 0);
    for ty in &m.parameters {
        if word == input {
            return Some(ty);
        }
        word += if matches!(ty.as_ref(), "J" | "D") {
            2
        } else {
            1
        };
    }
    None
}

fn builder_returns_destination(
    engine: &NativeEngine,
    call: Call<'_>,
    base: Call<'_>,
    remaining: usize,
    cancel: &AtomicBool,
) -> bool {
    if call == base {
        return true;
    }
    if remaining == 0 {
        return false;
    }
    let Some((class, method)) = definition(engine, call) else {
        return false;
    };
    let Ok(flow) = Flow::new(class, method, None, cancel) else {
        return false;
    };
    let mut returns = flow.instructions().filter(|i| i.opcode == 0x11).peekable();
    returns.peek().is_some()
        && returns.all(|ret| {
            flow.origin_call(flow.atom(ret, 0))
                .is_some_and(|(i, called)| {
                    flow.atom(i, 0) == Atom::Input(0)
                        && called.args.is_empty()
                        && (called == base && matches!(i.opcode, 0x6f | 0x75)
                            || called != call
                                && called.owner == call.owner
                                && class.access_flags & 0x10 != 0
                                && builder_returns_destination(
                                    engine,
                                    called,
                                    base,
                                    remaining - 1,
                                    cancel,
                                ))
                })
        })
}

fn inline_sites<'a, 'f>(
    engine: &'a NativeEngine,
    flow: &Flow<'a>,
    bases: &[Base<'a>],
    builders: &'f BTreeMap<Call<'a>, Builder<'a>>,
    graphs: &BTreeMap<&str, Graph<'a>>,
    cancel: &AtomicBool,
) -> Vec<Site<'a, 'f>> {
    let mut sites = Vec::new();
    for (add, call) in flow.calls() {
        if !matches!(
            call.owner,
            LIST | "Ljava/util/List;" | "Ljava/util/Collection;"
        ) || call.name != "add"
            || !call.has_args(&["Ljava/lang/Object;"])
            || call.ret != "Z"
        {
            continue;
        }
        let Some((read, nodes)) = flow.origin_field(flow.atom(add, 0)) else {
            continue;
        };
        let graph = flow.atom(read, 0);
        if !graphs
            .get(nodes.owner)
            .is_some_and(|role| role.nodes == nodes)
            || !proven_graph(engine, flow, graph, nodes.owner)
        {
            continue;
        }
        let Some((build, build_call)) = flow.origin_call(flow.atom(add, 1)) else {
            continue;
        };
        let allocation = flow.atom(build, 0);
        let Atom::Allocation(builder_type, allocation_pc) = allocation else {
            continue;
        };
        let mut ctors = flow.calls().filter(|(i, call)| {
            i.pc > allocation_pc
                && i.pc < build.pc
                && matches!(i.opcode, 0x70 | 0x76)
                && call.owner == builder_type
                && call.name == "<init>"
                && flow.atom(i, 0) == allocation
        });
        let Some((construct, ctor)) = ctors.next() else {
            continue;
        };
        let Some(builder) = builders.get(&ctor) else {
            continue;
        };
        let base = &bases[builder.base];
        if ctors.next().is_some()
            || !build_call.args.is_empty()
            || !classes(engine, builder_type).any(|t| t == build_call.owner)
            || !classes(engine, build_call.ret).any(|t| t == base.build.ret)
            || !builder_returns_destination(engine, build_call, base.build, 3, cancel)
            || !flow.dominates(construct.pc, build.pc)
            || !flow.dominates(build.pc, add.pc)
        {
            continue;
        }
        let route = flow.atom(construct, builder.route_word);
        let start = builder
            .start_word
            .map(|word| flow.atom(construct, word))
            .unwrap_or_default();
        let mut target = builder
            .target_word
            .map(|word| flow.atom(construct, word))
            .unwrap_or_default();
        let mut target_write = None;
        if let Some(model) = &builder.target_field {
            // Mutation proof is limited to one allocation and a linear span.
            // Calls receiving the object can change it and leave the field unknown.
            for i in flow
                .instructions()
                .filter(|i| i.pc > construct.pc && i.pc < build.pc)
            {
                if matches!(i.opcode, 0x27..=0x2c | 0x32..=0x3d)
                    || Call::at(flow.class, i).is_some()
                        && (0..i.reads.len()).any(|word| flow.atom(i, word) == allocation)
                {
                    target = Atom::Unknown;
                    target_write = None;
                    break;
                }
                if i.opcode == 0x5b
                    && Field::at(flow.class, i) == Some(model.field)
                    && flow.atom(i, 0) == allocation
                {
                    target = flow.atom(i, 1);
                    target_write = Some(i.pc);
                }
            }
        }
        sites.push(Site {
            builder,
            route,
            start,
            graph,
            allocation,
            target,
            target_write,
            construct: construct.pc,
            build: build.pc,
            add: add.pc,
            template: None,
        });
    }
    sites
}

struct Wrapper<'a, 'f> {
    builder: &'f Builder<'a>,
    route_word: usize,
    graph_word: usize,
    evidence: Value,
}

fn scan_calls(code: &[u16], ids: &[bool], work: &mut usize, cancel: &AtomicBool) -> Result<bool> {
    let mut pc = 0;
    while pc < code.len() {
        if pc.is_multiple_of(1024) {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        }
        ensure!(
            *work < MAX_SCAN_UNITS,
            "Compose candidate scan budget exceeded"
        );
        let (width, payload) = instruction_width(code, pc)?;
        if !payload
            && matches!(code[pc] as u8, 0x6e..=0x72 | 0x74..=0x78)
            && code.get(pc + 1).and_then(|index| ids.get(*index as usize)) == Some(&true)
        {
            return Ok(true);
        }
        pc += width;
        *work += width;
    }
    Ok(false)
}

struct RetentionBudget {
    max_items: usize,
    max_bytes: usize,
    max_item_bytes: usize,
    items: usize,
    bytes: usize,
}

impl RetentionBudget {
    fn new(max_items: usize, max_bytes: usize, max_item_bytes: usize) -> Self {
        Self {
            max_items,
            max_bytes,
            max_item_bytes,
            items: 0,
            bytes: 0,
        }
    }

    fn retain(&mut self, value: &Value) -> Result<bool> {
        if self.items == self.max_items {
            return Ok(false);
        }
        let mut encoded_bytes = JsonBytes::default();
        serde_json::to_writer(&mut encoded_bytes, value)?;
        let total = self.bytes.saturating_add(encoded_bytes.0);
        if total > self.max_bytes || encoded_bytes.0 > self.max_item_bytes {
            return Ok(false);
        }
        self.items += 1;
        self.bytes = total;
        Ok(true)
    }
}

#[derive(Default)]
struct JsonBytes(usize);

impl std::io::Write for JsonBytes {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Coverage {
    complete: bool,
    failed: usize,
    unresolved_routes: usize,
    unresolved_serializers: usize,
    unresolved_targets: usize,
    unresolved_graphs: usize,
    errors: Vec<Value>,
}

impl Coverage {
    fn error(&mut self, method: &DexMethod, error: anyhow::Error) {
        self.complete = false;
        self.failed += 1;
        if self.errors.len() < 30 {
            self.errors
                .push(json!({"method_id": method_id(method), "reason": error.to_string()}));
        }
    }
}

fn class_constant(atom: Atom<'_>) -> Option<(&str, usize)> {
    match atom {
        Atom::Class(ty, pc) | Atom::KClass(ty, pc) if ty.len() <= MAX_EVIDENCE_ID_BYTES => {
            Some((ty, pc))
        }
        _ => None,
    }
}

fn route_constant(atom: Atom<'_>) -> Option<(&str, usize)> {
    match atom {
        Atom::KClass(ty, pc) if ty.len() <= MAX_EVIDENCE_ID_BYTES => Some((ty, pc)),
        _ => None,
    }
}

struct Record<'a> {
    route: Atom<'a>,
    target: Atom<'a>,
    graph: Atom<'a>,
    owner: &'a DexClass,
    method: &'a DexMethod,
    row: Value,
}

fn draft<'a>(flow: &Flow<'a>, site: Site<'a, '_>, bases: &[Base<'a>]) -> Record<'a> {
    let kind = site.builder.kind;
    let route = route_constant(site.route);
    let target = class_constant(site.target);
    let evidence = if site.template.is_some() {
        json!({
            "registration_call": flow.evidence(site.add),
            "wrapper_registration": site.template,
        })
    } else {
        json!({
            "constructor": flow.evidence(site.construct),
            "build": flow.evidence(site.build),
            "graph_add": flow.evidence(site.add),
        })
    };
    Record {
        route: site.route,
        target: site.target,
        graph: site.graph,
        owner: flow.class,
        method: flow.method,
        row: json!({
            "kind": "compose_registration",
            "destination_kind": kind,
            "method_id": method_id(flow.method),
            "dex_origin": flow.class.symbols.origin.get(),
            "call_site_offset": site.add,
            "offset_unit": "dex_code_units",
            "route_class": route.map(|(ty, _)| type_name(ty)),
            "route_class_evidence": route.map(|(_, offset)| flow.evidence(offset)),
            "destination_class": target.map(|(ty, _)| type_name(ty)),
            "target_class_evidence": target.map(|(_, offset)| flow.evidence(offset)),
            "target_assignment_evidence": site.target_write.map(|offset| flow.evidence(offset)),
            "target_model": site.builder.target_field.as_ref().map(|model| &model.evidence),
            "graph_start_route_class": match site.start {
                Atom::Allocation(ty, _) | Atom::KClass(ty, _) if ty.len() <= MAX_EVIDENCE_ID_BYTES => Some(type_name(ty)),
                _ => None,
            },
            "registration_evidence": evidence,
            "id_model_evidence": bases[site.builder.base].id_evidence,
            "extras_forwarding": site.builder.extras.as_ref().cloned().unwrap_or_else(|| {
                json!({"status": if matches!(kind, "activity" | "fragment") {"unresolved"} else {"not_applicable"}})
            }),
            "external_reachability": "unresolved",
        }),
    }
}

pub(super) fn append(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
    page: &mut scan::Page,
) -> Result<Value> {
    let mut coverage = Coverage {
        complete: true,
        ..Default::default()
    };
    let mut kclass_types = BTreeSet::<&str>::from(["Lkotlin/reflect/KClass;"]);
    let mut seen = BTreeSet::new();
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let Some(class) = engine.dex_class(name) else {
            continue;
        };
        if !seen.insert(Arc::as_ptr(&class.symbols) as usize) {
            continue;
        }
        for index in 0..class.symbols.methods.len() {
            if let Some(call) =
                Call::pool(&class.symbols, index).filter(|call| kclass_factory(*call))
            {
                kclass_types.insert(call.ret);
            }
        }
    }
    let mut bases = Vec::new();
    let mut model_candidates = 0;
    let mut model_budget_exceeded = false;
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let Some(class) = engine.dex_class(name) else {
            continue;
        };
        for ctor in &class.methods {
            if ctor.name.as_ref() == "<init>"
                && ctor.parameters.len() == 3
                && kclass_types.contains(ctor.parameters[1].as_ref())
                && ctor.parameters[2].as_ref() == MAP
            {
                if model_candidates >= MAX_MODEL_CANDIDATES {
                    model_budget_exceeded = true;
                    break;
                }
                model_candidates += 1;
                if let Some(base) = model_base(engine, class, ctor, cancel) {
                    bases.push(base);
                }
            }
        }
    }
    let mut navigator_names = BTreeSet::<&str>::from(["Landroidx/navigation/Navigator$Name;"]);
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let Some(class) = engine.dex_class(name) else {
            continue;
        };
        let Some((marker, "activity")) = navigator_name(class, |ty| {
            engine.dex_class(&type_name(ty)).is_some_and(|a| {
                a.access_flags & 0x2000 != 0 && method_named(a, "value", &[], STRING).is_some()
            })
        }) else {
            continue;
        };
        if bases.iter().any(|base| {
            class.superclass.as_deref() == Some(base.ctor.args[0].as_ref())
                && model_activity(class, base, cancel).is_some()
        }) {
            navigator_names.insert(marker);
        }
    }
    let mut builders = BTreeMap::new();
    let mut graphs = BTreeMap::new();
    if !bases.is_empty() {
        for name in &project.classes {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let Some(class) = engine.dex_class(name) else {
                continue;
            };
            if !class
                .superclass
                .as_deref()
                .is_some_and(|parent| bases.iter().any(|base| base.ctor.owner == parent))
            {
                continue;
            }
            for ctor in &class.methods {
                if ctor.name.as_ref() != "<init>"
                    || ctor.parameters.len() < 3
                    || !ctor
                        .parameters
                        .iter()
                        .any(|ty| kclass_types.contains(ty.as_ref()))
                    || !ctor.parameters.iter().any(|ty| ty.as_ref() == MAP)
                {
                    continue;
                }
                if model_candidates >= MAX_MODEL_CANDIDATES {
                    model_budget_exceeded = true;
                    break;
                }
                model_candidates += 1;
                if let Some(builder) =
                    builder_model(engine, class, ctor, &bases, &navigator_names, cancel)
                {
                    if builder.kind == "graph"
                        && !graphs.contains_key(class.descriptor.as_ref())
                        && let Some(graph) = model_graph(class, &bases[builder.base], cancel)
                    {
                        graphs.insert(class.descriptor.as_ref(), graph);
                    }
                    builders.insert(Call::definition(ctor), builder);
                }
            }
        }
    }
    let mut wrappers = BTreeMap::new();
    if !graphs.is_empty() {
        for name in &project.classes {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let Some(class) = engine.dex_class(name) else {
                continue;
            };
            for method in &class.methods {
                if method.access_flags & 8 == 0
                    || method.parameters.len() < 3
                    || method.return_type.as_ref() != "V"
                    || !graphs.contains_key(method.parameters[0].as_ref())
                    || !kclass_types.contains(method.parameters[1].as_ref())
                    || method.parameters[2].as_ref() != MAP
                {
                    continue;
                }
                if model_candidates >= MAX_MODEL_CANDIDATES {
                    model_budget_exceeded = true;
                    break;
                }
                model_candidates += 1;
                let flow = match Flow::new(class, method, None, cancel) {
                    Ok(flow) => flow,
                    Err(error) => {
                        coverage.error(method, error);
                        continue;
                    }
                };
                let sites = inline_sites(engine, &flow, &bases, &builders, &graphs, cancel);
                let [site] = sites.as_slice() else { continue };
                if site.builder.kind == "graph"
                    || site.builder.kind == "activity"
                    || site.builder.kind == "fragment"
                {
                    continue;
                }
                let (Atom::Input(route_word), Atom::Input(graph_word)) = (site.route, site.graph)
                else {
                    continue;
                };
                wrappers.insert(
                    Call::definition(method),
                    Wrapper {
                        builder: site.builder,
                        route_word,
                        graph_word,
                        evidence: json!({
                            "constructor": flow.evidence(site.construct),
                            "build": flow.evidence(site.build),
                            "graph_add": flow.evidence(site.add),
                        }),
                    },
                );
            }
        }
    }
    let mut records = Vec::new();
    let mut record_budget =
        RetentionBudget::new(MAX_RECORDS, MAX_RETAINED_RECORD_BYTES, MAX_DRAFT_ROW_BYTES);
    let mut graph_budget = RetentionBudget::new(
        MAX_GRAPH_ASSOCIATIONS,
        MAX_GRAPH_ASSOCIATION_BYTES,
        MAX_GRAPH_EDGE_BYTES,
    );
    let mut record_budget_exceeded = false;
    let mut graph_budget_exceeded = false;
    let mut pools = HashMap::new();
    let mut graph_inputs = BTreeMap::<(Call<'_>, usize), Vec<Value>>::new();
    let mut scanned_methods = 0;
    let mut candidate_methods = 0;
    let mut work = 0;
    let mut scan_capped = false;
    let mut missing_scoped_classes = 0;
    if !graphs.is_empty() {
        'classes: for name in &project.classes {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            if !scan::package_matches(name, args) {
                continue;
            }
            let Some(class) = engine.dex_class(name) else {
                missing_scoped_classes += 1;
                continue;
            };
            let ids = pools
                .entry(Arc::as_ptr(&class.symbols) as usize)
                .or_insert_with(|| {
                    (0..class.symbols.methods.len())
                        .map(|idx| {
                            Call::pool(&class.symbols, idx).is_some_and(|call| {
                                builders.contains_key(&call) || wrappers.contains_key(&call)
                            })
                        })
                        .collect::<Vec<_>>()
                });
            if !ids.iter().any(|b| *b) {
                continue;
            }
            for method in &class.methods {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                let Some(code) = &method.code else { continue };
                scanned_methods += 1;
                match scan_calls(&code.instructions, ids, &mut work, cancel) {
                    Ok(false) => continue,
                    Ok(true) => {}
                    Err(error) => {
                        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                        coverage.error(method, error);
                        if work >= MAX_SCAN_UNITS {
                            scan_capped = true;
                            break 'classes;
                        }
                        continue;
                    }
                }
                candidate_methods += 1;
                if candidate_methods > MAX_FULL_FLOWS {
                    scan_capped = true;
                    break 'classes;
                }
                if wrappers.contains_key(&Call::definition(method)) {
                    continue;
                }
                let flow = match Flow::new(class, method, None, cancel) {
                    Ok(flow) => flow,
                    Err(error) => {
                        coverage.error(method, error);
                        continue;
                    }
                };
                let sites = inline_sites(engine, &flow, &bases, &builders, &graphs, cancel);
                let local_graphs: Vec<_> = sites
                    .iter()
                    .filter(|site| site.builder.kind == "graph")
                    .filter_map(|site| {
                        Some((
                            site.allocation,
                            route_constant(site.route)?,
                            site.construct,
                            site.add,
                        ))
                    })
                    .collect();
                for (i, target) in flow.calls() {
                    if graphs.contains_key(target.owner) || builders.contains_key(&target) {
                        continue;
                    }
                    for word in 0..i.reads.len() {
                        let atom = flow.atom(i, word);
                        if let Some((_, (route, _), ctor, add)) = local_graphs
                            .iter()
                            .find(|(allocation, _, ctor, _)| *allocation == atom && *ctor < i.pc)
                        {
                            let Some((_, target_method)) = definition(engine, target) else {
                                continue;
                            };
                            let Atom::Allocation(ty, _) = atom else {
                                continue;
                            };
                            if input_type(target_method, word).is_none_or(|arg| arg != ty) {
                                continue;
                            }
                            let key = (target, word);
                            if graph_inputs
                                .get(&key)
                                .is_some_and(|edges| edges.len() >= 32)
                            {
                                graph_budget_exceeded = true;
                                continue;
                            }
                            let edge = json!({
                                "graph_route_class": type_name(route),
                                "caller_graph_constructor": flow.evidence(*ctor),
                                "caller_graph_insertion": flow.evidence(*add),
                                "caller_graph_argument": flow.evidence(i.pc),
                                "callee_input_word": word,
                            });
                            if graph_budget.retain(&edge)? {
                                graph_inputs.entry(key).or_default().push(edge);
                            } else {
                                graph_budget_exceeded = true;
                            }
                        }
                    }
                }
                for site in sites
                    .into_iter()
                    .chain(flow.calls().filter_map(|(i, call)| {
                        let wrapper = wrappers.get(&call)?;
                        Some(Site {
                            builder: wrapper.builder,
                            route: flow.atom(i, wrapper.route_word),
                            start: Atom::Unknown,
                            graph: flow.atom(i, wrapper.graph_word),
                            allocation: Atom::Unknown,
                            target: Atom::Unknown,
                            target_write: None,
                            construct: i.pc,
                            build: i.pc,
                            add: i.pc,
                            template: Some(wrapper.evidence.clone()),
                        })
                    }))
                {
                    let mut record = draft(&flow, site, &bases);
                    if let Some((_, (route, _), ctor, _)) = local_graphs
                        .iter()
                        .find(|(allocation, _, _, _)| *allocation == record.graph)
                    {
                        record.row["graph"] = json!({
                            "status": "same_method_graph_builder",
                            "route_class": type_name(route),
                            "graph_constructor": flow.evidence(*ctor),
                        });
                    }
                    if !record_budget.retain(&record.row)? {
                        record_budget_exceeded = true;
                        break 'classes;
                    }
                    records.push(record);
                }
            }
        }
    }
    let mut descriptors = BTreeMap::new();
    let count = records.len();
    for mut record in records {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let route = route_constant(record.route).map(|(ty, _)| ty);
        let target = class_constant(record.target).map(|(ty, _)| ty);
        if record.row["graph"].is_null() {
            if let Atom::Input(word) = record.graph
                && let Some(associations) =
                    graph_inputs.get(&(Call::definition(record.method), word))
            {
                record.row["graph"] = json!({
                    "status": "observed_caller_graph_arguments",
                    "callee_input_word": word,
                    "callers": associations,
                });
            } else {
                coverage.unresolved_graphs += 1;
                record.row["graph"] = json!({"status": "unresolved"});
            }
        }
        if target.is_none()
            && matches!(
                record.row["destination_kind"].as_str(),
                Some("activity" | "fragment")
            )
        {
            coverage.unresolved_targets += 1;
        }
        if route.is_none() {
            coverage.unresolved_routes += 1;
        }
        let descriptor = route.and_then(|type_id| {
            descriptors
                .entry(type_id)
                .or_insert_with(|| {
                    let class = engine.dex_class(&type_name(type_id))?;
                    generated_serializer(engine, class, cancel)
                        .or_else(|| object_serializer(engine, class, cancel))
                })
                .as_ref()
        });
        if let Some(d) = descriptor {
            let id = destination_id(d.serial_name, d.arguments.iter().map(|(name, _, _)| *name));
            record.row["destination_id"] = json!(id);
            record.row["destination_id_hex"] = json!(format!("0x{:08x}", id as u32));
            record.row["serializer"] = json!({
                "status": "verified_descriptor",
                "serial_name": d.serial_name,
                "evidence": d.source,
            });
            record.row["arguments"] = json!(
                d.arguments
                    .iter()
                    .enumerate()
                    .map(|(index, (name, optional, offset))| json!({
                        "index": index, "name": name, "optional": optional,
                        "descriptor_add_element_offset": offset
                    }))
                    .collect::<Vec<_>>()
            );
        } else {
            coverage.unresolved_serializers += usize::from(route.is_some());
            record.row["destination_id"] = Value::Null;
            record.row["serializer"] = json!({"status": "unresolved"});
            record.row["arguments"] = Value::Null;
        }
        record.row["graph_registration_model"] = match record.graph {
            Atom::Allocation(ty, _) => graphs.get(ty),
            Atom::Input(word) => input_type(record.method, word).and_then(|ty| graphs.get(ty)),
            _ => None,
        }
        .map(|graph| {
            json!({
                "nodes_field": graph.nodes.id(),
                "node_consumption": graph.evidence,
            })
        })
        .unwrap_or(Value::Null);
        debug_assert_eq!(record.owner.descriptor, record.method.declaring_type);
        page.add(record.row)?;
    }
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    Ok(json!({
        "complete": coverage.complete && !scan_capped && !model_budget_exceeded
            && !record_budget_exceeded && !graph_budget_exceeded && missing_scoped_classes == 0,
        "coverage_basis": "Completeness refers to the bounded scan of recognized DEX models. Unsupported models and unresolved fields can occur even when the scan completes. No supported model does not establish that programmatic navigation is absent.",
        "model_candidates_examined": model_candidates,
        "model_budget_exceeded": model_budget_exceeded,
        "status": if bases.is_empty() {"no_supported_typed_destination_model"} else if graphs.is_empty() {"no_supported_graph_registration_model"} else {"scanned"},
        "scope": "registration method package prefix",
        "supported_patterns": "typed DEX builder construction, destination build and graph insertion, plus matching Compose wrappers",
        "verified_base_models": bases.len(),
        "verified_builder_constructors": builders.len(),
        "verified_graph_models": graphs.len(),
        "verified_registration_wrappers": wrappers.len(),
        "scanned_methods": scanned_methods,
        "candidate_methods": candidate_methods,
        "registration_records": count,
        "missing_scoped_classes": missing_scoped_classes,
        "failed_methods": coverage.failed,
        "errors": coverage.errors,
        "errors_truncated": coverage.failed > 30,
        "scan_budget_exceeded": scan_capped,
        "record_budget_exceeded": record_budget_exceeded,
        "retained_record_bytes": record_budget.bytes,
        "record_byte_budget": MAX_RETAINED_RECORD_BYTES,
        "draft_row_byte_budget": MAX_DRAFT_ROW_BYTES,
        "graph_association_budget_exceeded": graph_budget_exceeded,
        "retained_graph_associations": graph_budget.items,
        "retained_graph_association_bytes": graph_budget.bytes,
        "graph_association_byte_budget": MAX_GRAPH_ASSOCIATION_BYTES,
        "graph_association_edge_byte_budget": MAX_GRAPH_EDGE_BYTES,
        "unresolved_routes": coverage.unresolved_routes,
        "unresolved_serializers": coverage.unresolved_serializers,
        "unresolved_targets": coverage.unresolved_targets,
        "unresolved_graphs": coverage.unresolved_graphs,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::DexCode;

    fn hash_fixture() -> (DexClass, DexMethod) {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.symbols = Arc::new(DexSymbols {
            strings: [
                "getDescriptor",
                "getSerialName",
                "hashCode",
                "getElementsCount",
                "getElementName",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            types: [SERIALIZER, DESCRIPTOR, STRING]
                .into_iter()
                .map(Arc::from)
                .collect(),
            methods: vec![(0, 0, 0), (1, 1, 1), (2, 2, 2), (1, 2, 3), (1, 3, 4)],
            protos: vec![
                (Arc::from(DESCRIPTOR), Vec::new()),
                (Arc::from(STRING), Vec::new()),
                (Arc::from("I"), Vec::new()),
                (Arc::from(STRING), vec![Arc::from("I")]),
            ],
            ..Default::default()
        });
        let method = DexMethod {
            declaring_type: Arc::from("Lfixture/Routes;"),
            name: Arc::from("id"),
            return_type: Arc::from("I"),
            parameters: vec![Arc::from(SERIALIZER)],
            access_flags: 8,
            thrown_types: Vec::new(),
            code: Some(DexCode {
                registers: 5,
                ins: 1,
                outs: 2,
                tries: 0,
                try_regions: Vec::new(),
                offset: 0,
                instructions: vec![
                    0x1072, 0, 4, 0x000c, // descriptor
                    0x1072, 1, 0, 0x000c, // serial name
                    0x106e, 2, 0, 0x000a, // initial hash
                    0x1072, 0, 4, 0x010c, // descriptor
                    0x1072, 3, 1, 0x010a, // count
                    0x0212, 0x1235, 0x0014, // index and loop exit
                    0x00da, 0x1f00, // multiply accumulator by 31
                    0x1072, 0, 4, 0x030c, // descriptor
                    0x2072, 4, 0x0023, 0x030c, // element name
                    0x106e, 2, 3, 0x030a, // element name hash
                    0x30b0, 0x02d8, 0x0102, 0xed28, // add, increment, loop
                    0x000f,
                ],
            }),
        };
        (class, method)
    }

    #[test]
    fn rejects_hash_lookalikes() {
        let (class, mut method) = hash_fixture();
        assert!(proves_descriptor_hash(&class, &method));
        for (pc, replacement) in [
            (0x18, 0x1100),
            (0x1f, 0x0013),
            (0x25, 0x20b0),
            (0x27, 0x0202),
            (0x28, 0xee28),
            (0x29, 0x010f),
        ] {
            let code = method.code.as_mut().unwrap();
            let before = std::mem::replace(&mut code.instructions[pc], replacement);
            assert!(
                !proves_descriptor_hash(&class, &method),
                "accepted mutation at {pc:x}"
            );
            method.code.as_mut().unwrap().instructions[pc] = before;
        }
    }

    #[test]
    fn hashes_descriptor_names_with_java_utf16_and_wrapping() {
        assert_eq!(destination_id("alias", ["first", "second"]), 1_199_835_668);
        assert_eq!(destination_id("😀", Vec::<&str>::new()), 1_772_899);
        assert_ne!(
            destination_id("alias", ["first", "second"]),
            destination_id("alias", ["second", "first"])
        );
    }
    fn class_fixture(
        ty: &str,
        parent: &str,
        symbols: DexSymbols,
        methods: Vec<DexMethod>,
    ) -> DexClass {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.descriptor = Arc::from(ty);
        class.superclass = Some(Arc::from(parent));
        class.symbols = Arc::new(symbols);
        class.methods = methods;
        class.fields.clear();
        class
    }

    fn method_fixture(
        ty: &str,
        name: &str,
        args: &[&str],
        ret: &str,
        static_flag: bool,
        registers: u16,
        instructions: Vec<u16>,
    ) -> DexMethod {
        DexMethod {
            declaring_type: Arc::from(ty),
            name: Arc::from(name),
            return_type: Arc::from(ret),
            parameters: args.iter().map(|s| Arc::from(*s)).collect(),
            access_flags: if static_flag { 8 } else { 1 },
            thrown_types: Vec::new(),
            code: Some(DexCode {
                registers,
                ins: args.len() as u16 + u16::from(!static_flag),
                outs: 4,
                tries: 0,
                try_regions: Vec::new(),
                instructions,
                offset: 0,
            }),
        }
    }

    #[test]
    fn registration_requires_graph_receiver_and_tracks_distinct_classes() {
        const BUILDER: &str = "Lfixture/ActivityBuilder;";
        const BASE: &str = "Lfixture/BaseBuilder;";
        const GRAPH: &str = "Lfixture/Graph;";
        const ROUTE: &str = "Lfixture/Route;";
        const TARGET: &str = "Lfixture/DestinationActivity;";
        const DEST: &str = "Lfixture/Destination;";
        const REFLECT: &str = "Lkotlin/jvm/internal/Reflection;";
        const KCLASS: &str = "Lkotlin/reflect/KClass;";
        let symbols = DexSymbols {
            strings: [
                "getOrCreateKotlinClass",
                "<init>",
                "build",
                "add",
                "mutate",
                "activityClass",
                "nodes",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            types: [
                BUILDER,
                BASE,
                GRAPH,
                ROUTE,
                TARGET,
                DEST,
                REFLECT,
                KCLASS,
                LIST,
                "Lfixture/Program;",
            ]
            .into_iter()
            .map(Arc::from)
            .collect(),
            methods: vec![
                (6, 0, 0),
                (0, 1, 1),
                (0, 2, 2),
                (8, 3, 3),
                (9, 4, 4),
                (1, 2, 2),
            ],
            protos: vec![
                (Arc::from(KCLASS), vec![Arc::from(CLASS)]),
                (
                    Arc::from("V"),
                    [CLASS, KCLASS, MAP].into_iter().map(Arc::from).collect(),
                ),
                (Arc::from(DEST), Vec::new()),
                (Arc::from("Z"), vec![Arc::from("Ljava/lang/Object;")]),
                (Arc::from("V"), vec![Arc::from(BUILDER)]),
            ],
            fields: vec![(0, 7, 5), (2, 8, 6)],
            ..Default::default()
        };
        let base = Base {
            ctor: Call::pool(&symbols, 1).unwrap(),
            build: Call::pool(&symbols, 5).unwrap(),
            id_evidence: Value::Null,
        };
        let bases = [base];
        let builder = Builder {
            base: 0,
            route_word: 2,
            start_word: None,
            target_word: None,
            target_field: Some(TargetField {
                field: Field {
                    owner: BUILDER,
                    name: "activityClass",
                    ty: KCLASS,
                },
                evidence: Value::Null,
            }),
            kind: "activity",
            extras: None,
        };
        let builders = BTreeMap::from([(Call::pool(&symbols, 1).unwrap(), builder)]);
        let graphs = BTreeMap::from([(
            GRAPH,
            Graph {
                nodes: Field {
                    owner: GRAPH,
                    name: "nodes",
                    ty: LIST,
                },
                evidence: Value::Null,
            },
        )]);
        let program = vec![
            0x0022, 0, // builder allocation v0
            0x011c, 3, // route class v1
            0x1071, 0, 1, 0x010c, // v1 = KClass(v1)
            0x0212, 0x0312, // navigator, map
            0x4070, 1, 0x3120, // builder ctor (v0, v2, v1, v3)
            0x041c, 4, // target class v4
            0x1071, 0, 4, 0x040c, // v4 = KClass(v4)
            0x045b, 0, // write v4 into builder v0
            0x8554, 1, // list from graph v8 into v5
            0x106e, 2, 0, 0x060c, // build v0, move to v6
            0x206e, 3, 0x0065, // add to v5
            0x000e,
        ];
        let type_parent = "Ljava/lang/Object;";
        let build_class = class_fixture(
            BUILDER,
            BASE,
            DexSymbols {
                strings: symbols.strings.clone(),
                types: symbols.types.clone(),
                protos: symbols.protos.clone(),
                methods: symbols.methods.clone(),
                ..Default::default()
            },
            vec![method_fixture(
                BUILDER,
                "build",
                &[],
                DEST,
                false,
                2,
                vec![0x106f, 5, 1, 0x000c, 0x0011],
            )],
        );
        let engine =
            NativeEngine::from_test_classes(BTreeMap::from([(type_name(BUILDER), build_class)]));
        let builds = |words| {
            method_fixture(
                "Lfixture/Program;",
                "register",
                &[GRAPH],
                "V",
                true,
                9,
                words,
            )
        };
        let class = class_fixture(
            "Lfixture/Program;",
            type_parent,
            DexSymbols {
                strings: symbols.strings.clone(),
                types: symbols.types.clone(),
                protos: symbols.protos.clone(),
                methods: symbols.methods.clone(),
                fields: symbols.fields.clone(),
                ..Default::default()
            },
            Vec::new(),
        );
        let cancel = AtomicBool::new(false);
        let method = builds(program.clone());
        let flow = Flow::new(&class, &method, None, &cancel).unwrap();
        let sites = inline_sites(&engine, &flow, &bases, &builders, &graphs, &cancel);
        let [site] = sites.as_slice() else {
            panic!("expected one registration");
        };
        assert_eq!(site.route, Atom::KClass(ROUTE, 2));
        assert_eq!(site.target, Atom::KClass(TARGET, 0xd));
        assert_eq!(site.target_write, Some(0x13));

        let mut wrong_graph = program.clone();
        wrong_graph[0x15] = 0x7554;
        let method = builds(wrong_graph);
        let flow = Flow::new(&class, &method, None, &cancel).unwrap();
        assert!(inline_sites(&engine, &flow, &bases, &builders, &graphs, &cancel).is_empty());

        let mut missing_add = program.clone();
        missing_add.splice(0x1b..0x1e, []);
        let method = builds(missing_add);
        let flow = Flow::new(&class, &method, None, &cancel).unwrap();
        assert!(inline_sites(&engine, &flow, &bases, &builders, &graphs, &cancel).is_empty());

        let mut mutator = program.clone();
        mutator.splice(0x15..0x15, [0x1071, 4, 0]);
        let method = builds(mutator);
        let flow = Flow::new(&class, &method, None, &cancel).unwrap();
        let sites = inline_sites(&engine, &flow, &bases, &builders, &graphs, &cancel);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].target, Atom::Unknown);
        assert_eq!(sites[0].target_write, None);
    }

    #[test]
    fn extras_evidence_requires_bundle_input_and_launched_intent() {
        const NAV: &str = "Lfixture/ActivityNavigator;";
        const DEST: &str = "Lfixture/ActivityDestination;";
        let symbols = || DexSymbols {
            strings: [
                "<init>",
                "putExtras",
                "startActivity",
                "intent",
                "replaceExtras",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            types: [INTENT, CONTEXT, DEST].into_iter().map(Arc::from).collect(),
            fields: vec![(2, 0, 3)],
            methods: vec![(0, 0, 0), (0, 1, 1), (1, 0, 2), (0, 1, 4)],
            protos: vec![
                (Arc::from("V"), vec![Arc::from(INTENT)]),
                (Arc::from(INTENT), vec![Arc::from(BUNDLE)]),
            ],
            ..Default::default()
        };
        let build = Call {
            owner: "Lfixture/Builder;",
            name: "build",
            args: &[],
            ret: DEST,
        };
        let base = Base {
            ctor: build,
            build,
            id_evidence: Value::Null,
        };
        let code = vec![
            0x4054, 0, // v0 = p1.intent
            0x0122, 0, // new Intent v1
            0x2070, 0, 0x0001, // copy from v0
            0x206e, 1, 0x0051, // put extras from p2 (= v5)
            0x206e, 2, 0x0016, // p3.startActivity(v1)
            0x000e,
        ];
        let clazz = |code| {
            class_fixture(
                NAV,
                "Ljava/lang/Object;",
                symbols(),
                vec![method_fixture(
                    NAV,
                    "navigate",
                    &[DEST, BUNDLE, CONTEXT],
                    "V",
                    false,
                    7,
                    code,
                )],
            )
        };
        let cancel = AtomicBool::new(false);
        let class = clazz(code.clone());
        assert_eq!(
            model_activity(&class, &base, &cancel).unwrap().evidence["intent_put_extras"]["offset"],
            7
        );
        let mut changed_bundle = code.clone();
        changed_bundle[9] = 0x0041;
        assert!(model_activity(&clazz(changed_bundle), &base, &cancel).is_none());
        let mut changed_intent = code.clone();
        changed_intent[12] = 0x0006;
        assert!(model_activity(&clazz(changed_intent), &base, &cancel).is_none());
        let mut skipped_put = code.clone();
        skipped_put.splice(7..7, [0x0638, 5]); // A path skips putExtras.
        assert!(model_activity(&clazz(skipped_put), &base, &cancel).is_none());

        let mut replaced_after_put = code;
        replaced_after_put.splice(10..10, [0x206e, 3, 0x0051]);
        let replaced = clazz(replaced_after_put);
        let evidence = model_activity(&replaced, &base, &cancel).unwrap().evidence;
        assert_eq!(evidence["status"], "observed_bundle_copy_and_launch");
        assert_eq!(evidence["argument_values_at_launch"], "unresolved");
        assert_eq!(
            evidence["later_intent_call_samples"][0]["callee"],
            "android.content.Intent.replaceExtras(Landroid/os/Bundle;)Landroid/content/Intent;"
        );
    }
    #[test]
    fn generated_serializer_uses_serial_alias_and_ordered_elements() {
        const ROUTE: &str = "Lfixture/Route;";
        const SER: &str = "Lfixture/Route$$serializer;";
        const DESC: &str = "Lfixture/PluginDescriptor;";
        const OBJECT: &str = "Ljava/lang/Object;";
        let engine = |count: u16, reverse: bool| {
            let fixture_pool = || DexSymbols {
                types: [
                    SER,
                    DESC,
                    DESCRIPTOR,
                    STRING,
                    "I",
                    "[Ljava/lang/String;",
                    "[Z",
                ]
                .into_iter()
                .map(Arc::from)
                .collect(),
                strings: [
                    "INSTANCE",
                    "descriptor",
                    "<init>",
                    "addElement",
                    "test.SerialAlias",
                    "url",
                    "sizing",
                    "serialName",
                    "count",
                    "cursor",
                    "names",
                    "optional",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                fields: vec![
                    (0, 0, 0),
                    (0, 2, 1),
                    (1, 3, 7),
                    (1, 4, 8),
                    (1, 4, 9),
                    (1, 5, 10),
                    (1, 6, 11),
                ],
                methods: vec![(0, 0, 2), (1, 1, 2), (1, 2, 3)],
                protos: vec![
                    (Arc::from("V"), Vec::new()),
                    (
                        Arc::from("V"),
                        [STRING, OBJECT, "I"].into_iter().map(Arc::from).collect(),
                    ),
                    (
                        Arc::from("V"),
                        [STRING, "Z"].into_iter().map(Arc::from).collect(),
                    ),
                ],
                ..Default::default()
            };
            let make = |ty, name, args: &[&str], ret, words, code| {
                method_fixture(ty, name, args, ret, false, words, code)
            };
            let route = class_fixture(
                ROUTE,
                OBJECT,
                fixture_pool(),
                vec![make(
                    ROUTE,
                    "serializer",
                    &[],
                    SERIALIZER,
                    1,
                    vec![0x0062, 0, 0x0011],
                )],
            );
            let (first, second) = if reverse { (6, 5) } else { (5, 6) };
            let implementation = class_fixture(
                SER,
                OBJECT,
                fixture_pool(),
                vec![
                    make(
                        SER,
                        "getDescriptor",
                        &[],
                        DESCRIPTOR,
                        1,
                        vec![0x0062, 1, 0x0011],
                    ),
                    method_fixture(
                        SER,
                        "<clinit>",
                        &[],
                        "V",
                        true,
                        4,
                        vec![
                            0x0022,
                            0,
                            0x1070,
                            0,
                            0,
                            0x0069,
                            0, // singleton
                            0x0122,
                            1,
                            0x021a,
                            4,
                            0x0312 | (count << 12),
                            0x4070,
                            1,
                            0x3021,
                            0x021a,
                            first,
                            0x0312,
                            0x306e,
                            2,
                            0x0321,
                            0x021a,
                            second,
                            0x1312,
                            0x306e,
                            2,
                            0x0321,
                            0x0169,
                            1,
                            0x000e,
                        ],
                    ),
                ],
            );
            let descriptor = class_fixture(
                DESC,
                OBJECT,
                fixture_pool(),
                vec![
                    make(
                        DESC,
                        "getSerialName",
                        &[],
                        STRING,
                        1,
                        vec![0x0054, 2, 0x0011],
                    ),
                    make(
                        DESC,
                        "getElementsCount",
                        &[],
                        "I",
                        1,
                        vec![0x0052, 3, 0x000f],
                    ),
                    make(
                        DESC,
                        "getElementName",
                        &["I"],
                        STRING,
                        3,
                        vec![0x1054, 5, 0x0246, 0x0200, 0x0211],
                    ),
                    make(
                        DESC,
                        "<init>",
                        &[STRING, OBJECT, "I"],
                        "V",
                        5,
                        vec![
                            0x125b, 2, 0x1459, 3, 0xf012, 0x1059, 4, 0x4023, 5, 0x105b, 5, 0x4023,
                            6, 0x105b, 6, 0x000e,
                        ],
                    ),
                    make(
                        DESC,
                        "addElement",
                        &[STRING, "Z"],
                        "V",
                        6,
                        vec![
                            0x3052, 4, 0x00d8, 0x0100, 0x3059, 4, 0x3154, 5, 0x044d, 0x0001,
                            0x3154, 6, 0x054e, 0x0001, 0x000e,
                        ],
                    ),
                ],
            );
            NativeEngine::from_test_classes(BTreeMap::from([
                (type_name(ROUTE), route),
                (type_name(SER), implementation),
                (type_name(DESC), descriptor),
            ]))
        };
        let c = AtomicBool::new(false);
        let paired = engine(2, false);
        let route = paired.dex_class("fixture.Route").unwrap();
        let result = generated_serializer(&paired, route, &c).unwrap();
        assert_eq!(result.serial_name, "test.SerialAlias");
        assert_eq!(
            result
                .arguments
                .iter()
                .map(|(name, optional, _)| (*name, *optional))
                .collect::<Vec<_>>(),
            [("url", false), ("sizing", true)]
        );
        let original = destination_id(
            result.serial_name,
            result.arguments.iter().map(|(name, _, _)| *name),
        );

        let changed_count = engine(1, false);
        let route = changed_count.dex_class("fixture.Route").unwrap();
        assert!(generated_serializer(&changed_count, route, &c).is_none());
        let changed_order = engine(2, true);
        let route = changed_order.dex_class("fixture.Route").unwrap();
        let result = generated_serializer(&changed_order, route, &c).unwrap();
        assert_ne!(
            original,
            destination_id(
                result.serial_name,
                result.arguments.iter().map(|(name, _, _)| *name)
            )
        );
    }

    #[test]
    fn singleton_factory_selects_the_initializer_for_this_route() {
        const OBJECT: &str = "Ljava/lang/Object;";
        const ROUTE: &str = "Lfixture/SingletonRoute;";
        const OTHER: &str = "Lfixture/OtherRoute;";
        const THUNK: &str = "Lfixture/Dispatch;";
        let pool = || DexSymbols {
            types: [ROUTE, OTHER, THUNK, "I"]
                .into_iter()
                .map(Arc::from)
                .collect(),
            strings: ["initialize", "<init>", "variant"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            fields: vec![(2, 3, 2)],
            methods: vec![(0, 0, 0), (1, 0, 0), (2, 1, 1)],
            protos: vec![
                (Arc::from(SERIALIZER), Vec::new()),
                (Arc::from("V"), vec![Arc::from("I")]),
            ],
            ..Default::default()
        };
        let mut thunk = class_fixture(
            THUNK,
            OBJECT,
            pool(),
            vec![
                method_fixture(
                    THUNK,
                    "<init>",
                    &["I"],
                    "V",
                    false,
                    2,
                    vec![0x0159, 0, 0x000e],
                ),
                method_fixture(
                    THUNK,
                    "invoke",
                    &[],
                    OBJECT,
                    false,
                    2,
                    vec![
                        0x1052, 0, 0x002b, 14, 0, // variant field and packed-switch
                        0x0071, 1, 0, 0x000c, 0x0011, // default: unrelated initializer
                        0x0071, 0, 0, 0x000c, 0x0011, // variant 0: route initializer
                        0x0000, 0x0100, 1, 0, 0, 8, 0,
                    ],
                ),
            ],
        );
        thunk.access_flags = 0x11;
        thunk.fields.push(crate::native_dex::DexField {
            declaring_type: Arc::from(THUNK),
            name: Arc::from("variant"),
            field_type: Arc::from("I"),
            access_flags: 0x11,
            is_static: false,
        });
        let engine = NativeEngine::from_test_classes(BTreeMap::from([(type_name(THUNK), thunk)]));
        let parent = class_fixture(ROUTE, OBJECT, pool(), Vec::new());
        let cancel = AtomicBool::new(false);
        let resolve = |variant: u16| {
            let method = method_fixture(
                ROUTE,
                "<clinit>",
                &[],
                "V",
                true,
                2,
                vec![
                    0x0022,
                    2,
                    0x0112 | (variant << 12),
                    0x2070,
                    2,
                    0x0010,
                    0x000e,
                ],
            );
            let flow = Flow::new(&parent, &method, None, &cancel).unwrap();
            let closure =
                captured_function(&engine, &flow, Atom::Allocation(THUNK, 0), 6, &[], &cancel)
                    .unwrap();
            selected_serializer_initializer(&closure, ROUTE).map(|(_, call)| call.id())
        };
        assert_eq!(
            resolve(0).as_deref(),
            Some("fixture.SingletonRoute.initialize()Lkotlinx/serialization/KSerializer;")
        );
        assert!(resolve(1).is_none());
    }

    #[test]
    fn retained_evidence_enforces_exact_serialized_byte_and_item_budgets() {
        let mut bytes = RetentionBudget::new(10, 6, 4);
        assert!(!bytes.retain(&json!("large")).unwrap());
        assert!(bytes.retain(&json!("é")).unwrap()); // JSON quotes and two UTF-8 bytes.
        assert!(!bytes.retain(&json!("x")).unwrap());
        assert!(bytes.retain(&json!(0)).unwrap());
        assert_eq!((bytes.items, bytes.bytes), (2, 5));

        let mut items = RetentionBudget::new(1, 100, 20);
        assert!(items.retain(&json!(["graph", 1])).unwrap());
        assert!(!items.retain(&Value::Null).unwrap());
        assert_eq!(items.items, 1);
    }
}
