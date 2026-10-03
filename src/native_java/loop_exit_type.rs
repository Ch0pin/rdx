//! Exact, bounded narrow exit lifetime; never supplies an incoming value.
use crate::{
    native_ssa::{DefinitionKind, SsaMethod},
    native_types::{AssignmentBound, InferredTypes, TypeResolution},
};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

pub(crate) fn exit_type(
    ssa: &SsaMethod,
    types: &InferredTypes,
    range: Range<usize>,
    exit: usize,
    register: usize,
) -> Option<String> {
    if range.start >= range.end
        || range.end != exit
        || ssa.definitions.len() > 32768
        || types.values.len() != ssa.definitions.len()
        || types.wide_pair_issues != 0
        || ssa.graph.blocks.len() > 16384
        || ssa.reachable.len() != ssa.graph.blocks.len()
    {
        return None;
    }
    if ssa.graph.blocks.iter().any(|block| {
        range.contains(&block.start)
            && block
                .successors
                .iter()
                .any(|edge| edge.kind == crate::native_cfg::EdgeKind::Exceptional)
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
            required == ty || (reference && required.as_ref() == "Ljava/lang/Object;")
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
