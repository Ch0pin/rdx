//! Type a literal-null exception snapshot from its handler's first actual
//! reads, including reference/null joins reached after the catch body.
use crate::{
    native_hierarchy::{Relation, TypeHierarchy},
    native_ssa::{DefinitionKind, SsaMethod},
    native_types::{AssignmentBound, InferredTypes, TypeResolution},
};
use std::collections::{HashMap, HashSet};
pub(crate) fn select(
    ssa: &SsaMethod,
    types: &InferredTypes,
    handler: usize,
    register: usize,
    hierarchy: Option<&TypeHierarchy>,
) -> Option<String> {
    if ssa.definitions.len() > 32768
        || ssa.graph.blocks.len() > 16384
        || types.values.len() != ssa.definitions.len()
        || ssa.reachable.len() != ssa.graph.blocks.len()
        || types.wide_pair_issues != 0
    {
        return None;
    }
    let block = *ssa.graph.block_at.get(&handler)?;
    if ssa.graph.blocks[block].start != handler || !ssa.reachable[block] {
        return None;
    }
    let instructions: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    if instructions.len() != ssa.instructions.len() {
        return None;
    }
    let mut pending = vec![block];
    let mut seen = HashSet::new();
    let mut candidate = None;
    let mut budget = 65536usize;
    while let Some(block) = pending.pop() {
        budget = budget.checked_sub(1)?;
        if !seen.insert(block) || !ssa.reachable[block] {
            continue;
        }
        let mut stopped = false;
        for pc in &ssa.graph.blocks[block].instructions {
            budget = budget.checked_sub(1)?;
            let instruction = instructions.get(pc)?;
            let reads: Vec<_> = instruction
                .reads
                .iter()
                .filter(|r| usize::from(r.register) == register)
                .collect();
            if let Some(first) = reads.first() {
                if first.words.len() != 1 || reads.iter().any(|r| r.words != first.words) {
                    return None;
                }
                let ty = reference_value(ssa, types, first.words[0], hierarchy)?;
                if candidate.as_ref().is_some_and(|old| old != &ty) {
                    return None;
                }
                candidate = Some(ty);
                stopped = true;
                break;
            }
            if instruction.writes.iter().any(|w| {
                (usize::from(w.register)..usize::from(w.register) + w.words.len())
                    .contains(&register)
            }) {
                stopped = true;
                break;
            }
        }
        if !stopped {
            pending.extend(ssa.graph.blocks[block].successors.iter().map(|e| e.target));
        }
    }
    candidate
}
/// Every reaching assignment is already compatible with the inferred Java
/// reference type; complete SSA phi coverage prevents an omitted predecessor.
pub(crate) fn reference_value(
    ssa: &SsaMethod,
    types: &InferredTypes,
    id: usize,
    hierarchy: Option<&TypeHierarchy>,
) -> Option<String> {
    let TypeResolution::Resolved(ty) = &types.values.get(id)?.resolution else {
        return None;
    };
    reference_value_as(ssa, types, id, ty, hierarchy)
}
pub(crate) fn reference_value_as(
    ssa: &SsaMethod,
    types: &InferredTypes,
    id: usize,
    ty: &str,
    hierarchy: Option<&TypeHierarchy>,
) -> Option<String> {
    if ssa.definitions.len() > 32768
        || ssa.graph.blocks.len() > 16384
        || types.values.len() != ssa.definitions.len()
        || ssa.reachable.len() != ssa.graph.blocks.len()
        || types.wide_pair_issues != 0
    {
        return None;
    }
    if !(ty.starts_with('L') || ty.starts_with('[')) {
        return None;
    }
    let domain = |id: usize| {
        types.values.get(id).is_some_and(|v| {
            !v.assignment_bounds_truncated
                && !v.assignment.is_empty()
                && v.assignment.iter().all(|b| match b {
                    AssignmentBound::Literal {
                        bits: 0,
                        wide: false,
                    } => true,
                    AssignmentBound::Type(actual) => {
                        (actual.starts_with('L') || actual.starts_with('['))
                            && (actual.as_ref() == ty
                                || ty == "Ljava/lang/Object;"
                                || hierarchy
                                    .is_some_and(|h| h.assignable(actual, ty) == Relation::Proven))
                    }
                    _ => false,
                })
        })
    };
    let phis: HashMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    if phis.len() != ssa.phis.len() {
        return None;
    }
    let mut predecessors = vec![HashSet::new(); ssa.graph.blocks.len()];
    let mut budget = 65536usize;
    for (from, b) in ssa.graph.blocks.iter().enumerate() {
        for edge in &b.successors {
            budget = budget.checked_sub(1)?;
            predecessors.get_mut(edge.target)?.insert(from);
        }
    }
    let entry = *ssa.graph.block_at.get(&0)?;
    let mut pending = vec![id];
    let mut seen = HashSet::new();
    let mut roots = 0;
    while let Some(id) = pending.pop() {
        budget = budget.checked_sub(1)?;
        if !seen.insert(id) {
            continue;
        }
        if !domain(id) {
            return None;
        }
        let d = ssa.definitions.get(id)?;
        match d.kind {
            DefinitionKind::Instruction { block, word: 0, .. }
                if ssa.reachable.get(block) == Some(&true) =>
            {
                roots += 1
            }
            DefinitionKind::Parameter => roots += 1,
            DefinitionKind::Phi { block } => {
                let p = phis.get(&id)?;
                if p.block != block
                    || p.register != d.register
                    || p.incoming.is_empty()
                    || ssa.reachable.get(block) != Some(&true)
                {
                    return None;
                }
                let mut actual = HashSet::new();
                let mut listed = HashSet::new();
                for &(from, value) in &p.incoming {
                    budget = budget.checked_sub(1)?;
                    if !listed.insert(from) || ssa.definitions.get(value)?.register != p.register {
                        return None;
                    }
                    match from {
                        None if block == entry => {
                            actual.insert(None);
                            pending.push(value);
                        }
                        Some(from) if predecessors[block].contains(&from) => {
                            if ssa.reachable[from] {
                                actual.insert(Some(from));
                                pending.push(value);
                            }
                        }
                        _ => return None,
                    }
                }
                let mut expected: HashSet<_> = predecessors[block]
                    .iter()
                    .copied()
                    .filter(|&b| ssa.reachable[b])
                    .map(Some)
                    .collect();
                if block == entry {
                    expected.insert(None);
                }
                if actual != expected || actual.is_empty() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (roots != 0).then(|| ty.to_string())
}
