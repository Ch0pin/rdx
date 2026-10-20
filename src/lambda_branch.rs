//! Conservative constructor discriminator proof and raw DEX branch regions.
//! Offsets are DEX code units. Unsupported flow is explicitly unresolved.
use crate::native_cfg::{ControlFlowGraph, EdgeKind};
use crate::native_dex::{DexClass, DexMethod, DexSymbols};
use crate::native_ir::{DecodedMethod, Instruction};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Origin {
    Constant(i32),
    Parameter(u16),
    Allocation(usize),
    Field(String),
    Unknown,
}
fn method_id(m: &DexMethod) -> String {
    crate::native_engine::disassembly::method_id(m)
}
fn symbol(s: &DexSymbols, i: &Instruction, field: bool) -> Option<String> {
    let index = i.reference?.index as usize;
    if field {
        let &(owner, ty, name) = s.fields.get(index)?;
        Some(format!(
            "{}->{}:{}",
            s.types.get(owner as usize)?,
            s.strings.get(name as usize)?,
            s.types.get(ty as usize)?
        ))
    } else {
        let &(owner, proto, name) = s.methods.get(index)?;
        let (ret, args) = s.protos.get(proto as usize)?;
        Some(format!(
            "{}.{}({}){}",
            crate::native_engine::disassembly::type_name(s.types.get(owner as usize)?),
            s.strings.get(name as usize)?,
            args.join(""),
            ret
        ))
    }
}
fn read(state: &[Origin], i: &Instruction, n: usize) -> Origin {
    i.reads
        .get(n)
        .and_then(|r| state.get(r.register as usize))
        .cloned()
        .unwrap_or(Origin::Unknown)
}
fn transfer(state: &mut [Origin], i: &Instruction) {
    let value = match i.opcode {
        0x01..=0x09 => read(state, i, 0),
        0x12..=0x15 => i
            .literal
            .map(|x| Origin::Constant(x as i32))
            .unwrap_or(Origin::Unknown),
        0x22 => Origin::Allocation(i.pc),
        0x8d => match read(state, i, 0) {
            Origin::Constant(x) => Origin::Constant(x as i8 as i32),
            _ => Origin::Unknown,
        },
        _ => Origin::Unknown,
    };
    for w in &i.writes {
        for word in 0..w.kind.word_count() {
            state[w.register as usize + word] = value.clone();
        }
    }
}
fn linear(i: &Instruction) -> bool {
    !matches!(i.opcode, 0x0e..=0x11 | 0x27..=0x2c | 0x32..=0x3d)
}
fn initial(m: &DexMethod) -> Option<Vec<Origin>> {
    let c = m.code.as_ref()?;
    let start = c.registers.checked_sub(c.ins)?;
    let mut state = vec![Origin::Unknown; c.registers as usize];
    for r in start..c.registers {
        state[r as usize] = Origin::Parameter(r - start);
    }
    Some(state)
}
fn instruction_text(
    m: &DexMethod,
    symbols: &DexSymbols,
    cancelled: &impl Fn() -> bool,
) -> Result<HashMap<usize, String>, String> {
    let code = m.code.as_ref().ok_or("dispatcher code missing")?;
    if code.instructions.len() > 65_536 {
        return Err("dispatcher readable disassembly exceeds code budget".into());
    }
    let ir = DecodedMethod::decode(code).map_err(|e| e.to_string())?;
    let mut string_work = 0usize;
    for instruction in &ir.instructions {
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        if let Some(reference) = instruction.reference
            && reference.kind == crate::native_ir::PoolKind::String
        {
            string_work = string_work.saturating_add(
                symbols
                    .strings
                    .get(reference.index as usize)
                    .map_or(0, String::len),
            );
            if string_work > 2_000_000 {
                return Err("dispatcher readable disassembly exceeds string budget".into());
            }
        }
    }
    let rendered = crate::native_engine::disassembly::render_method(m, symbols);
    let mut text = HashMap::new();
    for line in rendered.source.lines() {
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        let line = line.trim();
        if let Some((offset, _)) = line.split_once(':')
            && let Ok(offset) = usize::from_str_radix(offset, 16)
        {
            text.insert(offset, line.to_owned());
        }
    }
    Ok(text)
}
fn unresolved(reason: impl AsRef<str>) -> Value {
    json!({"status":"unresolved", "reason":reason.as_ref(), "offset_unit":"dex_code_units"})
}

pub(crate) fn analyze(
    classes: &[&DexClass],
    caller_method_id: &str,
    call_site_offset: usize,
    cancelled: &impl Fn() -> bool,
) -> Value {
    match prove(classes, caller_method_id, call_site_offset, cancelled) {
        Ok(v) => v,
        Err(e) => unresolved(e),
    }
}
fn prove(
    classes: &[&DexClass],
    caller: &str,
    pc: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Value, String> {
    if cancelled() {
        return Err("analysis cancelled".into());
    }
    let caller_owner = caller
        .rsplit_once('.')
        .map(|(owner, _)| owner)
        .ok_or("invalid caller method ID")?;
    let mut candidates = Vec::new();
    for class in classes {
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        if crate::native_engine::disassembly::type_name(&class.descriptor) != caller_owner {
            continue;
        }
        for method in &class.methods {
            if method_id(method) == caller {
                candidates.push((*class, method));
            }
        }
    }
    if candidates.len() != 1 {
        return Err("caller method must resolve uniquely".into());
    }
    let (caller_class, method) = candidates[0];
    let code = method.code.as_ref().ok_or("caller has no DEX code")?;
    let ir = DecodedMethod::decode(code).map_err(|e| e.to_string())?;
    let call = ir
        .instructions
        .iter()
        .find(|i| i.pc == pc)
        .ok_or("call site is not an instruction boundary")?;
    if !matches!(call.opcode, 0x70 | 0x76) {
        return Err("call site must be invoke-direct constructor".into());
    }
    let target = symbol(&caller_class.symbols, call, false).ok_or("invalid constructor symbol")?;
    let constructor_owner = target
        .rsplit_once('.')
        .map(|(owner, _)| owner)
        .ok_or("invalid constructor ID")?;
    let mut constructors = Vec::new();
    for class in classes {
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        if crate::native_engine::disassembly::type_name(&class.descriptor) != constructor_owner {
            continue;
        }
        for method in &class.methods {
            if method_id(method) == target && method.name.as_ref() == "<init>" {
                constructors.push((*class, method));
            }
        }
    }
    if constructors.len() != 1 {
        return Err("constructor must resolve uniquely".into());
    }
    let (class, ctor) = constructors[0];
    // Start with unknown incoming values, then follow the unique predecessor chain.
    // This admits constructor sites inside a switch case without guessing at joins.
    let cfg = ControlFlowGraph::build(code).map_err(|e| e.to_string())?;
    let mut block = *cfg.block_at.get(&pc).ok_or("call site CFG block absent")?;
    let mut chain = vec![block];
    let mut predecessor_work = 0usize;
    loop {
        predecessor_work = predecessor_work.saturating_add(cfg.blocks.len());
        if predecessor_work > 1_000_000 {
            return Err("caller predecessor proof exceeds work budget".into());
        }
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        let predecessors: Vec<_> = cfg
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(b, x)| {
                x.successors
                    .iter()
                    .filter(move |e| e.target == block)
                    .map(move |e| (b, e.kind))
            })
            .collect();
        if predecessors.len() != 1
            || predecessors[0].1 != EdgeKind::Normal
            || chain.contains(&predecessors[0].0)
        {
            break;
        }
        block = predecessors[0].0;
        chain.push(block);
    }
    chain.reverse();
    let at: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let prefix: Vec<_> = chain
        .iter()
        .flat_map(|b| cfg.blocks[*b].instructions.iter())
        .take_while(|p| **p != pc)
        .filter_map(|p| at.get(p).copied())
        .collect();
    let mut state = vec![Origin::Unknown; code.registers as usize];
    for i in &prefix {
        transfer(&mut state, i);
    }
    let Origin::Allocation(allocation) = read(&state, call, 0) else {
        return Err("constructor receiver is not a proven fresh allocation".into());
    };
    let alloc = ir
        .instructions
        .iter()
        .find(|i| i.pc == allocation)
        .ok_or("allocation missing")?;
    let alloc_type = alloc
        .reference
        .and_then(|r| caller_class.symbols.types.get(r.index as usize))
        .ok_or("invalid allocation type")?;
    if alloc_type != &class.descriptor {
        return Err("allocation type differs from constructor owner".into());
    }
    let allocation_index = prefix
        .iter()
        .position(|i| i.pc == allocation)
        .ok_or("allocation not in reaching path")?;
    if prefix
        .iter()
        .skip(allocation_index + 1)
        .any(|i| matches!(i.opcode,0x4b..=0x51|0x59..=0x5f|0x67..=0x78|0xfa..=0xfd))
    {
        return Err("allocation may escape before constructor".into());
    }
    let args: Vec<_> = call
        .reads
        .iter()
        .map(|r| state[r.register as usize].clone())
        .collect();
    let cc = ctor.code.as_ref().ok_or("constructor has no code")?;
    if !cc.try_regions.is_empty() {
        return Err("constructor exception flow unsupported".into());
    }
    if ctor.access_flags & 8 != 0 || cc.ins as usize != args.len() {
        return Err("constructor argument/register layout mismatch".into());
    }
    let ci = DecodedMethod::decode(cc).map_err(|e| e.to_string())?;
    let mut cs = initial(ctor).ok_or("invalid constructor parameters")?;
    let mut stores = HashMap::<String, (Origin, usize)>::new();
    let mut returned = false;
    for i in &ci.instructions {
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        if matches!(i.opcode,0x4b..=0x51|0x67..=0x6d)
            && i.reads
                .iter()
                .any(|r| cs[r.register as usize] == Origin::Parameter(0))
        {
            return Err("constructor explicitly publishes this".into());
        }
        if matches!(i.opcode, 0x59..=0x5f) && read(&cs, i, 1) == Origin::Parameter(0) {
            return Err("constructor stores this as a field value".into());
        }
        if returned {
            return Err("constructor contains instructions after return".into());
        }
        if i.opcode == 0x0e {
            returned = true;
            continue;
        }
        if !linear(i) {
            return Err("constructor has control flow requiring SSA proof".into());
        }
        if matches!(i.opcode,0x6e..=0x78|0xfa..=0xfd) {
            if i.reads
                .iter()
                .skip(1)
                .any(|r| cs[r.register as usize] == Origin::Parameter(0))
            {
                return Err("constructor passes this as an explicit call argument".into());
            }
            if !matches!(i.opcode, 0x70 | 0x76)
                || !symbol(&class.symbols, i, false).is_some_and(|name| {
                    name == "java.lang.Object.<init>()V"
                        || class.superclass.as_ref().is_some_and(|owner| {
                            name.starts_with(&format!(
                                "{}.<init>(",
                                crate::native_engine::disassembly::type_name(owner)
                            ))
                        })
                })
                || read(&cs, i, 0) != Origin::Parameter(0)
            {
                return Err(
                    "constructor delegates or invokes code with unproven field effects".into(),
                );
            }
        }
        if matches!(i.opcode, 0x59..=0x5f) {
            if read(&cs, i, 0) != Origin::Parameter(0) {
                return Err("constructor writes through unproven receiver".into());
            }
            let field = symbol(&class.symbols, i, true).ok_or("invalid stored field")?;
            let origin = if matches!(
                (i.opcode, field.rsplit(':').next()),
                (0x59, Some("I"))
                    | (0x5c, Some("Z"))
                    | (0x5d, Some("B"))
                    | (0x5e, Some("C"))
                    | (0x5f, Some("S"))
            ) {
                read(&cs, i, 1)
            } else {
                Origin::Unknown
            };
            if stores.insert(field, (origin, i.pc)).is_some() {
                return Err("constructor writes same field multiple times".into());
            }
        }
        transfer(&mut cs, i);
    }
    if !returned {
        return Err("constructor normal return absent".into());
    }
    // Scan loaded references to the selected instance fields once, using each
    // DEX's own symbol table. Cross-class indices are never compared directly.
    let mut mutated = BTreeSet::new();
    let mut relevant_by_dex = HashMap::<usize, HashMap<u32, String>>::new();
    for owner in classes {
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        let dex_identity = std::sync::Arc::as_ptr(&owner.symbols) as usize;
        if let std::collections::hash_map::Entry::Vacant(entry) =
            relevant_by_dex.entry(dex_identity)
        {
            let mut relevant = HashMap::new();
            for (index, &(declaring, ty, name)) in owner.symbols.fields.iter().enumerate() {
                if cancelled() {
                    return Err("analysis cancelled".into());
                }
                if owner.symbols.types.get(declaring as usize) != Some(&class.descriptor) {
                    continue;
                }
                let (Some(name), Some(ty)) = (
                    owner.symbols.strings.get(name as usize),
                    owner.symbols.types.get(ty as usize),
                ) else {
                    continue;
                };
                let field = format!("{}->{}:{}", class.descriptor, name, ty);
                if stores.contains_key(&field) {
                    relevant.insert(index as u32, field);
                }
            }
            entry.insert(relevant);
        }
        let relevant = &relevant_by_dex[&dex_identity];
        if relevant.is_empty() {
            continue;
        }
        for method in &owner.methods {
            if cancelled() {
                return Err("analysis cancelled".into());
            }
            if method.name.as_ref() == "<init>" && owner.descriptor == class.descriptor {
                continue;
            }
            let Some(code) = &method.code else { continue };
            let words = &code.instructions;
            let mut offset = 0;
            while offset < words.len() {
                if cancelled() {
                    return Err("analysis cancelled".into());
                }
                let (width, payload) = crate::native_cfg::instruction_width(words, offset)
                    .map_err(|e| e.to_string())?;
                if width == 0
                    || offset
                        .checked_add(width)
                        .is_none_or(|end| end > words.len())
                {
                    return Err("invalid loaded instruction width".into());
                }
                if !payload
                    && matches!(words[offset] as u8, 0x59..=0x5f)
                    && let Some(field) = words
                        .get(offset + 1)
                        .and_then(|index| relevant.get(&u32::from(*index)))
                {
                    mutated.insert(field.clone());
                }
                offset += width;
            }
        }
    }
    stores.retain(|field, _| !mutated.contains(field));
    let mut proofs = Vec::new();
    for dispatcher in &class.methods {
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        if dispatcher.name.starts_with('<') || dispatcher.access_flags & 8 != 0 {
            continue;
        }
        if let Some(proof) = dispatch(class, dispatcher, &stores, &args, cancelled)? {
            proofs.push(proof);
        }
    }
    if proofs.is_empty() {
        return Err(
            "no dispatcher has a proven this-field switch fed by a constant constructor argument"
                .into(),
        );
    }
    Ok(
        json!({"status":"resolved", "offset_unit":"dex_code_units", "caller_method_id":caller, "call_site_offset":pc,"allocation_offset":allocation,"class_id":crate::native_engine::disassembly::type_name(&class.descriptor),"constructor_method_id":target,"dispatchers":proofs,"scope":"after normal constructor completion, assuming verified DEX final-field semantics and no reflection/native field writes; regions include conservative successor and exception closure"}),
    )
}

fn dispatch(
    class: &DexClass,
    m: &DexMethod,
    stores: &HashMap<String, (Origin, usize)>,
    args: &[Origin],
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Value>, String> {
    let Some(code) = &m.code else { return Ok(None) };
    let ir = DecodedMethod::decode(code).map_err(|e| e.to_string())?;
    let Some(mut state) = initial(m) else {
        return Ok(None);
    };
    for i in &ir.instructions {
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        if matches!(i.opcode, 0x2b | 0x2c) {
            let Origin::Field(field) = read(&state, i, 0) else {
                return Ok(None);
            };
            let Some((Origin::Parameter(parameter), store_pc)) = stores.get(&field) else {
                return Ok(None);
            };
            let Some(definition) = class
                .fields
                .iter()
                .find(|f| format!("{}->{}:{}", f.declaring_type, f.name, f.field_type) == field)
            else {
                return Ok(None);
            };
            if definition.access_flags & 0x10 == 0
                || definition.is_static
                || definition.declaring_type != class.descriptor
            {
                return Ok(None);
            }
            let Some(Origin::Constant(value)) = args.get(*parameter as usize) else {
                return Ok(None);
            };
            let value = match definition.field_type.as_ref() {
                "I" => *value,
                "B" => *value as i8 as i32,
                "S" => *value as i16 as i32,
                "C" => *value as u16 as i32,
                "Z" if *value == 0 || *value == 1 => *value,
                _ => return Ok(None),
            };
            let payload = i.payload_target.ok_or("switch payload missing")?;
            let words = &code.instructions;
            let count = words[payload + 1] as usize;
            let word32 = |at: usize| (u32::from(words[at]) | u32::from(words[at + 1]) << 16) as i32;
            let mut targets = Vec::new();
            let mut selected = i.pc + i.width;
            let mut matched = false;
            for n in 0..count {
                let key = if i.opcode == 0x2b {
                    word32(payload + 2) + n as i32
                } else {
                    word32(payload + 2 + n * 2)
                };
                let at = if i.opcode == 0x2b {
                    payload + 4 + n * 2
                } else {
                    payload + 2 + count * 2 + n * 2
                };
                let target = (i.pc as i64 + i64::from(word32(at))) as usize;
                if key != value {
                    targets.push(target);
                }
                if key == value {
                    selected = target;
                    matched = true;
                }
            }
            if matched {
                targets.push(i.pc + i.width);
            }
            let cfg = ControlFlowGraph::build(code).map_err(|e| e.to_string())?;
            let prelude_block_ids: BTreeSet<_> = ir
                .instructions
                .iter()
                .take_while(|x| x.pc <= i.pc)
                .filter_map(|x| cfg.block_at.get(&x.pc).copied())
                .collect();
            if cfg.blocks.iter().enumerate().any(|(source, block)| {
                block.successors.iter().any(|edge| {
                    prelude_block_ids.contains(&edge.target)
                        && (edge.kind == EdgeKind::Exceptional
                            || !prelude_block_ids.contains(&source))
                })
            }) {
                return Ok(None);
            }
            if code
                .try_regions
                .iter()
                .flat_map(|region| region.catches.iter())
                .any(|(_, handler)| {
                    cfg.block_at
                        .get(&(*handler as usize))
                        .is_some_and(|block| prelude_block_ids.contains(block))
                })
            {
                return Ok(None);
            }
            let readable = instruction_text(m, &class.symbols, cancelled)?;
            let entry = *cfg
                .block_at
                .get(&selected)
                .ok_or("selected case has no CFG block")?;
            let chosen = closure(&cfg, &[entry], true, cancelled)?;
            let other_entries: Vec<_> = targets
                .iter()
                .filter_map(|t| cfg.block_at.get(t).copied())
                .collect();
            let others = closure(&cfg, &other_entries, true, cancelled)?;
            let normal = closure(&cfg, &[entry], false, cancelled)?;
            let by_pc: HashMap<_, _> = ir.instructions.iter().map(|x| (x.pc, x)).collect();
            let prelude_instructions:Vec<_>=ir.instructions.iter().take_while(|x|x.pc<=i.pc).map(|x|json!({"offset":x.pc,"text":readable.get(&x.pc),"opcode":format!("0x{:02x}",x.opcode),"code_units":&words[x.pc..x.pc+x.width]})).collect();
            let prelude_blocks: BTreeSet<_> = ir
                .instructions
                .iter()
                .take_while(|x| x.pc <= i.pc)
                .filter_map(|x| cfg.block_at.get(&x.pc).copied())
                .collect();
            let exception_roots: Vec<_> = prelude_blocks
                .iter()
                .flat_map(|b| cfg.blocks[*b].successors.iter())
                .filter(|e| e.kind == EdgeKind::Exceptional)
                .map(|e| e.target)
                .collect();
            let prelude_exceptions = closure(&cfg, &exception_roots, true, cancelled)?;
            let prelude_exception_blocks:Vec<_>=prelude_exceptions.iter().map(|b| {
                let block=&cfg.blocks[*b];
                json!({"block_id":b,"start_offset":block.start,"end_offset_exclusive":block.end,"instructions":block.instructions.iter().filter_map(|pc|by_pc.get(pc).copied()).map(|x|json!({"offset":x.pc,"text":readable.get(&x.pc),"code_units":&words[x.pc..x.pc+x.width]})).collect::<Vec<_>>(),"successors":block.successors.iter().map(|e|json!({"block_id":e.target,"kind":if e.kind==EdgeKind::Exceptional {"exceptional"}else{"normal"}})).collect::<Vec<_>>()})
            }).collect();

            let blocks:Vec<_>=chosen.iter().map(|&b| {
                let block=&cfg.blocks[b];
                let instructions:Vec<_>=block.instructions.iter().filter_map(|p|by_pc.get(p)).map(|x|json!({"offset":x.pc,"text":readable.get(&x.pc),"opcode":format!("0x{:02x}",x.opcode),"code_units":&words[x.pc..x.pc+x.width],"referenced_method_id":if matches!(x.opcode,0x6e..=0x78|0xfa|0xfb) {symbol(&class.symbols,x,false)} else {None}})).collect();
                json!({"block_id":b,"start_offset":block.start,"end_offset_exclusive":block.end,"shared_with_other_case":others.contains(&b),"exception_only":!normal.contains(&b),"instructions":instructions,"successors":block.successors.iter().map(|e|json!({"block_id":e.target,"kind":if e.kind==EdgeKind::Exceptional {"exceptional"} else {"normal"}})).collect::<Vec<_>>()})
            }).collect();
            return Ok(Some(
                json!({"method_id":method_id(m),"class_id":crate::native_engine::disassembly::type_name(&class.descriptor),"discriminator":value,"field_id":field,"constructor_argument_word":parameter,"constructor_store_offset":store_pc,"switch_offset":i.pc,"selected_target_offset":selected,"selected_default":!matched,"prelude_instructions":prelude_instructions,"prelude_exception_blocks":prelude_exception_blocks,"blocks":blocks,"provenance":["caller const/move dataflow to constructor argument","constructor parameter/move dataflow to this-field store","dispatcher this-field/move dataflow to switch","validated DEX payload and CFG successor closure"]}),
            ));
        }
        if !linear(i) {
            return Ok(None);
        }
        if matches!(i.opcode, 0x52 | 0x55..=0x58) && read(&state, i, 0) == Origin::Parameter(0) {
            let field = symbol(&class.symbols, i, true).ok_or("invalid discriminator field")?;
            // Match the declared field width to its exact load opcode.
            if matches!(
                (i.opcode, field.rsplit(':').next()),
                (0x52, Some("I"))
                    | (0x55, Some("Z"))
                    | (0x56, Some("B"))
                    | (0x57, Some("C"))
                    | (0x58, Some("S"))
            ) {
                for w in &i.writes {
                    state[w.register as usize] = Origin::Field(field.clone());
                }
                continue;
            }
        }
        // No intervening calls or field writes can invalidate this-field provenance.
        if matches!(i.opcode,0x59..=0x5f|0x67..=0x78|0xfa..=0xfd) {
            return Ok(None);
        }
        transfer(&mut state, i);
    }
    Ok(None)
}
fn closure(
    cfg: &ControlFlowGraph,
    entries: &[usize],
    exceptions: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<BTreeSet<usize>, String> {
    let mut seen = BTreeSet::new();
    let mut pending = entries.to_vec();
    while let Some(b) = pending.pop() {
        if cancelled() {
            return Err("analysis cancelled".into());
        }
        if seen.insert(b) {
            for edge in &cfg.blocks[b].successors {
                if exceptions || edge.kind == EdgeKind::Normal {
                    pending.push(edge.target)
                }
            }
        }
    }
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::DexCode;
    fn decode(words: Vec<u16>) -> DecodedMethod {
        DecodedMethod::decode(&DexCode {
            registers: 4,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        })
        .unwrap()
    }
    #[test]
    fn constant_move_and_byte_conversion_keep_exact_signed_value() {
        let ir = decode(vec![0x0013, 255, 0x0101, 0x128d, 0x000e]);
        let mut state = vec![Origin::Unknown; 4];
        for i in &ir.instructions {
            transfer(&mut state, i);
        }
        assert_eq!(state[0], Origin::Constant(255));
        assert_eq!(state[1], Origin::Constant(255));
        assert_eq!(state[2], Origin::Constant(-1));
    }
    #[test]
    fn arithmetic_overwrite_discards_constant_proof() {
        let ir = decode(vec![0x1012, 0x00d8, 1, 0x000e]);
        let mut state = vec![Origin::Unknown; 4];
        for i in &ir.instructions {
            transfer(&mut state, i);
        }
        assert_eq!(state[0], Origin::Unknown);
    }
    #[test]
    fn branch_prefix_is_explicitly_unsupported() {
        let ir = decode(vec![0x0029, 2, 0x000e]);
        assert!(!linear(&ir.instructions[0]));
    }
    #[test]
    fn constructor_to_switch_returns_selected_raw_region() {
        use crate::native_dex::{DexField, DexSymbols};
        use std::sync::Arc;
        fn method(
            owner: &str,
            name: &str,
            parameters: Vec<Arc<str>>,
            registers: u16,
            ins: u16,
            words: Vec<u16>,
        ) -> DexMethod {
            DexMethod {
                declaring_type: owner.into(),
                name: name.into(),
                return_type: "V".into(),
                parameters,
                thrown_types: vec![],
                access_flags: 0,
                code: Some(DexCode {
                    registers,
                    ins,
                    outs: 2,
                    tries: 0,
                    try_regions: vec![],
                    instructions: words,
                    offset: 0,
                }),
            }
        }
        let symbols = Arc::new(DexSymbols {
            types: vec!["LLambda;".into(), "I".into()],
            strings: vec!["<init>".into(), "id".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            fields: vec![(0, 1, 1)],
            ..Default::default()
        });
        let ctor = method(
            "LLambda;",
            "<init>",
            vec!["I".into()],
            2,
            2,
            vec![0x0159, 0, 0x000e],
        );
        // iget v0,p0; packed-switch v0 payload@8; default return@5; case return@6.
        let dispatcher = method(
            "LLambda;",
            "run",
            vec![],
            2,
            1,
            vec![
                0x1052, 0, 0x002b, 6, 0, 0x000e, 0x000e, 0, 0x0100, 1, 1, 0, 4, 0,
            ],
        );
        let caller = method(
            "LLambda;",
            "factory",
            vec![],
            2,
            0,
            vec![0x0022, 0, 0x1112, 0x2070, 0, 0x0010, 0x000e],
        );
        let class = DexClass {
            symbols,
            descriptor: "LLambda;".into(),
            superclass: None,
            interfaces: vec![],
            access_flags: 0,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![DexField {
                declaring_type: "LLambda;".into(),
                name: "id".into(),
                field_type: "I".into(),
                access_flags: 0x10,
                is_static: false,
            }],
            methods: vec![ctor, dispatcher, caller],
        };
        let result = analyze(&[&class], "Lambda.factory()V", 3, &|| false);
        assert_eq!(result["status"], "resolved", "{result}");
        assert_eq!(result["dispatchers"][0]["selected_target_offset"], 6);
        assert_eq!(result["dispatchers"][0]["discriminator"], 1);
        assert!(
            result["dispatchers"][0]["blocks"][0]["instructions"][0]["text"]
                .as_str()
                .unwrap()
                .contains("return-void")
        );
        assert!(
            result["dispatchers"][0]["prelude_instructions"][0]["text"]
                .as_str()
                .unwrap()
                .contains("iget")
        );
        assert_eq!(
            result["dispatchers"][0]["prelude_instructions"][0]["offset"],
            0
        );
        assert_eq!(
            result["dispatchers"][0]["prelude_instructions"][1]["offset"],
            2
        );
        assert_eq!(
            analyze(&[&class], "Lambda.factory()V", 3, &|| true)["reason"],
            "analysis cancelled"
        );
        assert_eq!(result["dispatchers"][0]["blocks"][0]["start_offset"], 6);
        let mut class = class;
        class.methods[1].code.as_mut().unwrap().instructions = vec![
            0x1052, 0, 0x002b, 6, 0, 0x000e, 0x000e, 0, 0x0100, 2, 1, 0, 4, 0, 4, 0,
        ];
        let shared = analyze(&[&class], "Lambda.factory()V", 3, &|| false);
        assert_eq!(shared["status"], "resolved", "{shared}");
        assert_eq!(
            shared["dispatchers"][0]["blocks"][0]["shared_with_other_case"],
            true
        );
        class.methods[1].code.as_mut().unwrap().instructions[6] = 0xfc28; // later case jumps back to selector prelude.
        assert_eq!(
            analyze(&[&class], "Lambda.factory()V", 3, &|| false)["status"],
            "unresolved"
        );
        class.methods[1].code.as_mut().unwrap().instructions[6] = 0x000e;
        class.methods[2].code.as_mut().unwrap().instructions[2] = 0x010a; // move-result v1: unknown value.
        assert_eq!(
            analyze(&[&class], "Lambda.factory()V", 3, &|| false)["status"],
            "unresolved"
        );
    }
    #[test]
    fn closure_includes_exception_paths_and_honors_cancellation() {
        use crate::native_cfg::{BasicBlock, Edge};
        let cfg = ControlFlowGraph {
            blocks: vec![
                BasicBlock {
                    start: 0,
                    end: 1,
                    instructions: vec![0],
                    successors: vec![
                        Edge {
                            target: 1,
                            kind: EdgeKind::Normal,
                        },
                        Edge {
                            target: 2,
                            kind: EdgeKind::Exceptional,
                        },
                    ],
                },
                BasicBlock {
                    start: 1,
                    end: 2,
                    instructions: vec![1],
                    successors: vec![],
                },
                BasicBlock {
                    start: 2,
                    end: 3,
                    instructions: vec![2],
                    successors: vec![],
                },
            ],
            block_at: HashMap::from([(0, 0), (1, 1), (2, 2)]),
        };
        assert_eq!(
            closure(&cfg, &[0], true, &|| false).unwrap(),
            BTreeSet::from([0, 1, 2])
        );
        assert_eq!(
            closure(&cfg, &[0], false, &|| false).unwrap(),
            BTreeSet::from([0, 1])
        );
        assert!(
            closure(&cfg, &[0], true, &|| true)
                .unwrap_err()
                .contains("cancelled")
        );
    }
}
