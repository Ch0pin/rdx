//! Bounded node splitting of a dominated parent prefix into its exact reentry arms.
//! All selected exits return. Original targets remain immutable, and all copies
//! use the frame at the original edge. No execution state variable is introduced.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_dominators::DominatorTree,
    native_ir::DecodedMethod,
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PrefixPlan {
    pub words: Vec<u16>,
    pub registers: u16,
    pub ins: u16,
    pub widths: Vec<(usize, usize)>,
    pub targets: Vec<(usize, Option<usize>)>,
    pub parent_header: usize,
    pub child_header: usize,
    pub body_end: usize,
    pub prefix: Vec<usize>,
    pub parent_members: Vec<usize>,
    pub child_members: Vec<usize>,
    pub reentries: Vec<(usize, usize)>,
    pub terminal_edges: Vec<(usize, usize, Vec<usize>)>,
    pub edges: Vec<(usize, usize)>,
    pub copy_nodes: usize,
}
pub(super) fn validate(code: &DexCode, plan: &PrefixPlan) -> bool {
    select(code).as_ref() == Some(plan)
}
pub(super) fn select(code: &DexCode) -> Option<PrefixPlan> {
    if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 2048 {
        return None;
    }
    if !has_two_backedges(&code.instructions) {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    if ir
        .instructions
        .iter()
        .filter(|i| i.branch_target.is_some_and(|to| to < i.pc))
        .count()
        < 2
    {
        return None;
    }
    // First tranche delegates no allocation/switch/monitor or payload ownership.
    if ir.instructions.iter().any(|i| {
        matches!(i.opcode, 0x1d..=0x1e | 0x22..=0x26 | 0x2b..=0x2c)
            || (i.opcode == 0 && code.instructions[i.pc] != 0)
    }) {
        return None;
    }
    let by: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let mut next = 0;
    for i in &ir.instructions {
        if i.pc != next {
            return None;
        }
        next += i.width;
    }
    if next != code.instructions.len() {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 128 {
        return None;
    }
    let dom = DominatorTree::compute_with_work_limit(&cfg, 100_000).ok()?;
    let mut successors = BTreeMap::new();
    for b in &cfg.blocks {
        if b.successors.iter().any(|e| e.kind != EdgeKind::Normal) {
            return None;
        }
        for (n, &pc) in b.instructions.iter().enumerate() {
            successors.insert(
                pc,
                b.instructions
                    .get(n + 1)
                    .copied()
                    .map(|x| vec![x])
                    .unwrap_or_else(|| {
                        b.successors
                            .iter()
                            .map(|e| cfg.blocks[e.target].start)
                            .collect::<Vec<_>>()
                    }),
            );
        }
    }
    let mut loops: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (from, b) in cfg.blocks.iter().enumerate() {
        for e in &b.successors {
            if !dom.dominates(e.target, from) {
                continue;
            }
            let members = loops.entry(e.target).or_default();
            members.insert(e.target);
            let mut pending = vec![from];
            while let Some(x) = pending.pop() {
                if !members.insert(x) || x == e.target {
                    continue;
                }
                pending.extend(dom.predecessors[x].iter().copied());
            }
        }
    }
    // Exact depth two. Unrelated loops are left for a subsequent separately proved tranche.
    if loops.len() != 2 {
        return None;
    }
    let (&ph, parent) = loops
        .iter()
        .find(|(_, a)| loops.values().any(|b| *a != b && b.is_subset(a)))?;
    let (&ch, child) = loops.iter().find(|(h, _)| **h != ph)?;
    let parent_header = cfg.blocks[ph].start;
    let child_header = cfg.blocks[ch].start;
    if parent_header >= child_header || !child.is_subset(parent) {
        return None;
    }
    let pcs = |m: &BTreeSet<usize>| {
        let mut v: Vec<_> = m
            .iter()
            .flat_map(|&b| cfg.blocks[b].instructions.iter().copied())
            .collect();
        v.sort_unstable();
        v
    };
    let parent_members = pcs(parent);
    let child_members = pcs(child);
    if parent_members.first() != Some(&parent_header)
        || child_members.first() != Some(&child_header)
    {
        return None;
    }
    let body_end = child_members.last().map(|pc| pc + by[pc].width)?;
    if parent_members.last().map(|pc| pc + by[pc].width) != Some(body_end) {
        return None;
    }
    let mut pos = child_header;
    for &pc in &child_members {
        if pc != pos {
            return None;
        }
        pos += by[&pc].width;
    }
    let prefix: Vec<_> = by
        .range(parent_header..child_header)
        .map(|(&pc, _)| pc)
        .collect();
    if prefix.is_empty() || prefix.len() > 32 {
        return None;
    }
    let prefix_set: BTreeSet<_> = prefix.iter().copied().collect();
    let parent_set: BTreeSet<_> = parent_members.iter().copied().collect();
    let child_set: BTreeSet<_> = child_members.iter().copied().collect();
    if parent_set
        .iter()
        .any(|pc| !prefix_set.contains(pc) && !child_set.contains(pc))
    {
        return None;
    }
    let mut reentries = Vec::new();
    let mut terminal_edges = Vec::new();
    let mut edges = Vec::new();
    let terminal = |entry: usize| -> Option<Vec<usize>> {
        let mut pc = entry;
        let mut path = Vec::new();
        for _ in 0..16 {
            if parent_set.contains(&pc) || path.contains(&pc) {
                return None;
            }
            let i = by.get(&pc)?;
            path.push(pc);
            match i.opcode {
                0x0e..=0x11 => {
                    if !successors.get(&pc)?.is_empty() {
                        return None;
                    }
                    return Some(path);
                }
                0x00..=0x09 | 0x12..=0x19 if !i.may_throw => pc += i.width,
                0x28..=0x2a => {
                    let to = i.branch_target?;
                    if to != pc + i.width {
                        // Physical-range emission may not own skipped instructions.
                        return None;
                    }
                    pc = to;
                }
                _ => return None,
            }
            if successors.get(path.last()?)? != &vec![pc] {
                return None;
            }
        }
        None
    };
    for (&from, tos) in &successors {
        for &to in tos {
            if parent_set.contains(&to) && !parent_set.contains(&from) && to != parent_header {
                return None;
            }
            // No branch can enter the middle of a copied prefix, including from a terminal hole.
            if prefix_set.contains(&to) && !prefix_set.contains(&from) && to != parent_header {
                return None;
            }
            if !parent_set.contains(&from) && !prefix_set.contains(&from) {
                continue;
            }
            edges.push((from, to));
            if prefix_set.contains(&from) {
                // Prefix is a forward DAG; every nonterminal path reaches the child header.
                if to == child_header {
                    continue;
                }
                if prefix_set.contains(&to) && to > from {
                    continue;
                }
                let path = terminal(to)?;
                terminal_edges.push((from, to, path));
                continue;
            }
            if to == parent_header {
                if !child_set.contains(&from) || by[&from].branch_target != Some(to) {
                    return None;
                }
                reentries.push((from, to));
                continue;
            }
            if child_set.contains(&to) {
                // The only child cycle is a branch to its dominating header.
                if to <= from && (to != child_header || by[&from].branch_target != Some(to)) {
                    return None;
                }
                continue;
            }
            let path = terminal(to)?;
            terminal_edges.push((from, to, path));
        }
    }
    if reentries.is_empty()
        || reentries.len() > 8
        || terminal_edges.is_empty()
        || prefix.len() * (1 + reentries.len()) > 256
        || terminal_edges
            .iter()
            .map(|(_, _, p)| p.len())
            .sum::<usize>()
            > 128
    {
        return None;
    }
    let copy_nodes = prefix.len() * (1 + reentries.len())
        + terminal_edges
            .iter()
            .map(|(from, _, path)| {
                path.len()
                    * if prefix_set.contains(from) {
                        1 + reentries.len()
                    } else {
                        1
                    }
            })
            .sum::<usize>();
    if copy_nodes > 512 {
        return None;
    }
    // Pending result ownership: cloning may not sever producer/result adjacency.
    for &pc in &prefix {
        if matches!(by[&pc].opcode, 0x0a..=0x0c) {
            let producer = ir.instructions.iter().find(|i| i.pc + i.width == pc)?;
            if !prefix_set.contains(&producer.pc)
                || !matches!(producer.opcode, 0x6e..=0x78 | 0xfa..=0xfd)
                || successors
                    .iter()
                    .any(|(&from, tos)| from != producer.pc && tos.contains(&pc))
            {
                return None;
            }
        }
        if successors.get(&pc)?.is_empty() && !matches!(by[&pc].opcode, 0x0e..=0x11) {
            return None;
        }
        if matches!(by[&pc].opcode, 0x27) {
            return None;
        }
    }
    Some(PrefixPlan {
        words: code.instructions.clone(),
        registers: code.registers,
        ins: code.ins,
        widths: ir.instructions.iter().map(|i| (i.pc, i.width)).collect(),
        targets: ir
            .instructions
            .iter()
            .map(|i| (i.pc, i.branch_target))
            .collect(),
        parent_header,
        child_header,
        body_end,
        prefix,
        parent_members,
        child_members,
        reentries,
        terminal_edges,
        edges,
        copy_nodes,
    })
}

fn has_two_backedges(words: &[u16]) -> bool {
    let mut pc = 0usize;
    let mut backedges = 0;
    while pc < words.len() {
        let op = words[pc] as u8;
        let width = match op {
            0x18 => 5,
            0x00
            | 0x01
            | 0x04
            | 0x07
            | 0x0a..=0x12
            | 0x21
            | 0x27
            | 0x28
            | 0x7b..=0x8f
            | 0xb0..=0xcf => 1,
            0x02
            | 0x05
            | 0x08
            | 0x13
            | 0x15
            | 0x16
            | 0x19
            | 0x1a
            | 0x1c
            | 0x1f
            | 0x20
            | 0x22
            | 0x23
            | 0x29
            | 0x2d..=0x3d
            | 0x44..=0x6d
            | 0x90..=0xaf
            | 0xd0..=0xe2 => 2,
            0x03
            | 0x06
            | 0x09
            | 0x14
            | 0x17
            | 0x1b
            | 0x24..=0x26
            | 0x2a
            | 0x6e..=0x72
            | 0x74..=0x78 => 3,
            _ => return false,
        };
        if pc + width > words.len() || (op == 0 && words[pc] != 0) {
            return false;
        }
        if matches!(op, 0x28..=0x2a | 0x32..=0x3d)
            && branch_target(words, pc).is_some_and(|target| target <= pc)
        {
            backedges += 1;
        }
        pc += width;
    }
    backedges >= 2
}

fn branch_target(words: &[u16], pc: usize) -> Option<usize> {
    let delta = match *words.get(pc)? as u8 {
        0x28 => ((*words.get(pc)? >> 8) as i8) as i64,
        0x29 | 0x32..=0x3d => (*words.get(pc + 1)? as i16) as i64,
        0x2a => {
            (u32::from(*words.get(pc + 1)?) | (u32::from(*words.get(pc + 2)?) << 16)) as i32 as i64
        }
        _ => return None,
    };
    usize::try_from(pc as i64 + delta).ok()
}
