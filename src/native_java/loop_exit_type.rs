//! Exact, bounded narrow exit lifetime; never supplies an incoming value.
use crate::{
    native_ssa::{DefinitionKind, SsaMethod},
    native_types::{AssignmentBound, InferredTypes, TypeResolution},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ops::Range,
};

/// One original enclosing catch whose complete continuation cannot reach this
/// loop or its normal exit. Its original exceptional SSA values stay intact;
/// only the loop's normal-exit storage may acquire a distinct lifetime.
pub(crate) fn detached_handler_exit(
    code: &crate::native_dex::DexCode,
    range: Range<usize>,
    exit: usize,
) -> Option<usize> {
    use crate::native_cfg::{ControlFlowGraph, EdgeKind};
    if range.start >= range.end
        || range.end != exit
        || exit >= code.instructions.len()
        || code.instructions.len() > 4096
        || code.registers > 256
        || usize::from(code.tries) != code.try_regions.len()
        || code.try_regions.len() > 16
        || code
            .try_regions
            .iter()
            .map(|region| region.catches.len())
            .sum::<usize>()
            > 64
    {
        return None;
    }
    let mut owners = code
        .try_regions
        .iter()
        .filter(|region| region.start as usize <= range.start && range.end <= region.end as usize);
    let owner = owners.next()?;
    if owners.next().is_some() || owner.catches.len() != 1 {
        return None;
    }
    let handler = owner.catches[0].1 as usize;
    if handler <= exit {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 512
        || cfg.blocks[*cfg.block_at.get(&range.start)?].start != range.start
        || cfg.blocks[*cfg.block_at.get(&exit)?].start != exit
        || cfg.blocks[*cfg.block_at.get(&handler)?].start != handler
    {
        return None;
    }
    let mut found = false;
    for block in &cfg.blocks {
        if !block.instructions.iter().any(|pc| range.contains(pc)) {
            continue;
        }
        let source = *block.instructions.last()?;
        if !range.contains(&source) {
            return None;
        }
        for edge in &block.successors {
            if edge.kind != EdgeKind::Exceptional {
                continue;
            }
            if cfg.blocks[edge.target].start != handler
                || !(owner.start as usize <= source && source < owner.end as usize)
            {
                return None;
            }
            found = true;
        }
    }
    if !found {
        return None;
    }
    // Includes calls, captures, later catches, and ordinary continuation. A
    // handler with effects is admissible only when none of its actual normal
    // or exceptional paths can enter a loop instruction or its normal join.
    let mut pending = vec![*cfg.block_at.get(&handler)?];
    let mut visited = BTreeSet::new();
    let mut work = 16_384usize;
    while let Some(next) = pending.pop() {
        if work == 0 {
            return None;
        }
        work -= 1;
        if !visited.insert(next) {
            continue;
        }
        let block = cfg.blocks.get(next)?;
        if block.instructions.len() > work {
            return None;
        }
        work -= block.instructions.len();
        if block
            .instructions
            .iter()
            .any(|pc| range.contains(pc) || *pc == exit)
        {
            return None;
        }
        pending.extend(block.successors.iter().map(|edge| edge.target));
    }
    Some(handler)
}

/// Original exceptional entries whose complete nonthrowing, forward-only
/// continuations reach the exact exit. This changes no SSA edge or value.
pub(crate) fn pure_handler_exits(
    code: &crate::native_dex::DexCode,
    range: Range<usize>,
    exit: usize,
) -> Option<Vec<usize>> {
    use crate::native_cfg::{ControlFlowGraph, EdgeKind};
    if range.start >= range.end
        || range.end != exit
        || exit >= code.instructions.len()
        || code.instructions.len() > 4096
        || code.registers > 256
        || usize::from(code.tries) != code.try_regions.len()
        || code.try_regions.len() > 16
        || code
            .try_regions
            .iter()
            .map(|region| region.catches.len())
            .sum::<usize>()
            > 64
    {
        return None;
    }
    let ir = crate::native_ir::DecodedMethod::decode(code).ok()?;
    let at: BTreeMap<_, _> = ir
        .instructions
        .iter()
        .map(|instruction| (instruction.pc, instruction))
        .collect();
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 512 || !at.contains_key(&range.start) || !at.contains_key(&exit) {
        return None;
    }
    let mut handlers = BTreeSet::new();
    for block in &cfg.blocks {
        let source = *block.instructions.last()?;
        if !range.contains(&source) {
            continue;
        }
        for edge in &block.successors {
            if edge.kind != EdgeKind::Exceptional {
                continue;
            }
            let handler = cfg.blocks[edge.target].start;
            if !code.try_regions.iter().any(|region| {
                region.start as usize <= source
                    && source < region.end as usize
                    && region
                        .catches
                        .iter()
                        .any(|(_, entry)| *entry as usize == handler)
            }) {
                return None;
            }
            handlers.insert(handler);
        }
    }
    if handlers.len() > 16 {
        return None;
    }
    for &handler in &handlers {
        if !range.contains(&handler) {
            return None;
        }
        let mut pc = handler;
        let mut visited = BTreeSet::new();
        while pc != exit {
            if !range.contains(&pc) || !visited.insert(pc) || visited.len() > 32 {
                return None;
            }
            let instruction = at.get(&pc)?;
            if instruction.may_throw
                || instruction.payload_target.is_some()
                || !matches!(instruction.opcode, 0x00..=0x09 | 0x12..=0x19 | 0x28..=0x2a)
            {
                return None;
            }
            let next = instruction
                .branch_target
                .unwrap_or(instruction.pc + instruction.width);
            if next <= pc || next > exit {
                return None;
            }
            // Recheck canonical block successors, rather than treating a raw
            // opcode or a supplied handler address as proof of continuation.
            let block = &cfg.blocks[*cfg.block_at.get(&pc)?];
            let position = block.instructions.iter().position(|found| *found == pc)?;
            if let Some(&following) = block.instructions.get(position + 1) {
                if next != following {
                    return None;
                }
            } else if block.successors.len() != 1
                || block.successors[0].kind != EdgeKind::Normal
                || cfg.blocks[block.successors[0].target].start != next
            {
                return None;
            }
            pc = next;
        }
    }
    Some(handlers.into_iter().collect())
}

pub(crate) fn exit_type(
    ssa: &SsaMethod,
    types: &InferredTypes,
    range: Range<usize>,
    exit: usize,
    register: usize,
    detached_handler: Option<usize>,
    hierarchy: Option<&crate::native_hierarchy::TypeHierarchy>,
) -> Option<String> {
    exit_type_with_handler_exits(
        ssa,
        types,
        range,
        exit,
        register,
        detached_handler,
        &[],
        hierarchy,
    )
}

/// `handler_exits` must come from `pure_handler_exits` for the same immutable
/// method/range. Callers retain their original fresh-IR/call-binding check.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exit_type_with_handler_exits(
    ssa: &SsaMethod,
    types: &InferredTypes,
    range: Range<usize>,
    exit: usize,
    register: usize,
    detached_handler: Option<usize>,
    handler_exits: &[usize],
    hierarchy: Option<&crate::native_hierarchy::TypeHierarchy>,
) -> Option<String> {
    if range.start >= range.end
        || range.end != exit
        || ssa.definitions.len() > 32768
        || types.values.len() != ssa.definitions.len()
        || types.wide_pair_issues != 0
        || ssa.graph.blocks.len() > 16384
        || ssa.reachable.len() != ssa.graph.blocks.len()
        || handler_exits.len() > 16
        || handler_exits.windows(2).any(|pair| pair[0] >= pair[1])
        || handler_exits.iter().any(|handler| !range.contains(handler))
    {
        return None;
    }
    if ssa.graph.blocks.iter().any(|block| {
        range.contains(&block.start)
            && block.successors.iter().any(|edge| {
                edge.kind == crate::native_cfg::EdgeKind::Exceptional
                    && !(handler_exits.contains(&ssa.graph.blocks[edge.target].start)
                        || (detached_handler == Some(ssa.graph.blocks[edge.target].start)
                            && ssa.graph.blocks[edge.target].start >= range.end))
            })
    }) {
        return None;
    }
    let block = *ssa.graph.block_at.get(&exit)?;
    if ssa.graph.blocks.get(block)?.start != exit || ssa.reachable.get(block) != Some(&true) {
        return None;
    }
    let entry = *ssa.graph.block_at.get(&0)?;
    let mut work = 65536usize;
    let mut predecessors = vec![HashSet::new(); ssa.graph.blocks.len()];
    for (from, block) in ssa.graph.blocks.iter().enumerate() {
        if work == 0 {
            return None;
        }
        work -= 1;
        for edge in &block.successors {
            if work == 0 {
                return None;
            }
            work -= 1;
            predecessors.get_mut(edge.target)?.insert(from);
        }
    }
    let phis: HashMap<_, _> = ssa.phis.iter().map(|phi| (phi.result, phi)).collect();
    if phis.len() != ssa.phis.len() {
        return None;
    }
    let candidates: Vec<_> = ssa
        .phis
        .iter()
        .filter(|phi| phi.block == block && usize::from(phi.register) == register)
        .collect();
    let id = match candidates.as_slice() {
        [phi] if !phi.incoming.is_empty() => phi.result,
        [] => {
            let mut id = None;
            for instruction in ssa.instructions.iter().filter(|instruction| {
                ssa.graph.blocks[block]
                    .instructions
                    .contains(&instruction.pc)
            }) {
                let reads: Vec<_> = instruction
                    .reads
                    .iter()
                    .filter(|read| usize::from(read.register) == register)
                    .collect();
                if !reads.is_empty() {
                    if reads.len() != 1 || reads[0].words.len() != 1 {
                        return None;
                    }
                    id = Some(reads[0].words[0]);
                    break;
                }
                if instruction.writes.iter().any(|write| {
                    (usize::from(write.register)..usize::from(write.register) + write.words.len())
                        .contains(&register)
                }) {
                    break;
                }
            }
            id?
        }
        _ => return None,
    };
    if usize::from(ssa.definitions.get(id)?.register) != register {
        return None;
    }
    let value = types.values.get(id)?;
    let TypeResolution::Resolved(ty) = &value.resolution else {
        return None;
    };
    let reference = ty.starts_with('L') || ty.starts_with('[');
    if !(reference || matches!(ty.as_ref(), "I" | "B" | "C" | "S" | "Z" | "F")) {
        return None;
    }
    let domain = |id: usize| -> bool {
        let Some(value) = types.values.get(id) else {
            return false;
        };
        !value.bounds_truncated
            && !value.assignment.is_empty()
            && value.assignment.iter().all(|bound| match bound {
                AssignmentBound::Type(found) => found == ty,
                AssignmentBound::Literal { bits, wide } => {
                    !wide
                        && match ty.as_ref() {
                            "Z" => matches!(bits, 0 | 1),
                            "B" => (-128..=127).contains(bits),
                            "S" => (-32768..=32767).contains(bits),
                            "C" => (0..=65535).contains(bits),
                            "I" | "F" => (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(bits),
                            _ => reference && *bits == 0,
                        }
                }
                _ => false,
            })
    };
    if !domain(id)
        || !value.required_types.iter().all(|required| {
            required == ty
                || (reference
                    && (required.as_ref() == "Ljava/lang/Object;"
                        || hierarchy.is_some_and(|h| {
                            h.assignable(ty, required) == crate::native_hierarchy::Relation::Proven
                        })))
        })
    {
        return None;
    }
    let mut pending = vec![(id, true)];
    let mut seen = HashSet::new();
    let mut roots = 0;
    while let Some((id, initial)) = pending.pop() {
        if work == 0 {
            return None;
        }
        work -= 1;
        if !seen.insert(id) {
            continue;
        }
        if !domain(id) {
            return None;
        }
        let definition = ssa.definitions.get(id)?;
        match definition.kind {
            DefinitionKind::Instruction { pc, word: 0, block }
                if range.contains(&pc) && ssa.reachable.get(block) == Some(&true) =>
            {
                roots += 1
            }
            DefinitionKind::Phi { block }
                if (initial && block == ssa.graph.block_at[&exit])
                    || range.contains(&ssa.graph.blocks.get(block)?.start) =>
            {
                if ssa.reachable.get(block) != Some(&true) {
                    return None;
                }
                let phi = phis.get(&id)?;
                if phi.block != block
                    || phi.register != definition.register
                    || phi.incoming.is_empty()
                {
                    return None;
                }
                let mut listed = HashSet::new();
                let mut actual = HashSet::new();
                for &(from, incoming_id) in &phi.incoming {
                    if ssa.definitions.get(incoming_id)?.register != phi.register {
                        return None;
                    }
                    if work == 0 {
                        return None;
                    }
                    work -= 1;
                    if !listed.insert(from) {
                        return None;
                    }
                    match from {
                        None if block == entry => {
                            actual.insert(None);
                        }
                        Some(from) if predecessors[block].contains(&from) => {
                            if ssa.reachable.get(from) == Some(&true) {
                                actual.insert(Some(from));
                            }
                        }
                        _ => return None,
                    }
                }
                let mut expected: HashSet<_> = predecessors[block]
                    .iter()
                    .filter(|from| ssa.reachable.get(**from) == Some(&true))
                    .map(|from| Some(*from))
                    .collect();
                if block == entry {
                    expected.insert(None);
                }
                if actual != expected || actual.is_empty() {
                    return None;
                }
                let mut incoming = 0;
                for &(from, id) in &phi.incoming {
                    if from.is_some_and(|block| ssa.reachable.get(block) != Some(&true)) {
                        continue;
                    }
                    if work == 0 {
                        return None;
                    }
                    work -= 1;
                    incoming += 1;
                    pending.push((id, false));
                }
                if incoming == 0 {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (roots != 0).then(|| ty.to_string())
}

#[cfg(test)]
mod tests {
    use super::{detached_handler_exit, pure_handler_exits};
    use crate::native_dex::{DexCode, DexTryRegion};

    fn protected_exit() -> DexCode {
        DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 1,
            offset: 0,
            instructions: vec![0x0012, 0x0071, 0, 0, 0x0328, 0x0012, 0x0128, 0x0011],
            try_regions: vec![DexTryRegion {
                start: 1,
                end: 4,
                catches: vec![(None, 5)].into(),
            }],
        }
    }

    fn detached_catch() -> DexCode {
        DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 1,
            offset: 0,
            instructions: vec![
                0x0012, 0x0071, 0, 0, 0x0128, 0x000e, 0, // protected call; normal return
                0x000d, 0x0071, 0, 0, 0x000e, // captured exception; effectful catch; return
            ],
            try_regions: vec![DexTryRegion {
                start: 1,
                end: 5,
                catches: vec![(None, 7)].into(),
            }],
        }
    }

    #[test]
    fn detached_original_catch_keeps_capture_and_effectful_continuation() {
        let code = detached_catch();
        assert_eq!(detached_handler_exit(&code, 1..5, 5), Some(7));
        assert!(detached_handler_exit(&code, 1..4, 5).is_none());
        let mut changed = detached_catch();
        changed.try_regions[0].end = 4;
        assert!(detached_handler_exit(&changed, 1..5, 5).is_none());
        let mut changed = detached_catch();
        changed.tries = 2;
        assert!(detached_handler_exit(&changed, 1..5, 5).is_none());
    }

    #[test]
    fn detached_catch_normal_and_exceptional_reentry_decline() {
        for target in [1isize, 5] {
            let mut code = detached_catch();
            code.instructions[11] = (((target - 11) as i8 as u8 as u16) << 8) | 0x28;
            assert!(
                detached_handler_exit(&code, 1..5, 5).is_none(),
                "normal reentry target={target}"
            );
        }
        let mut code = detached_catch();
        code.tries = 2;
        code.try_regions.push(DexTryRegion {
            start: 8,
            end: 11,
            catches: vec![(None, 1)].into(),
        });
        assert!(detached_handler_exit(&code, 1..5, 5).is_none());
    }

    #[test]
    fn pure_handler_permission_preserves_original_entry_and_exact_exit() {
        let code = protected_exit();
        assert_eq!(pure_handler_exits(&code, 1..7, 7), Some(vec![5]));
        assert!(pure_handler_exits(&code, 1..6, 7).is_none());
        let mut code = protected_exit();
        code.tries = 2;
        assert!(pure_handler_exits(&code, 1..7, 7).is_none());
        let mut code = protected_exit();
        code.try_regions[0].catches = vec![(None, 1)].into();
        assert!(pure_handler_exits(&code, 1..7, 7).is_none());
    }

    #[test]
    fn handler_capture_throw_effect_and_reentry_never_authorize_lifetime_split() {
        for opcode in [0x000d, 0x0027, 0x001d, 0x001a] {
            let mut code = protected_exit();
            code.instructions[5] = opcode;
            assert!(
                pure_handler_exits(&code, 1..7, 7).is_none(),
                "opcode={opcode:04x}"
            );
        }
        for target in [0isize, 1, 5] {
            let mut code = protected_exit();
            code.instructions[6] = (((target - 6) as i8 as u8 as u16) << 8) | 0x28;
            assert!(
                pure_handler_exits(&code, 1..7, 7).is_none(),
                "target={target}"
            );
        }
    }
}
