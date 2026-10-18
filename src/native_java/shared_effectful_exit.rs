//! A shared acyclic exit DAG lies outside the actual natural-loop members.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_dominators::DominatorTree,
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SharedEffectfulExit {
    pub from: usize,
    pub target: usize,
    pub header: usize,
    pub latch: usize,
    pub exit: usize,
    pub loop_members: Vec<usize>,
    pub instructions: Vec<usize>,
    pub edges: Vec<(usize, usize)>,
    pub incoming_edges: Vec<(usize, usize)>,
    pub live_in_registers: Vec<u16>,
    pub raw_words: Vec<u16>,
    pub widths: Vec<(usize, usize)>,
    pub targets: Vec<(usize, Option<usize>)>,
    pub registers: u16,
    pub ins: u16,
    pub outs: u16,
}
pub(super) fn validate(code: &DexCode, plan: &SharedEffectfulExit) -> bool {
    prove(
        code,
        plan.from,
        plan.target,
        plan.header,
        plan.latch,
        plan.exit,
    )
    .as_ref()
        == Some(plan)
}
pub(super) fn prove(
    code: &DexCode,
    from: usize,
    target: usize,
    header: usize,
    latch: usize,
    exit: usize,
) -> Option<SharedEffectfulExit> {
    // Only external address entries are admitted by this specialization;
    // canonical membership below still establishes actual cyclic ownership.
    if header >= code.instructions.len()
        || latch >= code.instructions.len()
        || exit >= code.instructions.len()
        || from >= code.instructions.len()
        || (header..=latch).contains(&from)
        || !matches!(code.instructions[from] as u8, 0x28..=0x2a | 0x32..=0x3d)
    {
        return None;
    }
    if code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() > 4096
        || target <= header
        || target > latch
        || exit <= latch
        || from == target
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let by_pc: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let source = by_pc.get(&from)?;
    if !matches!(source.opcode,0x28..=0x2a|0x32..=0x3d) || source.branch_target != Some(target) {
        return None;
    }
    let latch_ins = by_pc.get(&latch)?;
    if !matches!(latch_ins.opcode, 0x28..=0x2a) || latch_ins.branch_target != Some(header) {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 2048 {
        return None;
    }
    let dominators = DominatorTree::compute(&cfg).ok()?;
    let h = *cfg.block_at.get(&header)?;
    let l = *cfg.block_at.get(&latch)?;
    if !dominators.dominates(h, l) {
        return None;
    }
    let mut members = BTreeSet::from([h, l]);
    let mut pending = vec![l];
    while let Some(b) = pending.pop() {
        for &p in &dominators.predecessors[b] {
            if p >= cfg.blocks.len() {
                return None;
            }
            if members.insert(p) && p != h {
                pending.push(p);
            }
        }
    }
    if members.iter().any(|&b| !dominators.dominates(h, b))
        || members.contains(cfg.block_at.get(&target)?)
        || members.contains(cfg.block_at.get(&from)?)
    {
        return None;
    }
    let mut normal = BTreeMap::new();
    for block in &cfg.blocks {
        if block
            .successors
            .iter()
            .any(|e| e.kind == EdgeKind::Exceptional)
        {
            return None;
        }
        for (index, &pc) in block.instructions.iter().enumerate() {
            let mut next = if let Some(&n) = block.instructions.get(index + 1) {
                vec![n]
            } else {
                block
                    .successors
                    .iter()
                    .filter(|e| e.kind == EdgeKind::Normal)
                    .map(|e| cfg.blocks[e.target].start)
                    .collect()
            };
            next.sort_unstable();
            next.dedup();
            normal.insert(pc, next);
        }
    }
    // Reaching the common continuation must never reenter this natural loop.
    let mut continuation = vec![*cfg.block_at.get(&exit)?];
    let mut visited = BTreeSet::new();
    while let Some(block) = continuation.pop() {
        if members.contains(&block) {
            return None;
        }
        if !visited.insert(block) {
            continue;
        }
        continuation.extend(
            cfg.blocks[block]
                .successors
                .iter()
                .filter(|e| e.kind == EdgeKind::Normal)
                .map(|e| e.target),
        );
    }
    if !normal.get(&from)?.contains(&target) {
        return None;
    }
    // No admission based on address interval alone: every DAG node is outside
    // canonical natural-loop membership and every actual successor is recorded.
    let mut nodes = BTreeSet::new();
    let mut pending = vec![target];
    let mut edges = vec![];
    while let Some(pc) = pending.pop() {
        if pc == exit {
            continue;
        }
        if !nodes.insert(pc) {
            continue;
        }
        if nodes.len() > 64 || !(header + 1..=latch).contains(&pc) {
            return None;
        }
        if members.contains(cfg.block_at.get(&pc)?) {
            return None;
        }
        let ins = by_pc.get(&pc)?;
        if !matches!(ins.opcode,0x00..=0x0c|0x12..=0x1b|0x1f..=0x21|0x28..=0x2a|0x2d..=0x3d|0x44..=0x78|0x7b..=0xe2)
        {
            return None;
        }
        // Allocation, monitors, move-exception, payload/switch and terminal leaves
        // have separate state/ownership contracts and are excluded above.
        let next = normal.get(&pc)?;
        if next.is_empty() {
            return None;
        }
        for &to in next {
            if to <= pc {
                return None;
            }
            edges.push((pc, to));
            pending.push(to);
        }
    }
    let mut incoming_edges = vec![];
    for (&pc, next) in &normal {
        if !nodes.contains(&pc) {
            for &to in next {
                if nodes.contains(&to) {
                    incoming_edges.push((pc, to));
                }
            }
        }
    }
    // Moving into a move-result can consume another owner's pending invocation.
    if incoming_edges
        .iter()
        .any(|(_, to)| matches!(by_pc[to].opcode, 0x0a..=0x0c))
    {
        return None;
    }
    let ssa = SsaMethod::build(code, &ir, &cfg).ok()?;
    let instructions: BTreeMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    let phis: BTreeMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    let mut live = BTreeSet::new();
    for instruction in ssa.instructions.iter().filter(|i| nodes.contains(&i.pc)) {
        for read in &instruction.reads {
            let mut pending = read.words.clone();
            let mut seen = BTreeSet::new();
            while let Some(value) = pending.pop() {
                if !seen.insert(value) {
                    continue;
                }
                if seen.len() > 256 {
                    return None;
                }
                match ssa.definitions.get(value)?.kind {
                    DefinitionKind::Undefined => return None,
                    DefinitionKind::Parameter => {
                        live.insert(read.register);
                    }
                    DefinitionKind::Instruction { pc, .. } => {
                        if !nodes.contains(&pc) {
                            live.insert(read.register);
                            pending.extend(
                                instructions
                                    .get(&pc)?
                                    .reads
                                    .iter()
                                    .flat_map(|r| r.words.iter().copied()),
                            );
                        }
                    }
                    DefinitionKind::Phi { .. } => {
                        pending.extend(phis.get(&value)?.incoming.iter().map(|(_, v)| *v))
                    }
                }
            }
        }
    }
    let ordered: Vec<_> = nodes.into_iter().collect();
    edges.sort_unstable();
    incoming_edges.sort_unstable();
    Some(SharedEffectfulExit {
        from,
        target,
        header,
        latch,
        exit,
        loop_members: members.iter().map(|&b| cfg.blocks[b].start).collect(),
        instructions: ordered.clone(),
        edges,
        incoming_edges,
        live_in_registers: live.into_iter().collect(),
        raw_words: code.instructions.clone(),
        widths: std::iter::once(from)
            .chain(ordered.iter().copied())
            .map(|pc| (pc, by_pc[&pc].width))
            .collect(),
        targets: std::iter::once(from)
            .chain(ordered.iter().copied())
            .map(|pc| (pc, by_pc[&pc].branch_target))
            .collect(),
        registers: code.registers,
        ins: code.ins,
        outs: code.outs,
    })
}
