//! Optional, project-scoped OkHttp declaration maps with bounded DEX proof.
use crate::{
    engine::{NativeEngine, Project},
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::{DexClass, DexMethod},
    native_engine::disassembly::{method_id, type_name},
    native_ir::{DecodedMethod, Instruction},
    retained_budget::Reservation,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const REQUEST: &str = "Lokhttp3/Request;";
const RESPONSE: &str = "Lokhttp3/Response;";
const BUILDER: &str = "Lokhttp3/Request$Builder;";
const CHAIN: &str = "Lokhttp3/Interceptor$Chain;";
const INTERCEPTOR: &str = "Lokhttp3/Interceptor;";
const URL: &str = "Lokhttp3/HttpUrl;";
const HEADERS: &str = "Lokhttp3/Headers;";
const STRING: &str = "Ljava/lang/String;";
const STRING_ARRAY: &str = "[Ljava/lang/String;";
const JAVA_URL: &str = "Ljava/net/URL;";
const HEADER_ARRAY_GETTER: &str = "okhttp3.Headers.getNamesAndValues$okhttp()[Ljava/lang/String;";
const ARRAY_GET: &str =
    "kotlin.collections.ArraysKt.getOrNull([Ljava/lang/Object;I)Ljava/lang/Object;";
const MAX_BODY: usize = 512;
const MAX_ROWS: usize = 100;
const BYTE_LIMIT: usize = 512 * 1024;

#[derive(Clone, Default)]
pub(crate) struct Cache {
    inventory: Arc<Mutex<Option<Arc<Inventory>>>>,
}
struct Inventory {
    rows: Vec<Value>,
    structural_coverage: Value,
    complete: bool,
    _retention: Option<Arc<Reservation>>,
}

fn check(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    Ok(())
}

fn signature(method: &DexMethod, parameters: &[&str], ret: &str) -> bool {
    method.access_flags & (2 | 8) == 0
        && method.return_type.as_ref() == ret
        && method
            .parameters
            .iter()
            .map(AsRef::as_ref)
            .eq(parameters.iter().copied())
}

fn invoke(class: &DexClass, instruction: &Instruction) -> Option<String> {
    crate::native_engine::disassembly::symbol(
        &class.symbols,
        'm',
        instruction.reference?.index as usize,
    )
}

fn field(class: &DexClass, index: u32) -> Option<(&str, &str, &str)> {
    let (owner, ty, name) = *class.symbols.fields.get(index as usize)?;
    Some((
        class.symbols.types.get(owner as usize)?.as_ref(),
        class.symbols.types.get(ty as usize)?.as_ref(),
        class.symbols.strings.get(name as usize)?.as_str(),
    ))
}

fn field_id(class: &DexClass, index: u32) -> Option<String> {
    let (owner, ty, name) = field(class, index)?;
    Some(format!("{}.{name}:{ty}", type_name(owner)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueOrigin {
    Unknown,
    This,
    Parameter(usize),
    Field(u32, usize),
    ParameterField(u32),
    String(u32),
    New(usize),
    Initialized(usize),
    Invoke(usize),
    HeaderReceiver,
    Array,
    Index(i32, i32),
    Entry(i32),
    Null,
}
#[derive(Clone, Copy)]
enum Returned<'a> {
    Field,
    Constructed(&'a str),
    Invoke(&'a str),
    HeaderIndex,
    ArrayGet,
}

fn read(instruction: &Instruction, state: &[ValueOrigin], index: usize) -> ValueOrigin {
    instruction
        .reads
        .get(index)
        .map_or(ValueOrigin::Unknown, |arg| state[arg.register as usize])
}

fn entry_state(method: &DexMethod) -> Option<Vec<ValueOrigin>> {
    let code = method.code.as_ref()?;
    let is_static = method.access_flags & 8 != 0;
    let mut slot = code.registers.checked_sub(code.ins)? as usize;
    let mut state = vec![ValueOrigin::Unknown; code.registers as usize + 1];
    if !is_static {
        *state.get_mut(slot)? = ValueOrigin::This;
        slot += 1;
    }
    for (index, parameter) in method.parameters.iter().enumerate() {
        *state.get_mut(slot)? = ValueOrigin::Parameter(index);
        slot += if matches!(parameter.as_ref(), "J" | "D") {
            2
        } else {
            1
        };
    }
    (slot == code.registers as usize).then_some(state)
}

fn transfer(
    class: &DexClass,
    instruction: &Instruction,
    state: &mut [ValueOrigin],
    proof: Returned<'_>,
) -> Option<()> {
    let first = read(instruction, state, 0);
    let pending = *state.last()?;
    let next_result = if matches!(instruction.opcode, 0x6e..=0x72 | 0x74..=0x78) {
        match (proof, invoke(class, instruction).as_deref()) {
            (Returned::Invoke(expected), Some(actual)) if expected == actual => {
                ValueOrigin::Invoke(instruction.pc)
            }
            (Returned::HeaderIndex, Some(HEADER_ARRAY_GETTER))
                if matches!(instruction.opcode, 0x6e | 0x74)
                    && first == ValueOrigin::HeaderReceiver
                    && instruction.reads.len() == 1 =>
            {
                ValueOrigin::Array
            }
            (Returned::HeaderIndex, Some(ARRAY_GET))
                if matches!(instruction.opcode, 0x71 | 0x77)
                    && first == ValueOrigin::Array
                    && instruction.reads.len() == 2 =>
            {
                match read(instruction, state, 1) {
                    ValueOrigin::Index(2, shift @ (0 | 1)) => ValueOrigin::Entry(shift),
                    _ => ValueOrigin::Unknown,
                }
            }
            _ => ValueOrigin::Unknown,
        }
    } else {
        ValueOrigin::Unknown
    };
    *state.last_mut()? = next_result;
    let array_proof = matches!(proof, Returned::HeaderIndex | Returned::ArrayGet);
    let origin = match instruction.opcode {
        0x12..=0x15 if array_proof && instruction.literal == Some(0) => ValueOrigin::Null,
        0x46 if matches!(proof, Returned::ArrayGet)
            && first == ValueOrigin::Array
            && read(instruction, state, 1) == ValueOrigin::Index(1, 0) =>
        {
            ValueOrigin::Entry(0)
        }
        0xd0 | 0xd2 | 0xd8 | 0xda | 0xe0 if matches!(proof, Returned::HeaderIndex) => {
            let ValueOrigin::Index(scale, shift) = first else {
                return None;
            };
            let value = i32::try_from(instruction.literal?).ok()?;
            if matches!(instruction.opcode, 0xd0 | 0xd8) {
                ValueOrigin::Index(scale, shift.checked_add(value)?)
            } else {
                let factor = if instruction.opcode == 0xe0 {
                    1i32.checked_shl((value & 31) as u32)?
                } else {
                    value
                };
                ValueOrigin::Index(scale.checked_mul(factor)?, shift.checked_mul(factor)?)
            }
        }
        0x01..=0x03 | 0x07..=0x09 | 0x1f => first,
        0x1a | 0x1b => ValueOrigin::String(instruction.reference?.index),
        0x54 if first == ValueOrigin::This => {
            let index = instruction.reference?.index;
            let (owner, _, _) = field(class, index)?;
            if owner == class.descriptor.as_ref() {
                ValueOrigin::Field(index, instruction.pc)
            } else {
                ValueOrigin::Unknown
            }
        }
        0x54 if first == ValueOrigin::Parameter(0) => {
            ValueOrigin::ParameterField(instruction.reference?.index)
        }
        0x22 => {
            let ty = class
                .symbols
                .types
                .get(instruction.reference?.index as usize)?;
            if let Returned::Constructed(expected) = proof
                && ty.as_ref() == expected
            {
                ValueOrigin::New(instruction.pc)
            } else {
                ValueOrigin::Unknown
            }
        }
        0x70 | 0x76 => {
            if let (Returned::Constructed(expected), ValueOrigin::New(allocation)) = (proof, first)
            {
                let target = invoke(class, instruction)?;
                let owner = type_name(expected);
                if target
                    .strip_prefix(&owner)
                    .is_some_and(|name| name.starts_with(".<init>(") && name.ends_with(")V"))
                {
                    for slot in state.iter_mut() {
                        if *slot == ValueOrigin::New(allocation) {
                            *slot = ValueOrigin::Initialized(allocation);
                        }
                    }
                }
            }
            ValueOrigin::Unknown
        }
        0x0c if matches!(proof, Returned::Invoke(_) | Returned::HeaderIndex) => pending,
        _ => ValueOrigin::Unknown,
    };
    for write in &instruction.writes {
        for word in 0..write.kind.word_count() {
            *state.get_mut(write.register as usize + word)? = origin;
        }
    }
    Some(())
}

// A small forward provenance proof. Every reachable normal return must carry
// the requested origin. Exception handlers start with unknown locals.
fn returned(
    class: &DexClass,
    method: &DexMethod,
    proof: Returned<'_>,
) -> Option<Vec<(usize, ValueOrigin)>> {
    let code = method.code.as_ref()?;
    if code.instructions.len() > MAX_BODY || code.registers > 64 {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    let mut states = vec![None; cfg.blocks.len()];
    let entry = *cfg.block_at.get(&0)?;
    let mut initial = entry_state(method)?;
    if matches!(proof, Returned::HeaderIndex | Returned::ArrayGet) {
        if method.access_flags & 8 == 0 || code.ins != 2 {
            return None;
        }
        let first = code.registers.checked_sub(2)? as usize;
        initial[first] = if matches!(proof, Returned::HeaderIndex) {
            ValueOrigin::HeaderReceiver
        } else {
            ValueOrigin::Array
        };
        initial[first + 1] = ValueOrigin::Index(1, 0);
    }
    states[entry] = Some(initial);
    let by_pc: BTreeMap<_, _> = ir
        .instructions
        .iter()
        .enumerate()
        .map(|(index, i)| (i.pc, index))
        .collect();
    let mut returns = Vec::new();
    let mut order: Vec<_> = (0..cfg.blocks.len()).collect();
    order.sort_unstable_by_key(|&index| cfg.blocks[index].start);
    for index in order {
        let block = &cfg.blocks[index];
        let Some(mut state) = states[index].take() else {
            continue;
        };
        for &pc in &block.instructions {
            let slot = by_pc[&pc];
            let instruction = &ir.instructions[slot];
            if instruction.opcode == 0x11 {
                let origin = read(instruction, &state, 0);
                let valid = matches!(
                    (proof, origin),
                    (Returned::Field, ValueOrigin::Field(_, _))
                        | (Returned::Constructed(_), ValueOrigin::Initialized(_))
                        | (Returned::Invoke(_), ValueOrigin::Invoke(_))
                        | (Returned::HeaderIndex, ValueOrigin::Entry(0 | 1))
                        | (
                            Returned::ArrayGet,
                            ValueOrigin::Entry(0) | ValueOrigin::Null
                        )
                );
                if !valid || returns.len() >= 16 {
                    return None;
                }
                returns.push((pc, origin));
            } else if matches!(instruction.opcode, 0x0e..=0x10)
                || (matches!(proof, Returned::Field)
                    && !matches!(instruction.opcode, 0x00 | 0x07..=0x09 | 0x1f | 0x54))
            {
                return None;
            }
            transfer(class, instruction, &mut state, proof)?;
        }
        for edge in &block.successors {
            if cfg.blocks[edge.target].start <= block.start {
                return None;
            }
            let state_on_edge = if edge.kind == EdgeKind::Exceptional {
                vec![ValueOrigin::Unknown; state.len()]
            } else {
                state.clone()
            };
            if let Some(prior) = &mut states[edge.target] {
                for (old, new) in prior.iter_mut().zip(state_on_edge) {
                    if *old != new {
                        *old = match (*old, new) {
                            (ValueOrigin::Entry(index), ValueOrigin::Null)
                            | (ValueOrigin::Null, ValueOrigin::Entry(index))
                                if matches!(proof, Returned::ArrayGet) =>
                            {
                                ValueOrigin::Entry(index)
                            }
                            _ => ValueOrigin::Unknown,
                        };
                    }
                }
            } else {
                states[edge.target] = Some(state_on_edge);
            }
        }
    }
    (!returns.is_empty()).then_some(returns)
}

fn role(mut candidates: Vec<Value>) -> Value {
    match candidates.len() {
        0 => json!({"status":"unresolved","reason":"no proven candidate"}),
        1 => {
            let mut candidate = candidates.remove(0);
            candidate["status"] = json!("resolved");
            candidate
        }
        _ => json!({"status":"ambiguous","candidate_count":candidates.len(),
            "candidates_truncated":candidates.len()>8,"candidates":candidates.iter().take(8).collect::<Vec<_>>()}),
    }
}

fn bounded_roles<'a>(
    methods: impl Iterator<Item = &'a DexMethod>,
    limit: usize,
    mut prove: impl FnMut(&'a DexMethod) -> Option<Value>,
) -> Value {
    let mut proved = Vec::new();
    for (index, method) in methods.take(limit + 1).enumerate() {
        if index == limit {
            return json!({"status":"unresolved","reason":"method candidate limit reached",
                "examined_candidates":limit,"proven_candidate_count_so_far":proved.len()});
        }
        if let Some(evidence) = prove(method) {
            proved.push(evidence);
        }
    }
    role(proved)
}

fn field_getters(
    class: &DexClass,
    ty: &str,
    mut include: impl FnMut(&DexMethod, u32) -> bool,
) -> Value {
    let mut candidates = Vec::new();
    for (candidate, method) in class
        .methods
        .iter()
        .filter(|method| signature(method, &[], ty))
        .take(65)
        .enumerate()
    {
        if candidate == 64 {
            return json!({"status":"unresolved","reason":"method candidate limit reached"});
        }
        let Some((index, evidence)) = field_getter(class, method, ty) else {
            continue;
        };
        if include(method, index) {
            candidates.push(evidence);
        }
    }
    role(candidates)
}

fn field_getter(class: &DexClass, method: &DexMethod, ty: &str) -> Option<(u32, Value)> {
    let proofs = returned(class, method, Returned::Field)?;
    let ValueOrigin::Field(index, _) = proofs.first()?.1 else {
        return None;
    };
    if !proofs
        .iter()
        .all(|(_, value)| matches!(value, ValueOrigin::Field(other, _) if *other == index))
    {
        return None;
    }
    let (owner, field_type, name) = field(class, index)?;
    if owner != class.descriptor.as_ref()
        || field_type != ty
        || !class
            .fields
            .iter()
            .any(|f| !f.is_static && f.name.as_ref() == name && f.field_type.as_ref() == ty)
    {
        return None;
    }
    Some((
        index,
        json!({"method_id":method_id(method),"field_id":field_id(class,index),
        "evidence":{"returned_own_typed_field":true,"return_offsets":proofs.iter().map(|(pc,_)|pc).collect::<Vec<_>>()}}),
    ))
}

fn preferred_role<'a>(
    class: &'a DexClass,
    name: &str,
    params: &[&str],
    ret: &str,
    mut prove: impl FnMut(&'a DexMethod) -> Option<Value>,
) -> Option<Value> {
    let mut methods = class.methods.iter().filter(|m| {
        m.access_flags & 1 != 0 && m.name.as_ref() == name && signature(m, params, ret)
    });
    let method = methods.next()?;
    if methods.next().is_some() {
        return Some(json!({"status":"unresolved","reason":"duplicate preserved named contract"}));
    }
    Some(if let Some(mut evidence) = prove(method) {
        evidence["confidence"] = json!("named_contract_and_dex");
        role(vec![evidence])
    } else {
        json!({"status":"unresolved","method_id":method_id(method),"reason":"named contract has no supported DEX proof"})
    })
}

fn named_field_getter(class: &DexClass, name: &str, ty: &str) -> Option<Value> {
    preferred_role(class, name, &[], ty, |method| {
        field_getter(class, method, ty).map(|(_, proof)| proof)
    })
}

fn linear_writes(class: &DexClass, method: &DexMethod) -> Option<Vec<(u32, ValueOrigin)>> {
    let code = method.code.as_ref()?;
    if code.instructions.len() > MAX_BODY || code.registers > 64 || !code.try_regions.is_empty() {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let mut state = entry_state(method)?;
    let mut writes = Vec::new();
    for instruction in &ir.instructions {
        if matches!(instruction.opcode, 0x26..=0x3d) {
            return None;
        }
        // Typed IR orders instance-field reads as receiver then stored value.
        if instruction.opcode == 0x5b && read(instruction, &state, 0) == ValueOrigin::This {
            writes.push((instruction.reference?.index, read(instruction, &state, 1)));
        }
        transfer(class, instruction, &mut state, Returned::Field)?;
    }
    Some(writes)
}

fn request_method_fields(request: &DexClass, builder: &DexClass) -> Option<Vec<u32>> {
    let mut defaults = Vec::new();
    for (candidate, method) in builder
        .methods
        .iter()
        .filter(|m| m.name.as_ref() == "<init>" && m.parameters.is_empty())
        .take(9)
        .enumerate()
    {
        if candidate == 8 {
            return None;
        }
        for (index, value) in linear_writes(builder, method).into_iter().flatten() {
            if let ValueOrigin::String(string) = value
                && builder
                    .symbols
                    .strings
                    .get(string as usize)
                    .is_some_and(|text| matches!(text.as_str(), "GET" | "POST" | "HEAD"))
                && field(builder, index)
                    .is_some_and(|(owner, ty, _)| owner == BUILDER && ty == STRING)
            {
                defaults.push(index);
            }
        }
    }
    let mut seeds = Vec::new();
    for (candidate, method) in builder
        .methods
        .iter()
        .filter(|m| {
            [BUILDER, "V"]
                .iter()
                .any(|ret| signature(m, &[STRING, "Lokhttp3/RequestBody;"], ret))
        })
        .take(17)
        .enumerate()
    {
        if candidate == 16 {
            return None;
        }
        for (index, value) in linear_writes(builder, method).into_iter().flatten() {
            if value == ValueOrigin::Parameter(0)
                && defaults.contains(&index)
                && let Some(seed) = field_id(builder, index)
            {
                seeds.push(seed);
            }
        }
    }
    let mut roles = Vec::new();
    for (candidate, method) in request
        .methods
        .iter()
        .filter(|m| {
            m.name.as_ref() == "<init>" && m.parameters.iter().map(AsRef::as_ref).eq([BUILDER])
        })
        .take(9)
        .enumerate()
    {
        if candidate == 8 {
            return None;
        }
        for (index, value) in linear_writes(request, method).into_iter().flatten() {
            if let ValueOrigin::ParameterField(source) = value
                && field(request, index)
                    .is_some_and(|(owner, ty, _)| owner == REQUEST && ty == STRING)
                && field_id(request, source).is_some_and(|source| seeds.contains(&source))
            {
                roles.push(index);
            }
        }
    }
    roles.sort_unstable();
    roles.dedup();
    Some(roles)
}

fn proof_row(method: &DexMethod, evidence: Vec<(usize, ValueOrigin)>) -> Value {
    json!({"method_id":method_id(method),"evidence":{"return_offsets":evidence.iter().map(|(pc,_)|pc).collect::<Vec<_>>(),
        "producer_offsets":evidence.iter().filter_map(|(_,value)| match value {
            ValueOrigin::Initialized(pc) | ValueOrigin::Invoke(pc) => Some(pc),
            _ => None,
        }).collect::<Vec<_>>()}})
}

fn loaded_declaration<'a>(
    engine: &'a NativeEngine,
    id: &str,
    inherit: bool,
) -> Option<(&'a DexClass, &'a DexMethod)> {
    let mut owner = crate::method_lookup::owner(id).ok()?.to_owned();
    let member = id.get(owner.len() + 1..)?;
    for _ in 0..32 {
        let class = engine.dex_class(&owner)?;
        let exact = format!("{owner}.{member}");
        let mut declarations = class
            .methods
            .iter()
            .filter(|method| method_id(method) == exact);
        if let Some(method) = declarations.next() {
            return declarations.next().is_none().then_some((class, method));
        }
        if !inherit {
            return None;
        }
        owner = type_name(class.superclass.as_deref()?);
    }
    None
}

fn indexed_header_delegate(headers: &DexClass, method: &DexMethod, target: &str) -> Option<usize> {
    let code = method.code.as_ref()?;
    if code.instructions.len() > 16 || !code.try_regions.is_empty() || code.ins != 2 {
        return None;
    }
    let decoded = DecodedMethod::decode(code).ok()?;
    let [invoke_site, result, returned] = decoded.instructions.as_slice() else {
        return None;
    };
    let start = code.registers.checked_sub(2)?;
    let [receiver, index] = invoke_site.reads.as_slice() else {
        return None;
    };
    let [value] = result.writes.as_slice() else {
        return None;
    };
    let [output] = returned.reads.as_slice() else {
        return None;
    };
    (matches!(invoke_site.opcode, 0x71 | 0x77)
        && invoke(headers, invoke_site).as_deref() == Some(target)
        && receiver.register == start
        && index.register == start + 1
        && result.opcode == 0x0c
        && returned.opcode == 0x11
        && value.register == output.register)
        .then_some(invoke_site.pc)
}

fn named_header_accessors(
    engine: &NativeEngine,
    headers: &DexClass,
    pair_name: &str,
) -> Option<Value> {
    const GET_NAME: &str = "okhttp3.Headers.name(I)Ljava/lang/String;";
    const GET_VALUE: &str = "okhttp3.Headers.value(I)Ljava/lang/String;";
    const COMMON_NAME: &str =
        "okhttp3.internal._HeadersCommonKt.commonName(Lokhttp3/Headers;I)Ljava/lang/String;";
    const COMMON_VALUE: &str =
        "okhttp3.internal._HeadersCommonKt.commonValue(Lokhttp3/Headers;I)Ljava/lang/String;";
    let (_, name) = loaded_declaration(engine, GET_NAME, false)?;
    let (_, value) = loaded_declaration(engine, GET_VALUE, false)?;
    if [name, value]
        .iter()
        .any(|m| m.access_flags & 1 == 0 || !signature(m, &["I"], STRING))
    {
        return None;
    }
    let mut evidence = json!({"storage_field":format!("okhttp3.Headers.{pair_name}:{STRING_ARRAY}"),"name_index":"2*i","value_index":"2*i+1"});
    if crate::network_stacks::entry_accessor(headers, name) != Some(0)
        || crate::network_stacks::entry_accessor(headers, value) != Some(1)
    {
        let (_, getter) = loaded_declaration(engine, HEADER_ARRAY_GETTER, false)?;
        let (storage, _) = field_getter(headers, getter, STRING_ARRAY)?;
        if field(headers, storage)?.2 != pair_name || !signature(getter, &[], STRING_ARRAY) {
            return None;
        }
        let (array_class, array_method) = loaded_declaration(engine, ARRAY_GET, true)?;
        // The Kotlin facade may inherit a static declaration. Every non-null
        // successful return must be the original array at the original index.
        let array_returns = returned(array_class, array_method, Returned::ArrayGet)?;
        if !array_returns
            .iter()
            .any(|(_, origin)| *origin == ValueOrigin::Entry(0))
        {
            return None;
        }
        let mut delegates = Vec::new();
        for (method, target, slot) in [(name, COMMON_NAME, 0), (value, COMMON_VALUE, 1)] {
            let call = indexed_header_delegate(headers, method, target)?;
            let (class, helper) = loaded_declaration(engine, target, false)?;
            let returns = returned(class, helper, Returned::HeaderIndex)?;
            if !returns
                .iter()
                .all(|(_, origin)| *origin == ValueOrigin::Entry(slot))
            {
                return None;
            }
            delegates.push(
                json!({"helper_method_id":method_id(helper),"call_site_offset":call,
                "return_offsets":returns.iter().map(|(pc,_)|pc).collect::<Vec<_>>() }),
            );
        }
        evidence["array_lookup_declaring_method_id"] = json!(method_id(array_method));
        evidence["delegates"] = json!(delegates);
    }
    Some(
        json!({"status":"resolved","method_id":GET_NAME,"confidence":"named_contract_and_dex",
        "paired_storage_field_name":pair_name,"paired_storage_field_id":evidence["storage_field"],
        "evidence":evidence}),
    )
}

fn named(engine: &NativeEngine, cancel: &AtomicBool) -> Result<Option<Value>> {
    let Some(request) = engine.dex_class("okhttp3.Request") else {
        return Ok(None);
    };
    let builder = engine.dex_class("okhttp3.Request$Builder");
    let http_url = engine.dex_class("okhttp3.HttpUrl");
    let headers = engine.dex_class("okhttp3.Headers");
    let contract = engine.dex_class(&type_name(INTERCEPTOR));
    let chain = engine.dex_class("okhttp3.Interceptor$Chain");
    let intercept = bounded_roles(
        contract
            .into_iter()
            .flat_map(|class| class.methods.iter())
            .filter(|method| signature(method, &[CHAIN], RESPONSE)),
        16,
        |method| Some(json!({"method_id":method_id(method)})),
    );
    let mut chain_roles = Vec::new();
    let mut unresolved_contracts = 0;
    if let Some(intercept_id) = intercept["method_id"].as_str() {
        for (candidate, method) in chain
            .into_iter()
            .flat_map(|class| class.methods.iter())
            .filter(|method| signature(method, &[REQUEST], RESPONSE))
            .take(9)
            .enumerate()
        {
            check(cancel)?;
            if candidate == 8 {
                unresolved_contracts += 1;
                break;
            }
            let id = method_id(method);
            let targets = engine.hook_targets(&json!({"method_id":id,"limit":100}), cancel)?;
            if targets["coverage"]["scan_complete"] != true || !targets["next_offset"].is_null() {
                unresolved_contracts += 1;
            }
            let mut implementations = Vec::new();
            for target in targets["items"]
                .as_array()
                .context("Missing implementation targets")?
            {
                let Some(id) = target["installation_method_id"].as_str() else {
                    continue;
                };
                let Some(class) = engine.dex_class(crate::method_lookup::owner(id)?) else {
                    continue;
                };
                let implementation = crate::method_lookup::resolve(class, id)?;
                if let Some(evidence) =
                    returned(class, implementation, Returned::Invoke(intercept_id))
                {
                    let mut row = proof_row(implementation, evidence);
                    row["possible_receiver_count"] = target["possible_receiver_count"].clone();
                    row["bridge"] = target["bridge"].clone();
                    implementations.push(row);
                }
            }
            if !implementations.is_empty() {
                chain_roles.push(
                    json!({"method_id":id,"implementation_count":implementations.len(),
                    "implementations_truncated":implementations.len()>8,
                    "implementations":implementations.iter().take(8).collect::<Vec<_>>()}),
                );
            }
        }
    }
    let build = builder.map_or_else(
        || role(vec![]),
        |builder| {
            let prove = |method| {
                Some(proof_row(
                    method,
                    returned(builder, method, Returned::Constructed(REQUEST))?,
                ))
            };
            preferred_role(builder, "build", &[], REQUEST, prove).unwrap_or_else(|| {
                bounded_roles(
                    builder
                        .methods
                        .iter()
                        .filter(|method| signature(method, &[], REQUEST)),
                    64,
                    prove,
                )
            })
        },
    );
    let url_getter = named_field_getter(request, "url", URL)
        .unwrap_or_else(|| field_getters(request, URL, |_, _| true));
    let method_getter = named_field_getter(request,"method",STRING).unwrap_or_else(|| {
        let method_fields = builder.map_or_else(|| Some(Vec::new()), |builder| request_method_fields(request, builder));
        if let Some(fields) = method_fields {
            field_getters(request, STRING, |_, index| fields.contains(&index))
        } else {
            json!({"status":"unresolved","reason":"method field provenance candidate limit reached"})
        }
    });
    let headers_getter = named_field_getter(request, "headers", HEADERS)
        .unwrap_or_else(|| field_getters(request, HEADERS, |_, _| true));
    let url_setter = builder
        .and_then(|class| {
            preferred_role(class, "url", &[STRING], BUILDER, |method| {
                const TYPED: &str =
                    "okhttp3.Request$Builder.url(Lokhttp3/HttpUrl;)Lokhttp3/Request$Builder;";
                let mut proof =
                    proof_row(method, returned(class, method, Returned::Invoke(TYPED))?);
                proof["delegating_method_id"] = json!(TYPED);
                Some(proof)
            })
        })
        .unwrap_or_else(|| role(vec![]));
    let conversion = http_url.map_or_else(
        || role(vec![]),
        |class| {
            let prove = |method| {
                Some(proof_row(
                    method,
                    returned(class, method, Returned::Constructed(JAVA_URL))?,
                ))
            };
            preferred_role(class, "url", &[], JAVA_URL, prove).unwrap_or_else(|| {
                bounded_roles(
                    class
                        .methods
                        .iter()
                        .filter(|method| signature(method, &[], JAVA_URL)),
                    64,
                    prove,
                )
            })
        },
    );
    let names = headers.map_or_else(|| role(vec![]), |class| {
        let mut storage = class.fields.iter().filter(|field| {
            !field.is_static && field.field_type.as_ref() == "[Ljava/lang/String;"
        });
        let Some(pairs) = storage.next() else { return role(vec![]) };
        if storage.next().is_some() {
            return json!({"status":"unresolved","reason":"paired header storage is ambiguous"});
        }
        if let Some(role) = named_header_accessors(engine, class, &pairs.name) {
            return role;
        }
        let candidates: Vec<_> = class.methods.iter().filter(|method| signature(method,&["I"],STRING)).take(65).collect();
        if candidates.len() > 64 {
            return json!({"status":"unresolved","reason":"header indexing candidate limit reached"});
        }
        let indices: Vec<_> = candidates.iter().map(|method| crate::network_stacks::entry_accessor(class, method)).collect();
        if !indices.contains(&Some(1)) {
            return role(vec![]);
        }
        role(candidates.into_iter().zip(indices).filter(|(_, index)| *index == Some(0))
            .map(|(method, _)| json!({"method_id":method_id(method),"paired_storage_field_name":pairs.name.as_ref(),
                "paired_storage_field_id":format!("okhttp3.Headers.{}:{}",pairs.name,pairs.field_type),
                "evidence":"paired String array index 2*i; pair count derived from this exact array length"})).collect())
    });
    let proceed = if unresolved_contracts == 0 {
        role(chain_roles)
    } else {
        json!({"status":"unresolved","reason":"incomplete candidate or implementor coverage","proven_candidates":chain_roles})
    };
    let request_source = if proceed["status"] == "resolved" {
        json!({"status":"resolved","method_id":proceed["method_id"],"argument_index":0})
    } else {
        role(vec![])
    };
    Ok(Some(
        json!({"kind":"hook_map","family":"okhttp-compatible","class_id":"okhttp3.Request","recognition":["named_okhttp3"],
        "roles":{"interceptor_contract":intercept,"interceptor_implementation":role(vec![]),
            "chain_proceed":proceed,"request_source":request_source,
            "request_builder":build,"request_url":url_getter,"request_url_setter":url_setter,
            "request_method":method_getter,"request_headers":headers_getter,
            "url_conversion":conversion,"header_name":names},"runtime_verified":false}),
    ))
}

fn observer(row: &Value) -> Option<Value> {
    let roles = &row["roles"];
    let proceed = &roles["chain_proceed"];
    if proceed["status"] != "resolved" || proceed["implementation_count"] != 1 {
        return None;
    }
    let [implementation] = proceed["implementations"].as_array()?.as_slice() else {
        return None;
    };
    let install = implementation["method_id"].as_str()?;
    let id = crate::method_lookup::owner(install).ok()?;
    let member = install[id.len() + 1..].split_once('(')?.0;
    let getter = |name: &str| -> Option<&str> {
        let role = &roles[name];
        if role["status"] != "resolved" {
            return None;
        }
        let id = role["method_id"].as_str()?;
        let owner = crate::method_lookup::owner(id).ok()?;
        Some(id[owner.len() + 1..].split_once('(')?.0)
    };
    let url = getter("request_url").zip(getter("url_conversion"));
    let method = getter("request_method");
    let headers = getter("request_headers")
        .zip(getter("header_name"))
        .zip(roles["header_name"]["paired_storage_field_name"].as_str())
        .map(|((request, name), pairs)| [request, name, pairs]);
    if url.is_none() && method.is_none() && headers.is_none() {
        return None;
    }
    let selectors = json!({"owner":id,"member":member,"request":"okhttp3.Request",
        "url":url,"method":method,"headers":headers});
    let omitted_roles = [
        ("origin", url.is_none()),
        ("http_method", method.is_none()),
        ("headers", headers.is_none()),
    ]
    .into_iter()
    .filter_map(|(name, omitted)| omitted.then_some(name))
    .collect::<Vec<_>>();
    Some(json!({"status":"template","installation_method_id":install,
        "omitted_roles":omitted_roles,
        "script":format!("const selectors = {selectors};\n{}",include_str!("network_observer.js"))}))
}

fn structural_builders(
    engine: &NativeEngine,
    project: &Project,
    rows: &[Value],
    cancel: &AtomicBool,
) -> Result<(BTreeMap<String, Vec<Value>>, bool)> {
    const METHOD_LIMIT: usize = 500_000;
    const PROOF_LIMIT: usize = 128;
    let mut proven: BTreeMap<String, Vec<Value>> = rows
        .iter()
        .filter_map(|row| {
            let class = engine.dex_class(row["roles"]["request"].as_str()?)?;
            Some((class.descriptor.to_string(), Vec::new()))
        })
        .collect();
    if proven.is_empty() {
        return Ok((proven, true));
    }
    let mut methods = 0;
    let mut proofs = 0;
    for class in project
        .classes
        .iter()
        .filter_map(|name| engine.dex_class(name))
    {
        check(cancel)?;
        for method in &class.methods {
            methods += 1;
            if methods > METHOD_LIMIT {
                return Ok((proven, false));
            }
            if method.parameters.is_empty()
                && method.access_flags & (2 | 8) == 0
                && let Some(results) = proven.get_mut(method.return_type.as_ref())
            {
                proofs += 1;
                if proofs > PROOF_LIMIT {
                    return Ok((proven, false));
                }
                if let Some(evidence) =
                    returned(class, method, Returned::Constructed(&method.return_type))
                {
                    results.push(proof_row(method, evidence));
                }
            }
        }
    }
    Ok((proven, true))
}

fn structural_row(item: &Value, builder: Value) -> Value {
    let map = &item["hook_map"];
    let method = |key: &str| {
        map[key].as_str().map_or_else(
            || role(Vec::new()),
            |id| json!({"status":"candidate","method_id":id}),
        )
    };
    let field = |key: &str| {
        map[key].as_str().map_or_else(
            || role(Vec::new()),
            |id| json!({"status":"candidate","field_id":id}),
        )
    };
    let class = map["interceptor_method_id"]
        .as_str()
        .and_then(|id| crate::method_lookup::owner(id).ok());
    let source = if map["request_from_chain_getter_id"].is_string() {
        method("request_from_chain_getter_id")
    } else {
        field("request_from_chain_field_id")
    };
    let mut names = method("header_name_method_id");
    if names["status"] == "candidate" {
        names["paired_storage_field_id"] = map["header_pairs_field_id"].clone();
        names["evidence"] = json!("bounded DEX indexing proof in a structural header model");
    }
    json!({"kind":"hook_map","family":"okhttp-compatible","class_id":class,
        "recognition":["structural_okhttp_compatible"],
        "roles":{"interceptor_contract":method("interceptor_contract_method_id"),
            "interceptor_implementation":method("interceptor_method_id"),
            "chain_proceed":method("chain_proceed_method_id"),"request_source":source,"request_builder":builder,
            "request_url":field("request_url_field_id"),"request_method":field("request_method_field_id"),
            "request_url_setter":role(vec![]),
            "request_headers":field("request_headers_field_id"),"url_conversion":role(vec![]),"header_name":names},
        "structural_evidence":{"classes":item["roles"],"selectors":map,"proof_notes":item["evidence"],
            "unresolved_roles":item["unresolved_roles"]},"runtime_verified":false})
}

impl Cache {
    fn get(
        &self,
        engine: &NativeEngine,
        project: &Project,
        cancel: &AtomicBool,
    ) -> Result<(Arc<Inventory>, bool)> {
        let mut slot = loop {
            check(cancel)?;
            match self.inventory.try_lock() {
                Ok(slot) => break slot,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
                Err(TryLockError::Poisoned(_)) => anyhow::bail!("Network hook cache unavailable"),
            }
        };
        if let Some(inventory) = slot.as_ref() {
            return Ok((inventory.clone(), true));
        }
        let structural = crate::network_stacks::structural_hook_inventory(engine, project, cancel)?;
        let mut rows = Vec::new();
        if let Some(named) = named(engine, cancel)? {
            rows.push(named);
        }
        let structural_rows = structural["items"]
            .as_array()
            .context("Missing structural maps")?;
        let (builders, builder_coverage) =
            structural_builders(engine, project, structural_rows, cancel)?;
        let mut clipped = false;
        for item in structural_rows.iter() {
            check(cancel)?;
            if rows.len() == MAX_ROWS {
                clipped = true;
                break;
            }
            let request = item["roles"]["request"]
                .as_str()
                .and_then(|name| engine.dex_class(name));
            let evidence = request.and_then(|class| builders.get(class.descriptor.as_ref()));
            let mut builder = if builder_coverage {
                role(evidence.cloned().unwrap_or_default())
            } else {
                json!({"status":"unresolved","reason":"request construction scan budget reached"})
            };
            if builder["status"] == "resolved" {
                builder["status"] = json!("candidate");
            }
            rows.push(structural_row(item, builder));
        }
        let bytes = serde_json::to_vec(&rows)?.len();
        ensure!(
            bytes <= 2 * 1024 * 1024,
            "Network hook inventory exceeds storage budget"
        );
        let complete = !clipped
            && builder_coverage
            && structural["coverage"]["complete"] == true
            && structural["next_offset"].is_null();
        let retention = engine
            .retained_budget
            .reserve(bytes.saturating_mul(4) + rows.len() * 512);
        let inventory = Arc::new(Inventory {
            rows,
            complete,
            structural_coverage: structural["coverage"].clone(),
            _retention: retention,
        });
        if inventory._retention.is_some() {
            *slot = Some(inventory.clone());
        }
        Ok((inventory, false))
    }

    pub(crate) fn search(
        &self,
        engine: &NativeEngine,
        project: &Project,
        args: &Value,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        let (inventory, cached) = self.get(engine, project, cancel)?;
        let scope = crate::package_scope::PackageScope::from_args(args)?;
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().unwrap_or(25) as usize;
        ensure!((1..=100).contains(&limit), "limit must be 1..100");
        let matching: Vec<_> = inventory
            .rows
            .iter()
            .filter(|row| row["class_id"].as_str().is_some_and(|id| scope.matches(id)))
            .collect();
        let outside = inventory.rows.len() - matching.len();
        let outside_named = inventory
            .rows
            .iter()
            .filter(|row| {
                row["recognition"] == json!(["named_okhttp3"])
                    && row["class_id"]
                        .as_str()
                        .is_some_and(|id| !scope.matches(id))
            })
            .count();
        ensure!(offset <= matching.len(), "Invalid hook map offset");
        let mut items = Vec::new();
        let mut bytes = 0;
        for row in matching.iter().skip(offset).take(limit) {
            check(cancel)?;
            let mut row = (**row).clone();
            if args["include_observer"] == true {
                row["observer"] = observer(&row).unwrap_or_else(|| json!({"status":"unresolved","reason":"No unique, proven install declaration and safe capture roles"}));
            }
            let size = serde_json::to_vec(&row)?.len();
            if bytes + size > BYTE_LIMIT {
                ensure!(!items.is_empty(), "Hook map record exceeds output budget");
                break;
            }
            bytes += size;
            items.push(row);
        }
        let next = offset + items.len();
        Ok(
            json!({"items":items,"offset":offset,"total":inventory.complete.then_some(matching.len()),
            "matching_candidates_so_far":matching.len(),"next_offset":(next < matching.len()).then_some(next),
            "projection":"hook_maps","cache_hit":cached,"cache_retained":inventory._retention.is_some(),
            "coverage":{"complete":inventory.complete,"retained_map_limit":MAX_ROWS,"package_scope":scope.describe(),
                "scope_basis":"map_declaration_owner","retained_maps_outside_prefix":outside,
                "retained_named_maps_outside_prefix":outside_named,"structural":inventory.structural_coverage},
            "runtime_values_included":false,
            "scope":"Exact named OkHttp APIs and conservative descriptor/DEX role proof, alongside structural candidates. Prefixes filter map declaration owners.",
            "scope_guidance":(!scope.is_unrestricted()).then_some("Omit package_prefix to see available library maps. Use projection=all or services for scoped network usage and service evidence.")}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexField, DexSymbols};

    fn method(
        owner: &str,
        name: &str,
        params: &[&str],
        ret: &str,
        body: Option<(u16, Vec<u16>)>,
    ) -> DexMethod {
        let ins = 1 + params.len() as u16;
        DexMethod {
            declaring_type: owner.into(),
            name: name.into(),
            return_type: ret.into(),
            parameters: params.iter().map(|p| (*p).into()).collect(),
            thrown_types: vec![],
            access_flags: if body.is_some() { 1 } else { 0x401 },
            code: body.map(|(registers, instructions)| DexCode {
                registers,
                ins,
                instructions,
                outs: 3,
                tries: 0,
                try_regions: vec![],
                offset: 32,
            }),
        }
    }

    fn class(
        ty: &str,
        superclass: &str,
        flags: u32,
        interfaces: &[&str],
        symbols: DexSymbols,
        fields: &[(&str, &str)],
        methods: Vec<DexMethod>,
    ) -> (String, DexClass) {
        (
            type_name(ty),
            DexClass {
                descriptor: ty.into(),
                superclass: Some(superclass.into()),
                access_flags: flags,
                interfaces: interfaces.iter().map(|ty| (*ty).into()).collect(),
                symbols: Arc::new(symbols),
                fields: fields
                    .iter()
                    .map(|(name, field_type)| DexField {
                        declaring_type: ty.into(),
                        name: (*name).into(),
                        field_type: (*field_type).into(),
                        access_flags: 1,
                        is_static: false,
                    })
                    .collect(),
                methods,
                annotations_offset: 0,
                static_values_offset: 0,
                static_values: vec![],
            },
        )
    }

    #[test]
    fn named_hook_map_proves_renamed_accessors_builder_and_inherited_proceed() {
        const OBJECT: &str = "Ljava/lang/Object;";
        const BASE: &str = "Lokhttp3/internal/Base;";
        const CHILD: &str = "Lokhttp3/internal/Child;";
        let request = class(
            REQUEST,
            OBJECT,
            1,
            &[],
            DexSymbols {
                types: [REQUEST, URL, STRING, HEADERS, BUILDER]
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                strings: ["urlField", "verbField", "headField", "builderVerb"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                fields: vec![(0, 1, 0), (0, 2, 1), (0, 3, 2), (4, 2, 3)],
                ..Default::default()
            },
            &[
                ("urlField", URL),
                ("verbField", STRING),
                ("headField", HEADERS),
            ],
            vec![
                method(
                    REQUEST,
                    "renamedUrl",
                    &[],
                    URL,
                    Some((2, vec![0x1054, 0, 0x0011])),
                ),
                method(
                    REQUEST,
                    "renamedMethod",
                    &[],
                    STRING,
                    Some((2, vec![0x1054, 1, 0x0011])),
                ),
                method(
                    REQUEST,
                    "renamedHeaders",
                    &[],
                    HEADERS,
                    Some((2, vec![0x1054, 2, 0x0011])),
                ),
                method(
                    REQUEST,
                    "<init>",
                    &[BUILDER],
                    "V",
                    Some((3, vec![0x2054, 3, 0x105b, 1, 0x0e])),
                ),
            ],
        );
        let builder = class(
            BUILDER,
            OBJECT,
            1,
            &[],
            DexSymbols {
                types: [BUILDER, REQUEST, STRING]
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                strings: ["builderVerb", "GET", "<init>"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                fields: vec![(0, 2, 0)],
                protos: vec![("V".into(), vec![BUILDER.into()])],
                methods: vec![(1, 0, 2)],
                ..Default::default()
            },
            &[("builderVerb", STRING)],
            vec![
                method(
                    BUILDER,
                    "<init>",
                    &[],
                    "V",
                    Some((2, vec![0x001a, 1, 0x105b, 0, 0x0e])),
                ),
                method(
                    BUILDER,
                    "setter",
                    &[STRING, "Lokhttp3/RequestBody;"],
                    "V",
                    Some((3, vec![0x015b, 0, 0x0e])),
                ),
                method(
                    BUILDER,
                    "create",
                    &[],
                    REQUEST,
                    Some((2, vec![0x0022, 1, 0x2070, 0, 0x0010, 0x0011])),
                ),
                method(
                    BUILDER,
                    "falseCandidate",
                    &[],
                    REQUEST,
                    Some((2, vec![0x0012, 0x0011])),
                ),
            ],
        );
        let contract = class(
            INTERCEPTOR,
            OBJECT,
            0x600,
            &[],
            DexSymbols::default(),
            &[],
            vec![method(INTERCEPTOR, "next", &[CHAIN], RESPONSE, None)],
        );
        let chain = class(
            CHAIN,
            OBJECT,
            0x600,
            &[],
            DexSymbols::default(),
            &[],
            vec![
                method(CHAIN, "step", &[REQUEST], RESPONSE, None),
                method(CHAIN, "notProceed", &[REQUEST], RESPONSE, None),
            ],
        );
        let base = class(
            BASE,
            OBJECT,
            0x400,
            &[],
            DexSymbols {
                types: vec![INTERCEPTOR.into()],
                strings: vec!["next".into()],
                protos: vec![(RESPONSE.into(), vec![CHAIN.into()])],
                methods: vec![(0, 0, 0)],
                ..Default::default()
            },
            &[],
            vec![
                method(
                    BASE,
                    "step",
                    &[REQUEST],
                    RESPONSE,
                    Some((3, vec![0x2072, 0, 0x0010, 0x000c, 0x0011])),
                ),
                method(
                    BASE,
                    "notProceed",
                    &[REQUEST],
                    RESPONSE,
                    Some((3, vec![0x2072, 0, 0x0010, 0x000c, 0x0012, 0x0011])),
                ),
            ],
        );
        let child = class(CHILD, BASE, 1, &[CHAIN], DexSymbols::default(), &[], vec![]);
        let mut classes = BTreeMap::from([request, builder, contract, chain, base, child]);
        let names: Vec<_> = classes.keys().cloned().collect();
        let engine = NativeEngine::from_test_classes(std::mem::take(&mut classes));
        let project = Project {
            classes: names,
            resources: vec![],
        };
        let cancel = AtomicBool::new(false);
        let args = json!({"projection":"hook_maps","include_observer":true});
        let result = engine.network_stacks(&project, &args, &cancel).unwrap();
        let roles = &result["items"][0]["roles"];
        assert_eq!(
            roles["request_builder"]["method_id"],
            "okhttp3.Request$Builder.create()Lokhttp3/Request;"
        );
        assert_eq!(
            roles["request_method"]["method_id"],
            "okhttp3.Request.renamedMethod()Ljava/lang/String;"
        );
        assert_eq!(
            roles["request_url"]["method_id"],
            "okhttp3.Request.renamedUrl()Lokhttp3/HttpUrl;"
        );
        assert_eq!(
            roles["request_headers"]["method_id"],
            "okhttp3.Request.renamedHeaders()Lokhttp3/Headers;"
        );
        assert_eq!(
            roles["chain_proceed"]["method_id"],
            "okhttp3.Interceptor$Chain.step(Lokhttp3/Request;)Lokhttp3/Response;"
        );
        assert_eq!(
            roles["chain_proceed"]["implementations"][0]["method_id"],
            "okhttp3.internal.Base.step(Lokhttp3/Request;)Lokhttp3/Response;"
        );
        assert_eq!(result["items"][0]["observer"]["status"], "template");
        assert_eq!(
            result["items"][0]["observer"]["omitted_roles"],
            json!(["origin", "headers"])
        );
        assert_eq!(result["cache_hit"], false);
        assert_eq!(
            engine.network_stacks(&project, &args, &cancel).unwrap()["cache_hit"],
            true
        );
        let scoped = engine
            .network_stacks(
                &project,
                &json!({"projection":"hook_maps","package_prefix":"example.app"}),
                &cancel,
            )
            .unwrap();
        assert_eq!(scoped["items"], json!([]));
        assert_eq!(scoped["coverage"]["scope_basis"], "map_declaration_owner");
        assert_eq!(scoped["coverage"]["retained_named_maps_outside_prefix"], 1);
        assert!(
            scoped["scope_guidance"]
                .as_str()
                .unwrap()
                .contains("projection=all")
        );
        let structural = structural_row(
            &json!({"hook_map":{"interceptor_method_id":"okhttp3.App.next()V",
            "chain_proceed_method_id":"okhttp3.internal.Base.step(Lokhttp3/Request;)Lokhttp3/Response;",
            "request_url_field_id":"okhttp3.Request.urlField:Lokhttp3/HttpUrl;"}}),
            role(vec![]),
        );
        let keys = |roles: &Value| {
            roles
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(roles), keys(&structural["roles"]));
        assert_eq!(structural["roles"]["chain_proceed"]["status"], "candidate");
        assert_eq!(
            structural["roles"]["request_url"]["field_id"],
            "okhttp3.Request.urlField:Lokhttp3/HttpUrl;"
        );
        assert!(observer(&structural).is_none());
    }

    #[test]
    #[ignore = "requires RDX_HOOK_APK for exact Splitwise named SDK regression"]
    fn network_hook_maps_exact_splitwise_named_observer() {
        const SHA: &str = "0cf24232bf514095d0eddf604c26179fc54bc0f5839c66c42722f653d009f9e9";
        let path =
            std::path::PathBuf::from(std::env::var_os("RDX_HOOK_APK").expect("Set RDX_HOOK_APK"));
        let snapshot = crate::project_snapshot::ProjectSnapshot::open(&path).unwrap();
        assert_eq!(snapshot.apk_sha256(), SHA);
        let cancel = AtomicBool::new(false);
        let call = |args: Value| {
            snapshot
                .engine()
                .network_stacks(snapshot.project(), &args, &cancel)
                .unwrap()
        };
        let full = call(json!({"projection":"hook_maps","include_observer":true}));
        let scoped = call(
            json!({"projection":"hook_maps","package_prefix":"com.Splitwise.SplitwiseMobile"}),
        );
        if let Some(path) = std::env::var_os("RDX_HOOK_OUTPUT") {
            std::fs::write(
                path,
                serde_json::to_vec_pretty(&json!({"apk_sha256":SHA,"global":full,"scoped":scoped}))
                    .unwrap(),
            )
            .unwrap();
        }
        let named = full["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["recognition"][0] == "named_okhttp3")
            .expect("named OkHttp map");
        let roles = &named["roles"];
        for (role, id) in [
            (
                "request_builder",
                "okhttp3.Request$Builder.build()Lokhttp3/Request;",
            ),
            ("request_url", "okhttp3.Request.url()Lokhttp3/HttpUrl;"),
            (
                "request_method",
                "okhttp3.Request.method()Ljava/lang/String;",
            ),
            (
                "request_headers",
                "okhttp3.Request.headers()Lokhttp3/Headers;",
            ),
            (
                "request_url_setter",
                "okhttp3.Request$Builder.url(Ljava/lang/String;)Lokhttp3/Request$Builder;",
            ),
            ("url_conversion", "okhttp3.HttpUrl.url()Ljava/net/URL;"),
            ("header_name", "okhttp3.Headers.name(I)Ljava/lang/String;"),
        ] {
            assert_eq!(roles[role]["status"], "resolved", "{role}: {}", roles[role]);
            assert_eq!(roles[role]["method_id"], id);
        }
        assert_eq!(
            roles["chain_proceed"]["implementations"][0]["method_id"],
            "okhttp3.internal.http.RealInterceptorChain.proceed(Lokhttp3/Request;)Lokhttp3/Response;"
        );
        assert_eq!(named["observer"]["status"], "template");
        assert_eq!(named["observer"]["omitted_roles"], json!([]));
        assert_eq!(scoped["coverage"]["retained_named_maps_outside_prefix"], 1);
        assert_eq!(scoped["cache_hit"], true);
    }

    #[test]
    fn over_budget_candidates_never_prove_unique_roles() {
        let methods: Vec<_> = (0..=64)
            .map(|index| method(BUILDER, &format!("create{index}"), &[], REQUEST, None))
            .collect();
        let prove = |method: &DexMethod| {
            matches!(method.name.as_ref(), "create0" | "create64")
                .then(|| json!({"method_id":method_id(method)}))
        };
        let truncated = bounded_roles(methods.iter(), 64, prove);
        assert_eq!(truncated["status"], "unresolved");
        assert_eq!(truncated["proven_candidate_count_so_far"], 1);
        assert_eq!(
            bounded_roles(methods.iter(), 65, prove)["status"],
            "ambiguous"
        );
        let preview = bounded_roles(methods.iter(), 65, |method| {
            Some(json!({"method_id":method_id(method)}))
        });
        assert_eq!(preview["candidate_count"], 65);
        assert_eq!(preview["candidates_truncated"], true);
        assert_eq!(preview["candidates"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn observer_templates_emit_only_allowed_names_and_values() {
        let script = format!(
            r#"
const events = [];
const log = console.log;
console.log = value => events.push(JSON.parse(value));
const target = "okhttp3.internal.Base";
const request = "okhttp3.Request";
const mockUrl = {{
  getProtocol: () => "https", getHost: () => "example.com", getPort: () => 443,
  getUserInfo: () => "secret-userinfo", getPath: () => "secret-path", getQuery: () => "secret-query",
  getRef: () => "secret-fragment", toString: () => "secret-url"
}};
const mockRequest = {{body: () => "secret-body"}};
let headers = ["Authorization","Content-Type","X-API-Key","secret-header-name"];
let method = "POST";
let throwing = false;
let headerFailure = -1;
const classes = {{}};
function overload(owner, name, args, ret, fn) {{
  classes[owner] ??= {{}};
  const member = {{
    argumentTypes: args.map(x => ({{className:x}})), returnType: {{className:ret}},
    call: (receiver, ...params) => fn(receiver,...params)
  }};
  classes[owner][name] = {{overloads:[member]}};
  return member;
}}
const hook = overload(target,"go",[request],"okhttp3.Response",() => "unchanged");
overload(request,"u",[],"okhttp3.HttpUrl",() => {{ if(throwing) throw Error("secret-exception"); return mockUrl; }});
overload(request,"m",[],"java.lang.String",() => method);
overload(request,"h",[],"okhttp3.Headers",() => headers);
overload("okhttp3.HttpUrl","c",[],"java.net.URL",url => url);
overload("okhttp3.Headers","n",["int"],"java.lang.String",(list,i) => {{
  if (i >= list.length || i === headerFailure) throw Error("secret-getter");
  return list[i];
}});
classes["okhttp3.Headers"].class = {{getDeclaredField: name => {{
  if (name !== "n") throw Error("secret-field");
  return {{
    getType: () => ({{getName: () => "[Ljava.lang.String;"}}),
    setAccessible: () => {{}},
    get: list => list.flatMap(name => [name,"secret-header-value"])
  }};
}}}};
classes["java.lang.reflect.Array"] = {{getLength: array => array.length}};
const Java = {{perform: fn => fn(),use: name => classes[name]}};
const selectors = {{owner:target,member:"go",request,url:["u","c"],method:"m",headers:["h","n","n"]}};
{}
if (hook.implementation(mockRequest) !== "unchanged") throw Error("original return changed");
throwing = true;
method = "secret-method";
hook.implementation(mockRequest);
headers = Array(257).fill("Accept");
hook.implementation(mockRequest);
headers = ["Cookie",null,"Authorization"];
hook.implementation(mockRequest);
headers = ["Authorization","Cookie","X-API-Key"];
headerFailure = 1;
hook.implementation(mockRequest);
delete classes[target].go;
{}
log(JSON.stringify(events));
"#,
            include_str!("network_observer.js"),
            include_str!("network_observer.js")
        );
        let output = match std::process::Command::new("node")
            .arg("-e")
            .arg(script)
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("Observer fixture failed: {error}"),
        };
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("secret-"), "{text}");
        let events: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            events[0],
            json!({"origin":{"scheme":"https","host":"example.com","port":443},
            "http_method":"POST","header_names":["authorization","content-type","x-api-key"],"headers_complete":true,
            "header_names_scope":"allowlisted","omitted_header_name_count":1,
            "credential_presence":{"authorization":true,"proxy-authorization":false,"cookie":false,"x-api-key":true}})
        );
        assert!(events[1].get("origin").is_none() && events[1].get("http_method").is_none());
        assert_eq!(events[2]["headers_complete"], false);
        assert_eq!(events[2]["credential_presence"], json!({}));
        assert_eq!(events[3]["headers_complete"], false);
        assert_eq!(events[3]["credential_presence"], json!({"cookie":true}));
        assert_eq!(events[4]["headers_complete"], false);
        assert_eq!(
            events[4]["credential_presence"],
            json!({"authorization":true})
        );
        assert_eq!(
            events[5],
            json!({"observer_status":"unavailable","phase":"install"})
        );
    }
}
