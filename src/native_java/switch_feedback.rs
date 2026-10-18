//! Exact raw ownership certificate for switch arms returning to their enclosing loop.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::{DexClass, DexMethod},
    native_dominators::DominatorTree,
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PhiFrame {
    pub register: u16,
    pub incoming: Vec<(Option<usize>, usize)>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    pub header: usize,
    pub switch: usize,
    pub latch: usize,
    pub edges: Vec<(usize, usize)>,
    pub owned: BTreeSet<usize>,
    pub phi_inputs: Vec<PhiFrame>,
    pub layout: Vec<(usize, usize, Option<usize>)>,
    pub switch_targets: Vec<usize>,
}
pub(super) fn prove(
    class: &DexClass,
    method: &DexMethod,
    header: usize,
    switch: usize,
    latch: usize,
) -> Option<Plan> {
    let code = method.code.as_ref()?;
    if code.registers > 256
        || code.ins > code.registers
        || !code.try_regions.is_empty()
        || code.instructions.len() > 4096
        || !(header < switch && switch < latch)
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let at: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    if !matches!(at.get(&switch)?.opcode, 0x2b | 0x2c)
        || !matches!(at.get(&latch)?.opcode, 0x28..=0x2a)
        || at[&latch].branch_target != Some(header)
    {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 128 {
        return None;
    }
    let dom = DominatorTree::compute_with_work_limit(&cfg, 100_000).ok()?;
    let hb = *cfg.block_at.get(&header)?;
    let sb = *cfg.block_at.get(&switch)?;
    let lb = *cfg.block_at.get(&latch)?;
    if cfg.blocks[hb].start != header
        || cfg.blocks[sb].instructions.last() != Some(&switch)
        || cfg.blocks[lb].instructions.last() != Some(&latch)
    {
        return None;
    }
    let guard = *cfg.blocks[hb].instructions.last()?;
    let exit = latch + at[&latch].width;
    if !matches!(at[&guard].opcode, 0x32..=0x3d) || at[&guard].branch_target != Some(exit) {
        return None;
    }
    let blocks: BTreeSet<_> = cfg
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| header <= b.start && b.start <= latch)
        .map(|(i, _)| i)
        .collect();
    let owned: BTreeSet<_> = blocks
        .iter()
        .flat_map(|b| cfg.blocks[*b].instructions.iter().copied())
        .collect();
    if owned.len() > 256
        || !owned.contains(&switch)
        || owned.iter().any(|p| {
            matches!(at[p].opcode, 0x1d | 0x1e) || (at[p].payload_target.is_some() && *p != switch)
        })
    {
        return None;
    }
    let mut edges = Vec::new();
    let mut dag = BTreeMap::new();
    for (from, b) in cfg.blocks.iter().enumerate() {
        for e in &b.successors {
            if e.kind != EdgeKind::Normal {
                return None;
            }
            if blocks.contains(&from) {
                if !dom.dominates(hb, from) {
                    return None;
                }
                let pc = *b.instructions.last()?;
                if e.target == hb {
                    if !matches!(at[&pc].opcode, 0x28..=0x2a)
                        || at[&pc].branch_target != Some(header)
                        || !dom.dominates(sb, from)
                    {
                        return None;
                    }
                    edges.push((pc, header));
                } else if blocks.contains(&e.target) {
                    dag.entry(from).or_insert_with(Vec::new).push(e.target);
                } else if pc != guard || cfg.blocks[e.target].start != exit {
                    return None;
                }
            } else if blocks.contains(&e.target) && e.target != hb {
                return None;
            }
        }
    }
    if !(2..=32).contains(&edges.len()) {
        return None;
    }
    // Full owned DAG, including terminal/default paths; no hidden child cycle.
    let mut colors = BTreeMap::new();
    let mut work = 0;
    for start in &blocks {
        let mut todo = vec![(*start, false)];
        while let Some((n, end)) = todo.pop() {
            work += 1;
            if work > 100_000 {
                return None;
            }
            if end {
                colors.insert(n, 2);
                continue;
            }
            match colors.get(&n) {
                Some(1) => return None,
                Some(2) => continue,
                _ => {}
            }
            colors.insert(n, 1);
            todo.push((n, true));
            todo.extend(dag.get(&n).into_iter().flatten().map(|t| (*t, false)));
        }
    }
    let ssa = SsaMethod::build_with_work_limit(code, &ir, &cfg, 100_000).ok()?;
    let reads: BTreeMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    let phis: BTreeMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    let mut todo: Vec<_> = owned
        .iter()
        .flat_map(|p| reads[p].reads.iter().flat_map(|r| r.words.iter().copied()))
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(id) = todo.pop() {
        work += 1;
        if work > 100_000 {
            return None;
        }
        if !seen.insert(id) {
            continue;
        }
        match &ssa.definitions.get(id)?.kind {
            DefinitionKind::Undefined => return None,
            DefinitionKind::Parameter => {}
            DefinitionKind::Instruction { pc, .. } => todo.extend(
                reads
                    .get(pc)?
                    .reads
                    .iter()
                    .flat_map(|r| r.words.iter().copied()),
            ),
            DefinitionKind::Phi { .. } => {
                todo.extend(phis.get(&id)?.incoming.iter().map(|(_, v)| *v))
            }
        }
    }
    let bound = crate::native_calls::BoundCalls::bind(code, &ir, &class.symbols).ok()?;
    let calls = crate::native_call_values::SsaCalls::bind(&bound, &ssa).ok()?;
    let types = crate::native_types::InferredTypes::infer_with_work_limit(
        method,
        &ir,
        &ssa,
        &calls,
        &class.symbols,
        100_000,
    )
    .ok()?;
    if types.wide_pair_issues != 0 {
        return None;
    }
    let phi_inputs = ssa
        .phis
        .iter()
        .filter(|p| p.block == hb)
        .map(|p| PhiFrame {
            register: p.register,
            incoming: p.incoming.clone(),
        })
        .collect();
    let layout = owned
        .iter()
        .map(|p| (*p, at[p].width, at[p].branch_target))
        .collect();
    let mut switch_targets: Vec<_> = cfg.blocks[sb]
        .successors
        .iter()
        .map(|edge| cfg.blocks[edge.target].start)
        .collect();
    switch_targets.sort_unstable();
    switch_targets.dedup();
    Some(Plan {
        header,
        switch,
        latch,
        edges,
        owned,
        phi_inputs,
        layout,
        switch_targets,
    })
}
