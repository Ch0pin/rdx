//! A terminal DAG may lie inside a loop's address interval without belonging
//! to its natural cycle. Preserve its original edge and render it at that edge.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_dominators::DominatorTree,
};
use std::{collections::BTreeSet, ops::Range};

pub(crate) fn admits(code: &DexCode, interval: Range<usize>, from: usize, target: usize) -> bool {
    prove(code, interval, from, target).is_some()
}

fn prove(code: &DexCode, interval: Range<usize>, from: usize, target: usize) -> Option<()> {
    if code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() > 4096
        || !interval.contains(&from)
        || !interval.contains(&target)
        || target == interval.start
    {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 128 {
        return None;
    }
    let header = *cfg.block_at.get(&interval.start)?;
    let source = *cfg.block_at.get(&from)?;
    let destination = *cfg.block_at.get(&target)?;
    if cfg.blocks[header].start != interval.start
        || cfg.blocks[destination].start != target
        || cfg.blocks[source].instructions.last() != Some(&from)
        || !cfg.blocks[source]
            .successors
            .iter()
            .any(|e| e.target == destination && e.kind == EdgeKind::Normal)
        || cfg
            .blocks
            .iter()
            .flat_map(|b| &b.successors)
            .any(|e| e.kind != EdgeKind::Normal)
    {
        return None;
    }
    let forward: Vec<Vec<usize>> = cfg
        .blocks
        .iter()
        .map(|b| b.successors.iter().map(|e| e.target).collect())
        .collect();
    let mut reverse = vec![vec![]; cfg.blocks.len()];
    for (from, edges) in forward.iter().enumerate() {
        for &to in edges {
            reverse.get_mut(to)?.push(from);
        }
    }
    let down = reachable(header, &forward)?;
    let up = reachable(header, &reverse)?;
    let cycle: BTreeSet<_> = down.intersection(&up).copied().collect();
    if cycle.len() < 2
        || !cycle.contains(&source)
        || cycle.contains(&destination)
        || cycle
            .iter()
            .any(|&b| !interval.contains(&cfg.blocks[b].start))
    {
        return None;
    }
    let dom = DominatorTree::compute_with_work_limit(&cfg, 100_000).ok()?;
    if cycle.iter().any(|&b| !dom.dominates(header, b)) {
        return None;
    }
    let mut colors = vec![0u8; cfg.blocks.len()];
    let mut pending = vec![(destination, false)];
    let mut budget = 4096usize;
    while let Some((block, finished)) = pending.pop() {
        budget = budget.checked_sub(1)?;
        if cycle.contains(&block) {
            return None;
        }
        if finished {
            colors[block] = 2;
            continue;
        }
        match colors[block] {
            1 => return None,
            2 => continue,
            _ => {}
        }
        colors[block] = 1;
        pending.push((block, true));
        let b = &cfg.blocks[block];
        // Entering a pending call result or monitor boundary needs separate ownership.
        if matches!(*code.instructions.get(b.start)? as u8, 0x0a..=0x0d)
            || b.instructions
                .iter()
                .any(|&pc| matches!(code.instructions[pc] as u8, 0x1d | 0x1e))
        {
            return None;
        }
        if b.successors.is_empty() {
            if !matches!(
                *code.instructions.get(*b.instructions.last()?)? as u8,
                0x0e..=0x11 | 0x27
            ) {
                return None;
            }
        } else {
            pending.extend(b.successors.iter().map(|e| (e.target, false)));
        }
    }
    Some(())
}
fn reachable(start: usize, edges: &[Vec<usize>]) -> Option<BTreeSet<usize>> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![start];
    while let Some(block) = pending.pop() {
        if seen.insert(block) {
            pending.extend(edges.get(block)?.iter().copied());
        }
    }
    Some(seen)
}
