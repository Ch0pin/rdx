//! Closed single-entry switch iterations with case tails after the direct latch.
//! Address order never grants ownership: every normal edge and every cycle is
//! checked against the original decoded CFG before legacy rendering is enabled.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_dominators::DominatorTree,
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub header: usize,
    pub guard: usize,
    pub latch: usize,
    pub exit: usize,
    pub switch: usize,
    pub owned: BTreeSet<usize>,
    pub edges: BTreeSet<(usize, usize)>,
}

pub(super) fn select(code: &DexCode) -> Option<Plan> {
    if code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() > 4096
        || code.registers > 256
        || code.ins > code.registers
    {
        return None;
    }
    // Operand coincidences can admit analysis, never authorize ownership.
    if !code
        .instructions
        .iter()
        .any(|word| matches!(*word as u8, 0x2b | 0x2c))
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let at: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let switches: Vec<_> = ir
        .instructions
        .iter()
        .filter(|i| matches!(i.opcode, 0x2b | 0x2c))
        .collect();
    if switches.len() != 1
        || ir
            .instructions
            .iter()
            .any(|i| matches!(i.opcode, 0x1d | 0x1e))
    {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 128 {
        return None;
    }
    let dom = DominatorTree::compute_with_work_limit(&cfg, 100_000).ok()?;
    let mut selected = None;
    for (hb, h) in cfg.blocks.iter().enumerate() {
        let header = h.start;
        let guard = *h.instructions.last()?;
        let gi = at.get(&guard)?;
        if !matches!(gi.opcode, 0x32..=0x3d) {
            continue;
        }
        let Some(exit) = gi.branch_target.filter(|e| *e > guard) else {
            continue;
        };
        let switch = switches[0].pc;
        if !(guard < switch && switch < exit) {
            continue;
        }
        let blocks: BTreeSet<_> = cfg
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| header <= b.start && b.start < exit)
            .map(|(b, _)| b)
            .collect();
        let owned: BTreeSet<_> = blocks
            .iter()
            .flat_map(|b| cfg.blocks[*b].instructions.iter().copied())
            .collect();
        if owned.len() > 512 || !owned.contains(&switch) {
            continue;
        }
        let mut edges = BTreeSet::new();
        let mut dag: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        let mut feedback = Vec::new();
        let mut valid = true;
        for (from, b) in cfg.blocks.iter().enumerate() {
            if blocks.contains(&from) && !dom.dominates(hb, from) {
                valid = false;
                break;
            }
            for e in &b.successors {
                if e.kind != EdgeKind::Normal {
                    valid = false;
                    break;
                }
                let pc = *b.instructions.last()?;
                let target = cfg.blocks[e.target].start;
                if blocks.contains(&from) {
                    edges.insert((pc, target));
                    if e.target == hb {
                        if !matches!(at[&pc].opcode, 0x28..=0x2a)
                            || at[&pc].branch_target != Some(header)
                        {
                            valid = false;
                            break;
                        }
                        feedback.push(pc);
                    } else if blocks.contains(&e.target) {
                        dag.entry(from).or_default().push(e.target);
                    } else if pc != guard || target != exit {
                        valid = false;
                        break;
                    }
                } else if blocks.contains(&e.target) && e.target != hb {
                    valid = false;
                    break;
                }
            }
            if !valid {
                break;
            }
        }
        let Some(&latch) = feedback.iter().max() else {
            continue;
        };
        if !valid || feedback.len() > 32 || latch + at[&latch].width >= exit {
            continue;
        }
        // Removing only genuine header feedback must leave the whole iteration
        // acyclic. Shared backwards copy prefixes are then safe to emit inline.
        let mut incoming: BTreeMap<_, usize> = blocks.iter().map(|b| (*b, 0)).collect();
        for next in dag.values() {
            for to in next {
                *incoming.get_mut(to)? += 1;
            }
        }
        let mut ready: Vec<_> = incoming
            .iter()
            .filter_map(|(b, n)| (*n == 0).then_some(*b))
            .collect();
        let mut count = 0;
        while let Some(b) = ready.pop() {
            count += 1;
            for to in dag.get(&b).into_iter().flatten() {
                let n = incoming.get_mut(to)?;
                *n -= 1;
                if *n == 0 {
                    ready.push(*to);
                }
            }
        }
        if count != blocks.len() {
            continue;
        }
        // Every read in the whole iteration, including late case tails, must
        // have complete SSA input provenance. Unknown entry state cannot gain
        // a Java local merely because the normal-flow graph is reducible.
        let ssa = SsaMethod::build_with_work_limit(code, &ir, &cfg, 100_000).ok()?;
        let reads: BTreeMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
        let phis: BTreeMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
        let mut todo: Vec<_> = owned
            .iter()
            .flat_map(|p| reads[p].reads.iter().flat_map(|r| r.words.iter().copied()))
            .collect();
        let mut seen = BTreeSet::new();
        let mut work = 0;
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
        // Full immutable code is redecoded at emission; this certificate grants
        // no permission to alter original instruction targets or operands.
        let plan = Plan {
            header,
            guard,
            latch,
            exit,
            switch,
            owned,
            edges,
        };
        if selected.replace(plan).is_some() {
            return None;
        }
    }
    selected
}
