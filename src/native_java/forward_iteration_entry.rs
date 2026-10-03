//! One feedback header, forward iteration DAG, exact edge-specific entry copies.
//! Typed joins, constructor timing, and throw legality remain emission proofs.
use crate::{
    native_cfg::{ControlFlowGraph, Edge, EdgeKind},
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
pub(super) struct IterationPlan {
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
pub(super) fn validate(code: &DexCode, plan: &IterationPlan) -> bool {
    select(code).as_ref() == Some(plan)
}

pub(super) fn select(code: &DexCode) -> Option<IterationPlan> {
    if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 4096 {
        return None;
    }
    if !code.instructions.iter().enumerate().any(|(pc, &word)| {
        let offset = match word as u8 {
            0x28 => Some((word >> 8) as i8 as i64),
            0x29 => code.instructions.get(pc + 1).map(|w| *w as i16 as i64),
            0x2a => code
                .instructions
                .get(pc + 1)
                .zip(code.instructions.get(pc + 2))
                .map(|(lo, hi)| (u32::from(*lo) | u32::from(*hi) << 16) as i32 as i64),
            _ => None,
        };
        offset.is_some_and(|off| off < 0 && pc as i64 + off >= 0)
    }) {
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
    let headers: BTreeSet<_> = backward.iter().map(|(_, to)| *to).collect();
    let [header] = headers.iter().copied().collect::<Vec<_>>()[..] else {
        return None;
    };
    if backward.is_empty() || backward.len() > 8 {
        return None;
    }
    let mut reverse: BTreeMap<_, Vec<_>> = ins.keys().map(|&pc| (pc, vec![])).collect();
    for (&from, ts) in &edges {
        for to in ts {
            reverse.get_mut(to)?.push(from);
        }
    }
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
    if !dag(&dag_nodes, &edges) {
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
            || *to != header
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
                if !matches!(ins[&from].opcode, 0x28..=0x2a) {
                    return None;
                }
                let nodes = reachable(to, &edges, &BTreeSet::from([header]))?;
                if nodes.len() > 32
                    || !nodes.is_subset(&members)
                    || !dag(&nodes, &edges)
                    || nodes.iter().any(|pc| matches!(ins[pc].opcode, 0x22..=0x26))
                {
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
                if !matches!(ins[&copy_end].opcode, 0x28..=0x2a)
                    || ins[&copy_end].branch_target != Some(header)
                {
                    return None;
                }
                copies.push(Copy {
                    from,
                    target: to,
                    instructions: nodes.into_iter().collect(),
                    header: Some(header),
                });
            } else {
                let nodes = reachable(to, &edges, &members)?;
                // An edge-specific terminal copy is a separate bounded region.
                // It cannot allocate, reenter any loop, or jump backwards. Each
                // effect is emitted from this edge's actual frame and ownership;
                // terminal copies never contribute a synthesized header value.
                if nodes.is_empty()
                    || nodes.len() > 32
                    || !dag(&nodes, &edges)
                    || nodes.iter().any(|pc| {
                        matches!(ins[pc].opcode, 0x22..=0x26)
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
                });
            }
        }
    }
    if copies.is_empty()
        || copies.len() > 8
        || !copies.iter().any(|c| c.header.is_some())
        || copies.iter().map(|c| c.instructions.len()).sum::<usize>() > 256
    {
        return None;
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
                || nodes.len() > 32
                || !dag(&nodes, &edges)
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
            projected.blocks[source].successors = vec![Edge {
                target: header_block,
                kind: EdgeKind::Normal,
            }];
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
