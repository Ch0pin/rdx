//! Outer iteration with bounded natural inner loop, exact edge-specific copies.
//! Typed joins, constructor timing, and throw legality remain emission proofs.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_dominators::DominatorTree,
    native_ir::DecodedMethod,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Copy {
    pub from: usize,
    pub target: usize,
    pub instructions: Vec<usize>,
    pub header: Option<usize>,
    pub partial_latch: Option<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TerminalEdge {
    pub from: usize,
    pub target: usize,
    pub instructions: Vec<usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EdgeOwner {
    Original,
    HeaderContinue,
    EntryCopy,
    PrefixTerminal,
    Terminal,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NestedLoop {
    pub header: usize,
    pub latch: usize,
    pub instructions: Vec<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AllocationIsland {
    pub allocation: usize,
    pub initialize: usize,
    pub receiver: usize,
    pub instructions: Vec<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct IterationPlan {
    pub nested: Option<NestedLoop>,
    pub allocation_islands: Vec<AllocationIsland>,
    pub nested_ssa_reads: Vec<(usize, u16, Vec<usize>)>,
    pub terminal_copy_ssa_reads: Vec<(usize, u16, Vec<usize>)>,
    pub header: usize,
    pub latch: usize,
    pub members: Vec<usize>,
    pub iteration_nodes: Vec<usize>,
    pub copies: Vec<Copy>,
    pub terminal_edges: Vec<TerminalEdge>,
    pub continues: Vec<(usize, usize)>,
    pub widths: Vec<(usize, usize)>,
    pub targets: Vec<(usize, usize)>,
    pub edges: Vec<(usize, usize, EdgeOwner)>,
    pub words: Vec<u16>,
    pub registers: u16,
    pub ins: u16,
    pub outs: u16,
}
fn prove_terminal_copy_frames(
    code: &DexCode,
    decoded: &DecodedMethod,
    cfg: &ControlFlowGraph,
    owned: &BTreeSet<usize>,
) -> Option<Vec<(usize, u16, Vec<usize>)>> {
    use crate::native_ssa::{DefinitionKind, SsaMethod};
    if code.registers > 256 || code.ins > code.registers {
        return None;
    }
    let ssa = SsaMethod::build_with_work_limit(code, decoded, cfg, 100_000).ok()?;
    let instructions: BTreeMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    let phis: BTreeMap<_, _> = ssa.phis.iter().map(|phi| (phi.result, phi)).collect();
    let read_frames: Vec<_> = ssa
        .instructions
        .iter()
        .filter(|i| owned.contains(&i.pc))
        .flat_map(|i| {
            i.reads
                .iter()
                .map(|read| (i.pc, read.register, read.words.clone()))
        })
        .collect();
    let mut work = 100_000usize;
    let mut seen = BTreeSet::new();
    let mut pending: Vec<_> = read_frames
        .iter()
        .flat_map(|(_, _, words)| words.iter().copied())
        .collect();
    while let Some(value) = pending.pop() {
        work = work.checked_sub(1)?;
        if !seen.insert(value) {
            continue;
        }
        match &ssa.definitions.get(value)?.kind {
            DefinitionKind::Undefined => return None,
            DefinitionKind::Parameter => {}
            DefinitionKind::Instruction { pc, .. } => pending.extend(
                instructions
                    .get(pc)?
                    .reads
                    .iter()
                    .flat_map(|read| read.words.iter().copied()),
            ),
            DefinitionKind::Phi { .. } => {
                pending.extend(phis.get(&value)?.incoming.iter().map(|(_, value)| *value))
            }
        }
    }
    let mut pending_pairs: Vec<_> = ssa
        .instructions
        .iter()
        .filter(|i| owned.contains(&i.pc))
        .flat_map(|i| i.reads.iter())
        .filter_map(|read| match read.words[..] {
            [low, high] => Some((low, high)),
            _ => None,
        })
        .collect();
    let mut seen_pairs = BTreeSet::new();
    while let Some((low, high)) = pending_pairs.pop() {
        work = work.checked_sub(1)?;
        if !seen_pairs.insert((low, high)) {
            continue;
        }
        let low_def = ssa.definitions.get(low)?;
        let high_def = ssa.definitions.get(high)?;
        if low_def.register.checked_add(1)? != high_def.register {
            return None;
        }
        match (&low_def.kind, &high_def.kind) {
            (DefinitionKind::Parameter, DefinitionKind::Parameter) => {}
            (
                DefinitionKind::Instruction {
                    pc: low_pc,
                    word: 0,
                    ..
                },
                DefinitionKind::Instruction {
                    pc: high_pc,
                    word: 1,
                    ..
                },
            ) if low_pc == high_pc => {
                let source = instructions.get(low_pc)?;
                if !source.writes.iter().any(|write| write.words == [low, high]) {
                    return None;
                }
                pending_pairs.extend(source.reads.iter().filter_map(|read| match read.words[..] {
                    [low, high] => Some((low, high)),
                    _ => None,
                }));
            }
            (
                DefinitionKind::Phi { block: low_block },
                DefinitionKind::Phi { block: high_block },
            ) if low_block == high_block => {
                let low_phi = phis.get(&low)?;
                let high_phi = phis.get(&high)?;
                if low_phi.incoming.len() != high_phi.incoming.len() {
                    return None;
                }
                for ((low_from, low_value), (high_from, high_value)) in
                    low_phi.incoming.iter().zip(&high_phi.incoming)
                {
                    if low_from != high_from {
                        return None;
                    }
                    pending_pairs.push((*low_value, *high_value));
                }
            }
            _ => return None,
        }
    }
    Some(read_frames)
}

pub(super) fn validate(code: &DexCode, plan: &IterationPlan) -> bool {
    select(code).as_ref() == Some(plan)
}

pub(super) fn select(code: &DexCode) -> Option<IterationPlan> {
    if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 4096 {
        return None;
    }
    let raw_target = |pc: usize, word: u16| -> Option<usize> {
        let offset = match word as u8 {
            0x28 => (word >> 8) as i8 as i64,
            0x29 | 0x32..=0x3d => *code.instructions.get(pc + 1)? as i16 as i64,
            0x2a => {
                (u32::from(*code.instructions.get(pc + 1)?)
                    | u32::from(*code.instructions.get(pc + 2)?) << 16) as i32
                    as i64
            }
            _ => return None,
        };
        usize::try_from(pc as i64 + offset)
            .ok()
            .filter(|target| *target < code.instructions.len())
    };
    // Preserve the existing unconditional gate. A conditional feedback edge
    // also needs a cheap possible exterior entry before IR/CFG decoding. Raw
    // operand coincidences can admit work; decoded edges alone authorize plans.
    let existing_gate = code.instructions.iter().enumerate().any(|(pc, &word)| {
        matches!(word as u8, 0x28..=0x2a) && raw_target(pc, word).is_some_and(|target| target < pc)
    });
    if !existing_gate
        && !code.instructions.iter().enumerate().any(|(pc, &word)| {
            if !matches!(word as u8, 0x32..=0x3d) {
                return false;
            }
            let Some(header) = raw_target(pc, word).filter(|target| *target < pc) else {
                return false;
            };
            code.instructions[..header]
                .iter()
                .enumerate()
                .any(|(from, &prefix)| {
                    raw_target(from, prefix).is_some_and(|target| header < target && target <= pc)
                })
        })
    {
        return None;
    }
    let decoded = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 128 {
        return None;
    }
    let ins: BTreeMap<_, _> = decoded.instructions.iter().map(|i| (i.pc, i)).collect();
    let mut end = 0;
    for i in &decoded.instructions {
        if i.pc != end
            || i.payload_target.is_some()
            || matches!(i.opcode, 0x1d | 0x1e | 0x2b | 0x2c)
            || (i.opcode == 0 && code.instructions[i.pc] != 0)
        {
            return None;
        }
        end = i.pc.checked_add(i.width)?;
    }
    if end != code.instructions.len() {
        return None;
    }
    let mut edges = BTreeMap::new();
    for block in &cfg.blocks {
        for (index, &pc) in block.instructions.iter().enumerate() {
            let next = if let Some(&next) = block.instructions.get(index + 1) {
                if next != pc + ins[&pc].width {
                    return None;
                }
                vec![next]
            } else {
                if block.end != pc + ins[&pc].width
                    || block.successors.iter().any(|e| e.kind != EdgeKind::Normal)
                {
                    return None;
                }
                block
                    .successors
                    .iter()
                    .map(|e| cfg.blocks[e.target].start)
                    .collect()
            };
            edges.insert(pc, next);
        }
    }
    if edges.len() != ins.len()
        || reachable(0, &edges, &BTreeSet::new())? != ins.keys().copied().collect()
    {
        return None;
    }
    let backward: Vec<_> = edges
        .iter()
        .flat_map(|(&from, ts)| {
            ts.iter()
                .filter(move |&&to| to <= from)
                .map(move |&to| (from, to))
        })
        .collect();
    let backward = cyclic_backward_edges(&edges, backward)?;
    let headers: BTreeSet<_> = backward.iter().map(|(_, to)| *to).collect();
    if headers.is_empty() || headers.len() > 2 {
        return None;
    }
    let header = *headers.first()?;
    let nested_header = (headers.len() == 2).then(|| *headers.last().unwrap());
    if backward.is_empty() || backward.len() > 8 {
        return None;
    }
    let mut reverse: BTreeMap<_, Vec<_>> = ins.keys().map(|&pc| (pc, vec![])).collect();
    for (&from, ts) in &edges {
        for to in ts {
            reverse.get_mut(to)?.push(from);
        }
    }
    let nested = if let Some(inner) = nested_header {
        let inner_latches: Vec<_> = backward
            .iter()
            .filter(|(_, to)| *to == inner)
            .map(|(from, _)| *from)
            .collect();
        let outer_latches: Vec<_> = backward
            .iter()
            .filter(|(_, to)| *to == header)
            .map(|(from, _)| *from)
            .collect();
        let ([inner_latch], [outer_latch]) = (inner_latches.as_slice(), outer_latches.as_slice())
        else {
            return None;
        };
        if !(header < inner
            && inner < *inner_latch
            && *inner_latch < *outer_latch
            && matches!(ins[inner_latch].opcode, 0x32..=0x3d)
            && ins[inner_latch].branch_target == Some(inner))
        {
            return None;
        }
        let mut nodes = BTreeSet::from([inner]);
        let mut todo = vec![*inner_latch];
        while let Some(pc) = todo.pop() {
            if nodes.insert(pc) {
                todo.extend(&reverse[&pc]);
            }
        }
        if nodes.len() > 32
            || nodes.iter().any(|pc| *pc < inner || *pc > *inner_latch)
            || edges.iter().any(|(from, ts)| {
                !nodes.contains(from) && ts.iter().any(|to| *to != inner && nodes.contains(to))
            })
            || nodes.iter().any(|pc| {
                edges[pc]
                    .iter()
                    .any(|to| !nodes.contains(to) && *to <= *inner_latch)
            })
        {
            return None;
        }
        Some(NestedLoop {
            header: inner,
            latch: *inner_latch,
            instructions: nodes.into_iter().collect(),
        })
    } else {
        None
    };
    let nested_dag_edges = if let Some(inner) = &nested {
        let mut pruned = edges.clone();
        pruned
            .get_mut(&inner.latch)?
            .retain(|to| *to != inner.header);
        Some(pruned)
    } else {
        None
    };
    let dag_edges = nested_dag_edges.as_ref().unwrap_or(&edges);
    let iteration = reachable(header, &edges, &BTreeSet::new())?;
    let back = reachable(header, &reverse, &BTreeSet::new())?;
    let members: BTreeSet<_> = iteration.intersection(&back).copied().collect();
    let latch = *members.last()?;
    if members.first() != Some(&header)
        || members.len() < 2
        || iteration.len() > 256
        || !matches!(ins[&latch].opcode, 0x28..=0x2a | 0x32..=0x3d)
        || ins[&latch].branch_target != Some(header)
    {
        return None;
    }
    let dag_nodes: BTreeSet<_> = iteration
        .iter()
        .copied()
        .filter(|pc| *pc != header)
        .collect();
    if !dag(&dag_nodes, dag_edges) {
        return None;
    }
    if iteration
        .iter()
        .any(|pc| edges[pc].is_empty() && !matches!(ins[pc].opcode, 0x0e..=0x11 | 0x27))
    {
        return None;
    }
    if backward.iter().any(|(from, to)| {
        !members.contains(from)
            || (*to != header
                && nested
                    .as_ref()
                    .is_none_or(|n| n.header != *to || n.latch != *from))
            || !matches!(ins[from].opcode, 0x28..=0x2a | 0x32..=0x3d)
    }) {
        return None;
    }
    let mut copies = vec![];
    for (&from, ts) in &edges {
        if from >= header {
            continue;
        }
        for &to in ts {
            if to <= header {
                continue;
            }
            if members.contains(&to) {
                if !matches!(ins[&from].opcode, 0x28..=0x2a | 0x32..=0x3d) {
                    return None;
                }
                let nodes = reachable(to, &edges, &BTreeSet::from([header]))?;
                if nodes.len() > 64
                    || !nodes.is_subset(&iteration)
                    || !dag(&nodes, dag_edges)
                    || nodes.iter().any(|pc| matches!(ins[pc].opcode, 0x23..=0x26))
                {
                    return None;
                }
                if nested.as_ref().is_some_and(|n| {
                    n.instructions.iter().any(|pc| nodes.contains(pc))
                        && (!n.instructions.iter().all(|pc| nodes.contains(pc)) || to != n.header)
                }) {
                    return None;
                }
                let ends: Vec<_> = nodes
                    .iter()
                    .copied()
                    .filter(|pc| edges[pc].contains(&header))
                    .collect();
                let [copy_end] = ends[..] else {
                    return None;
                };
                if !matches!(ins[&copy_end].opcode, 0x28..=0x2a | 0x32..=0x3d)
                    || ins[&copy_end].branch_target != Some(header)
                {
                    return None;
                }
                // Terminal leaves in an unconditional entry copy need the
                // exact feedback instruction as the renderer boundary. The
                // CFG DAG, terminal, and projected dominance proofs above
                // establish ownership; this bound selects its emission fence.
                let partial = matches!(ins[&copy_end].opcode, 0x32..=0x3d);
                if !partial
                    && (!matches!(ins[&from].opcode, 0x28..=0x2a)
                        || (!nodes.is_subset(&members) && nodes.last() != Some(&copy_end)))
                {
                    return None;
                }
                copies.push(Copy {
                    from,
                    target: to,
                    instructions: nodes.into_iter().collect(),
                    header: Some(header),
                    partial_latch: matches!(ins[&copy_end].opcode, 0x32..=0x3d).then_some(copy_end),
                });
            } else {
                let nodes = reachable(to, &edges, &members)?;
                // An edge-specific terminal copy is a separate bounded region.
                // It cannot reenter any loop or jump backwards. New instances
                // require a closed initialization island proved below; other
                // allocation forms remain guarded. Each effect is emitted from
                // this edge's actual frame and ownership;
                // terminal copies never contribute a synthesized header value.
                if nodes.is_empty()
                    || nodes.len() > 64
                    || !dag(&nodes, dag_edges)
                    || nodes.iter().any(|pc| {
                        matches!(ins[pc].opcode, 0x23..=0x26)
                            || edges[pc]
                                .iter()
                                .any(|next| members.contains(next) || next <= pc)
                    })
                {
                    return None;
                }
                if nodes
                    .iter()
                    .any(|pc| edges[pc].is_empty() && !matches!(ins[pc].opcode, 0x0e..=0x11 | 0x27))
                {
                    return None;
                }
                copies.push(Copy {
                    from,
                    target: to,
                    instructions: nodes.into_iter().collect(),
                    header: None,
                    partial_latch: None,
                });
            }
        }
    }
    if copies.is_empty()
        || copies.len() > 8
        || copies.iter().map(|c| c.instructions.len()).sum::<usize>() > 256
    {
        return None;
    }
    let terminal_only = copies.iter().all(|copy| copy.header.is_none());
    let terminal_copy_ssa_reads = if terminal_only {
        if headers.len() != 1
            || !copies
                .iter()
                .any(|copy| header < copy.target && copy.target <= latch)
            || copies
                .iter()
                .any(|copy| copy.instructions.iter().any(|pc| members.contains(pc)))
            || edges.iter().any(|(from, successors)| {
                !members.contains(from)
                    && successors
                        .iter()
                        .any(|to| members.contains(to) && *to != header)
            })
        {
            return None;
        }
        let raw_dom = DominatorTree::compute_with_work_limit(&cfg, 100_000).ok()?;
        let header_block = cfg.block_at[&header];
        if members
            .iter()
            .any(|pc| !raw_dom.dominates(header_block, cfg.block_at[pc]))
        {
            return None;
        }
        let owned: BTreeSet<_> = copies
            .iter()
            .flat_map(|copy| copy.instructions.iter().copied())
            .collect();
        prove_terminal_copy_frames(code, &decoded, &cfg, &owned)?
    } else {
        Vec::new()
    };
    let allocations: BTreeSet<_> = copies
        .iter()
        .flat_map(|c| c.instructions.iter().copied())
        .filter(|pc| ins[pc].opcode == 0x22)
        .collect();
    let mut allocation_islands = vec![];
    if !allocations.is_empty() {
        let owned: BTreeSet<_> = copies
            .iter()
            .flat_map(|c| c.instructions.iter().copied())
            .collect();
        let ssa =
            crate::native_ssa::SsaMethod::build_with_work_limit(code, &decoded, &cfg, 100_000)
                .ok()?;
        let values: BTreeMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
        let phi_inputs: BTreeMap<_, Vec<_>> = ssa
            .phis
            .iter()
            .map(|p| (p.result, p.incoming.iter().map(|(_, id)| *id).collect()))
            .collect();
        let mut ancestry_work = 100_000usize;
        for &allocation in &allocations {
            let alloc = ins[&allocation];
            let [write] = alloc.writes.as_slice() else {
                return None;
            };
            let [receiver] = values[&allocation].writes.first()?.words.as_slice() else {
                return None;
            };
            let mut sequence = vec![allocation];
            let mut pc = allocation.checked_add(alloc.width)?;
            loop {
                let i = *ins.get(&pc)?;
                if !owned.contains(&pc)
                    || reverse[&pc].as_slice() != [*sequence.last()?]
                    || edges[sequence.last()?].as_slice() != [pc]
                {
                    return None;
                }
                sequence.push(pc);
                if matches!(i.opcode, 0x70 | 0x76) {
                    let call = values[&pc];
                    if i.reads.first()?.register != write.register
                        || call.reads.first()?.words.as_slice() != [*receiver]
                        || call
                            .reads
                            .iter()
                            .skip(1)
                            .any(|read| read.words.contains(receiver))
                    {
                        return None;
                    }
                    for read in call.reads.iter().skip(1) {
                        let mut seen = BTreeSet::new();
                        let mut todo = read.words.clone();
                        while let Some(id) = todo.pop() {
                            ancestry_work = ancestry_work.checked_sub(1)?;
                            if seen.insert(id) {
                                match &ssa.definitions.get(id)?.kind {
                                    crate::native_ssa::DefinitionKind::Undefined => return None,
                                    crate::native_ssa::DefinitionKind::Phi { .. } => {
                                        todo.extend(phi_inputs.get(&id)?.iter().copied())
                                    }
                                    crate::native_ssa::DefinitionKind::Instruction {
                                        pc, ..
                                    } => todo.extend(
                                        values
                                            .get(pc)?
                                            .reads
                                            .iter()
                                            .flat_map(|operand| operand.words.iter().copied()),
                                    ),
                                    crate::native_ssa::DefinitionKind::Parameter => {}
                                }
                            }
                        }
                    }
                    allocation_islands.push(AllocationIsland {
                        allocation,
                        initialize: pc,
                        receiver: *receiver,
                        instructions: sequence,
                    });
                    break;
                }
                if sequence.len() > 5
                    || !matches!(i.opcode, 0x12..=0x1b)
                    || !i.reads.is_empty()
                    || i.writes.iter().any(|w| w.register == write.register)
                    || i.branch_target.is_some()
                    || i.payload_target.is_some()
                {
                    return None;
                }
                pc = pc.checked_add(i.width)?;
            }
        }
        if allocation_islands.len() > 8 {
            return None;
        }
    }
    for i in &decoded.instructions {
        if matches!(i.opcode, 0x0a..=0x0c) {
            let [before] = reverse[&i.pc][..] else {
                return None;
            };
            if before + ins[&before].width != i.pc
                || !matches!(ins[&before].opcode, 0x6e..=0x72 | 0x74..=0x78)
                || copies.iter().any(|c| c.target == i.pc)
            {
                return None;
            }
        }
    }
    let mut terminal_edges = vec![];
    for &from in &members {
        for &to in &edges[&from] {
            if members.contains(&to) {
                continue;
            }
            let nodes = reachable(to, &edges, &members)?;
            if nodes.is_empty()
                || nodes.len() > 64
                || !dag(&nodes, dag_edges)
                || nodes
                    .iter()
                    .any(|pc| edges[pc].iter().any(|next| members.contains(next)))
            {
                return None;
            }
            terminal_edges.push(TerminalEdge {
                from,
                target: to,
                instructions: nodes.into_iter().collect(),
            });
        }
    }
    if terminal_edges.len() > 16
        || terminal_edges
            .iter()
            .map(|t| t.instructions.len())
            .sum::<usize>()
            > 512
    {
        return None;
    }
    let prefix_terminals: BTreeSet<_> = copies
        .iter()
        .filter(|c| c.header.is_none())
        .flat_map(|c| c.instructions.iter().copied())
        .collect();
    if ins
        .keys()
        .any(|pc| *pc >= header && !iteration.contains(pc) && !prefix_terminals.contains(pc))
    {
        return None;
    }
    let mut projected = cfg.clone();
    let header_block = *cfg.block_at.get(&header)?;
    for copy in &copies {
        let source = *cfg.block_at.get(&copy.from)?;
        if cfg.blocks[source].instructions.last() != Some(&copy.from) {
            return None;
        }
        if copy.header.is_some() {
            let old_target = *cfg.block_at.get(&copy.target)?;
            let mut replaced = 0;
            for edge in &mut projected.blocks[source].successors {
                if edge.target == old_target {
                    edge.target = header_block;
                    replaced += 1;
                }
            }
            if replaced != 1 {
                return None;
            }
        } else {
            let target = *cfg.block_at.get(&copy.target)?;
            projected.blocks[source]
                .successors
                .retain(|e| e.target != target);
        }
    }
    let dom = DominatorTree::compute_with_work_limit(&projected, 100_000).ok()?;
    if iteration
        .iter()
        .any(|pc| !dom.dominates(header_block, cfg.block_at[pc]))
    {
        return None;
    }
    let mut nested_ssa_reads = vec![];
    if let Some(inner) = &nested {
        let inner_block = cfg.block_at[&inner.header];
        if inner
            .instructions
            .iter()
            .any(|pc| !dom.dominates(inner_block, cfg.block_at[pc]))
        {
            return None;
        }
        if decoded.instructions.iter().any(|i| {
            i.reads
                .iter()
                .chain(&i.writes)
                .any(|operand| operand.kind.word_count() != 1)
        }) {
            return None;
        }
        let ssa =
            crate::native_ssa::SsaMethod::build_with_work_limit(code, &decoded, &cfg, 100_000)
                .ok()?;
        let instruction_reads: BTreeMap<_, Vec<_>> = ssa
            .instructions
            .iter()
            .map(|i| {
                (
                    i.pc,
                    i.reads
                        .iter()
                        .flat_map(|operand| operand.words.iter().copied())
                        .collect(),
                )
            })
            .collect();
        let phi_inputs: BTreeMap<_, Vec<_>> = ssa
            .phis
            .iter()
            .map(|phi| (phi.result, phi.incoming.iter().map(|(_, id)| *id).collect()))
            .collect();
        let owned: BTreeSet<_> = copies
            .iter()
            .flat_map(|c| c.instructions.iter().copied())
            .collect();
        let mut ancestry_work = 100_000usize;
        for instruction in &ssa.instructions {
            if !owned.contains(&instruction.pc) {
                continue;
            }
            for read in &instruction.reads {
                let mut seen = BTreeSet::new();
                let mut todo = read.words.clone();
                while let Some(id) = todo.pop() {
                    ancestry_work = ancestry_work.checked_sub(1)?;
                    if seen.insert(id) {
                        match &ssa.definitions.get(id)?.kind {
                            crate::native_ssa::DefinitionKind::Undefined => return None,
                            crate::native_ssa::DefinitionKind::Phi { .. } => {
                                todo.extend(phi_inputs.get(&id)?.iter().copied());
                            }
                            crate::native_ssa::DefinitionKind::Instruction { pc, .. } => {
                                todo.extend(instruction_reads.get(pc)?.iter().copied());
                            }
                            crate::native_ssa::DefinitionKind::Parameter => {}
                        }
                    }
                }
                nested_ssa_reads.push((instruction.pc, read.register, read.words.clone()));
            }
        }
    }
    let owned_edges = edges
        .iter()
        .flat_map(|(&from, ts)| {
            let copies = &copies;
            let members = &members;
            ts.iter().map(move |&to| {
                (
                    from,
                    to,
                    if let Some(copy) = copies.iter().find(|c| c.from == from && c.target == to) {
                        if copy.header.is_some() {
                            EdgeOwner::EntryCopy
                        } else {
                            EdgeOwner::PrefixTerminal
                        }
                    } else if members.contains(&from) && to == header {
                        EdgeOwner::HeaderContinue
                    } else if members.contains(&from) && !members.contains(&to) {
                        EdgeOwner::Terminal
                    } else {
                        EdgeOwner::Original
                    },
                )
            })
        })
        .collect();
    Some(IterationPlan {
        allocation_islands,
        nested,
        nested_ssa_reads,
        terminal_copy_ssa_reads,
        header,
        latch,
        members: members.into_iter().collect(),
        iteration_nodes: iteration.into_iter().collect(),
        copies,
        terminal_edges,
        continues: backward,
        widths: decoded
            .instructions
            .iter()
            .map(|i| (i.pc, i.width))
            .collect(),
        targets: decoded
            .instructions
            .iter()
            .filter_map(|i| i.branch_target.map(|to| (i.pc, to)))
            .collect(),
        edges: owned_edges,
        words: code.instructions.clone(),
        registers: code.registers,
        ins: code.ins,
        outs: code.outs,
    })
}

fn reachable(
    start: usize,
    edges: &BTreeMap<usize, Vec<usize>>,
    stop: &BTreeSet<usize>,
) -> Option<BTreeSet<usize>> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![start];
    while let Some(pc) = todo.pop() {
        if !stop.contains(&pc) && seen.insert(pc) {
            todo.extend(edges.get(&pc)?);
        }
    }
    Some(seen)
}
fn dag(nodes: &BTreeSet<usize>, edges: &BTreeMap<usize, Vec<usize>>) -> bool {
    let mut counts: BTreeMap<_, usize> = nodes.iter().map(|&pc| (pc, 0)).collect();
    for pc in nodes {
        for to in &edges[pc] {
            if let Some(count) = counts.get_mut(to) {
                *count += 1;
            }
        }
    }
    let mut ready: Vec<_> = counts
        .iter()
        .filter_map(|(&pc, &count)| (count == 0).then_some(pc))
        .collect();
    let mut visited = 0;
    while let Some(pc) = ready.pop() {
        visited += 1;
        for to in &edges[&pc] {
            if let Some(count) = counts.get_mut(to) {
                *count -= 1;
                if *count == 0 {
                    ready.push(*to);
                }
            }
        }
    }
    visited == nodes.len()
}

pub(super) fn cyclic_backward_edges(
    edges: &BTreeMap<usize, Vec<usize>>,
    backward: Vec<(usize, usize)>,
) -> Option<Vec<(usize, usize)>> {
    // A sole feedback candidate still undergoes the existing SCC ownership
    // proof below. Preserve that common path without a repeated traversal.
    if backward.len() <= 1 {
        return Some(backward);
    }
    if backward.len() > 16 {
        return None;
    }
    // Address order alone does not prove feedback. Keep every raw edge and
    // distinguish prefix transfers with bounded exact normal reachability.
    let mut work = 0usize;
    let mut cyclic = Vec::new();
    for (from, to) in backward {
        let mut seen = BTreeSet::new();
        let mut pending = vec![to];
        let mut returns = false;
        while let Some(pc) = pending.pop() {
            work = work.checked_add(1)?;
            if work > 65_536 {
                return None;
            }
            if pc == from {
                returns = true;
                break;
            }
            if seen.insert(pc) {
                pending.extend(edges.get(&pc)?);
            }
        }
        if returns {
            cyclic.push((from, to));
        }
    }
    Some(cyclic)
}
