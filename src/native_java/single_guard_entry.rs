//! Exact node splitting for one linear iteration with a terminal guard arm.
//! This proof owns original instruction paths; constructor validity and current
//! typed-frame joins remain independent renderer obligations.
use crate::{
    native_cfg::{ControlFlowGraph, Edge, EdgeKind},
    native_dex::DexCode,
    native_dominators::DominatorTree,
    native_ir::DecodedMethod,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EdgeOwner {
    Prefix,
    HeaderEntry,
    EntryCopy,
    Iteration,
    ReturnArm,
    Continue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SingleGuardPlan {
    pub header: usize,
    pub guard: usize,
    pub latch: usize,
    pub continuation: usize,
    pub terminal: usize,
    pub copy_from: usize,
    pub copy_target: usize,
    pub copy: Vec<usize>,
    pub header_path: Vec<usize>,
    pub continuation_path: Vec<usize>,
    pub terminal_path: Vec<usize>,
    pub widths: Vec<(usize, usize)>,
    pub targets: Vec<(usize, usize)>,
    pub edges: Vec<(usize, usize, EdgeOwner)>,
    // Include actual operands, register bounds, and invoke outs in reproof.
    pub words: Vec<u16>,
    pub registers: u16,
    pub ins: u16,
    pub outs: u16,
}

pub(super) fn validate(code: &DexCode, plan: &SingleGuardPlan) -> bool {
    select(code).as_ref() == Some(plan)
}

pub(super) fn select(code: &DexCode) -> Option<SingleGuardPlan> {
    if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 2048 {
        return None;
    }
    // An operand false positive only costs decoding; this is not authority.
    if !code.instructions.iter().enumerate().any(|(pc, &word)| {
        let delta = match word as u8 {
            0x28 => Some((word >> 8) as i8 as i64),
            0x29 => code
                .instructions
                .get(pc + 1)
                .map(|word| *word as i16 as i64),
            0x2a => code
                .instructions
                .get(pc + 1)
                .zip(code.instructions.get(pc + 2))
                .map(|(low, high)| (u32::from(*low) | (u32::from(*high) << 16)) as i32 as i64),
            _ => None,
        };
        delta.is_some_and(|offset| {
            offset < 0
                && (pc as i64)
                    .checked_add(offset)
                    .is_some_and(|target| target >= 0)
        })
    }) {
        return None;
    }
    let decoded = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 64 {
        return None;
    }
    let instructions: BTreeMap<_, _> = decoded.instructions.iter().map(|i| (i.pc, i)).collect();
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
            let i = instructions.get(&pc)?;
            let targets = if let Some(&next) = block.instructions.get(index + 1) {
                if next != pc + i.width {
                    return None;
                }
                vec![next]
            } else {
                if block.end != pc + i.width {
                    return None;
                }
                if block.successors.iter().any(|e| e.kind != EdgeKind::Normal) {
                    return None;
                }
                block
                    .successors
                    .iter()
                    .map(|e| cfg.blocks[e.target].start)
                    .collect()
            };
            edges.insert(pc, targets);
        }
    }
    if edges.len() != instructions.len()
        || reachable(0, &edges)? != instructions.keys().copied().collect()
    {
        return None;
    }
    let backwards: Vec<_> = edges
        .iter()
        .flat_map(|(&from, ts)| {
            ts.iter()
                .filter(move |&&to| to <= from)
                .map(move |&to| (from, to))
        })
        .collect();
    let [(latch, header)] = backwards.as_slice() else {
        return None;
    };
    let (latch, header) = (*latch, *header);
    if !matches!(instructions[&latch].opcode, 0x28..=0x2a)
        || instructions[&latch].branch_target != Some(header)
    {
        return None;
    }
    let mut reverse: BTreeMap<_, Vec<_>> = instructions.keys().map(|&pc| (pc, vec![])).collect();
    for (&from, ts) in &edges {
        for to in ts {
            reverse.get_mut(to)?.push(from);
        }
    }
    let forward = reachable(header, &edges)?;
    let back = reachable(header, &reverse)?;
    let members: BTreeSet<_> = forward.intersection(&back).copied().collect();
    if members.first() != Some(&header)
        || members.last() != Some(&latch)
        || members.len() > 128
        || members
            .iter()
            .any(|pc| matches!(instructions[pc].opcode, 0x22..=0x26))
    {
        return None;
    }
    let guards: Vec<_> = members
        .iter()
        .filter(|pc| edges[pc].len() == 2)
        .copied()
        .collect();
    let [guard] = guards.as_slice() else {
        return None;
    };
    let guard = *guard;
    if !matches!(instructions[&guard].opcode, 0x32..=0x3d) {
        return None;
    }
    let continuing: Vec<_> = edges[&guard]
        .iter()
        .filter(|pc| members.contains(pc))
        .copied()
        .collect();
    let returning: Vec<_> = edges[&guard]
        .iter()
        .filter(|pc| !members.contains(pc))
        .copied()
        .collect();
    let ([continuation], [terminal]) = (continuing.as_slice(), returning.as_slice()) else {
        return None;
    };
    let (continuation, terminal) = (*continuation, *terminal);
    let header_path = linear(header, Some(guard), &edges, &instructions, 128)?;
    let continuation_path = linear(continuation, Some(latch), &edges, &instructions, 128)?;
    let terminal_path = linear(terminal, None, &edges, &instructions, 16)?;
    if !matches!(instructions[terminal_path.last()?].opcode, 0x0e..=0x11) {
        return None;
    }
    let expected_members: BTreeSet<_> = header_path
        .iter()
        .chain(&continuation_path)
        .copied()
        .collect();
    if expected_members != members {
        return None;
    }
    let entry: Vec<_> = edges
        .iter()
        .filter(|(from, _)| !members.contains(from))
        .flat_map(|(&from, ts)| {
            ts.iter()
                .filter(|to| members.contains(to))
                .map(move |&to| (from, to))
        })
        .collect();
    let foreign: Vec<_> = entry
        .iter()
        .filter(|(_, to)| *to != header)
        .copied()
        .collect();
    let [(copy_from, copy_target)] = foreign.as_slice() else {
        return None;
    };
    let (copy_from, copy_target) = (*copy_from, *copy_target);
    if entry.len() != 2
        || !entry.iter().any(|(_, to)| *to == header)
        || copy_from >= header
        || !matches!(instructions[&copy_from].opcode, 0x28..=0x2a)
        || instructions[&copy_from].branch_target != Some(copy_target)
    {
        return None;
    }
    let copy = linear(copy_target, Some(latch), &edges, &instructions, 32)?;
    if copy.iter().any(|pc| !members.contains(pc) || *pc == guard) {
        return None;
    }
    // Copies never begin at a pending result; all original result atoms have
    // exactly their adjacent invoke producer as predecessor.
    for i in &decoded.instructions {
        if matches!(i.opcode, 0x0a..=0x0c) {
            let [producer] = reverse.get(&i.pc)?.as_slice() else {
                return None;
            };
            let before = instructions[producer];
            if before.pc + before.width != i.pc
                || !matches!(before.opcode, 0x6e..=0x72 | 0x74..=0x78)
                || copy_target == i.pc
            {
                return None;
            }
        }
    }
    let tail_members: BTreeSet<_> = terminal_path.iter().copied().collect();
    // Complete physical ownership: no tail holes, unrelated suffix, or entry
    // into the terminal constructor body can be hidden by selected metadata.
    if instructions
        .keys()
        .any(|pc| *pc >= header && !members.contains(pc) && !tail_members.contains(pc))
    {
        return None;
    }
    for (&from, ts) in &edges {
        for &to in ts {
            if from < header && to >= header && !entry.contains(&(from, to)) {
                return None;
            }
            if members.contains(&from) && !members.contains(&to) && (from, to) != (guard, terminal)
            {
                return None;
            }
            if tail_members.contains(&to)
                && !tail_members.contains(&from)
                && (from, to) != (guard, terminal)
            {
                return None;
            }
        }
    }
    let mut projected = cfg.clone();
    let source = *cfg.block_at.get(&copy_from)?;
    let header_block = *cfg.block_at.get(&header)?;
    if cfg.blocks[source].instructions.last() != Some(&copy_from) {
        return None;
    }
    projected.blocks[source].successors = vec![Edge {
        target: header_block,
        kind: EdgeKind::Normal,
    }];
    let dom = DominatorTree::compute_with_work_limit(&projected, 100_000).ok()?;
    if members
        .iter()
        .chain(&tail_members)
        .any(|pc| !dom.dominates(header_block, cfg.block_at[pc]))
    {
        return None;
    }
    let owned_edges = edges
        .iter()
        .flat_map(|(&from, ts)| {
            let entry = &entry;
            let members = &members;
            let tail_members = &tail_members;
            ts.iter().map(move |&to| {
                (
                    from,
                    to,
                    if (from, to) == (copy_from, copy_target) {
                        EdgeOwner::EntryCopy
                    } else if entry.contains(&(from, to)) {
                        EdgeOwner::HeaderEntry
                    } else if (from, to) == (latch, header) {
                        EdgeOwner::Continue
                    } else if (from, to) == (guard, terminal) || tail_members.contains(&from) {
                        EdgeOwner::ReturnArm
                    } else if members.contains(&from) {
                        EdgeOwner::Iteration
                    } else {
                        EdgeOwner::Prefix
                    },
                )
            })
        })
        .collect();
    Some(SingleGuardPlan {
        header,
        guard,
        latch,
        continuation,
        terminal,
        copy_from,
        copy_target,
        copy,
        header_path,
        continuation_path,
        terminal_path,
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

fn reachable(start: usize, edges: &BTreeMap<usize, Vec<usize>>) -> Option<BTreeSet<usize>> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![start];
    while let Some(pc) = pending.pop() {
        if seen.insert(pc) {
            pending.extend(edges.get(&pc)?);
        }
    }
    Some(seen)
}

fn linear(
    start: usize,
    stop: Option<usize>,
    edges: &BTreeMap<usize, Vec<usize>>,
    ins: &BTreeMap<usize, &crate::native_ir::Instruction>,
    limit: usize,
) -> Option<Vec<usize>> {
    let mut path = vec![];
    let mut pc = start;
    loop {
        if path.len() == limit || path.contains(&pc) {
            return None;
        }
        path.push(pc);
        if stop == Some(pc) {
            return Some(path);
        }
        match edges.get(&pc)?.as_slice() {
            [] if stop.is_none() => return Some(path),
            [next] if *next == pc + ins.get(&pc)?.width => pc = *next,
            _ => return None,
        }
    }
}
