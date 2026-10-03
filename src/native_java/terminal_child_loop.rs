//! Exact, bounded ownership for a terminal child whose guards continue its parent.
//! The plan describes original edges. It does not choose Java values or replace
//! either loop's latch with a synthetic address.
use crate::{native_cfg::ControlFlowGraph, native_dex::DexCode, native_dominators::DominatorTree};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EdgeOwner {
    Internal,
    ChildContinue,
    ParentContinue,
    ParentBreak,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OwnedEdge {
    pub from: usize,
    pub to: usize,
    pub owner: EdgeOwner,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TerminalChildPlan {
    pub prefix_allocation_pairs: Vec<(usize, usize, u16)>,
    pub original_words: Vec<u16>,
    pub parent_header: usize,
    pub parent_guard: usize,
    pub parent_exit: usize,
    pub parent_backedges: Vec<usize>,
    pub parent_body_end: usize,
    pub parent_members: Vec<usize>,
    pub child_header: usize,
    pub child_header_continue_guards: Vec<usize>,
    pub child_latch: usize,
    pub child_body_end: usize,
    pub child_members: Vec<usize>,
    pub terminal_fallthrough: Option<usize>,
    pub terminal_tail: Vec<usize>,
    pub edges: Vec<OwnedEdge>,
}

/// Recompute at emission; a cached plan is never authority for edited bytecode.
pub(super) fn validate(code: &DexCode, plan: &TerminalChildPlan) -> bool {
    select(code).as_ref() == Some(plan)
}

#[derive(Default)]
struct NaturalLoop {
    members: BTreeSet<usize>,
    latches: BTreeSet<usize>,
}

pub(super) fn select(code: &DexCode) -> Option<TerminalChildPlan> {
    if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 2048 {
        return None;
    }
    // Allocation-free negative filter before corpus-wide CFG construction.
    // Admission still requires the canonical CFG and exact proof below.
    if !has_two_backedges(&code.instructions) {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 128 {
        return None;
    }
    let dom = DominatorTree::compute_with_work_limit(&cfg, 100_000).ok()?;
    let words = &code.instructions;
    let mut widths = BTreeMap::new();
    let mut successors = BTreeMap::new();
    for block in &cfg.blocks {
        for (index, &pc) in block.instructions.iter().enumerate() {
            let end = block
                .instructions
                .get(index + 1)
                .copied()
                .unwrap_or(block.end);
            let op = words[pc] as u8;
            // Allocation state and synchronization need additional frame proofs.
            if matches!(op, 0x1d | 0x1e | 0x2b | 0x2c) || (op == 0 && words[pc] != 0) {
                return None;
            }
            widths.insert(pc, end - pc);
            successors.insert(
                pc,
                if end < block.end {
                    vec![end]
                } else {
                    block
                        .successors
                        .iter()
                        .map(|edge| cfg.blocks[edge.target].start)
                        .collect()
                },
            );
        }
    }
    // Preserve complete reachable executable coverage; unreachable bodies cannot
    // silently become part of an address-based emission interval.
    let mut next = 0;
    for (&pc, &width) in &widths {
        if pc != next {
            return None;
        }
        next = pc + width;
    }
    if next != words.len() {
        return None;
    }
    let mut loops: BTreeMap<usize, NaturalLoop> = BTreeMap::new();
    for (source, block) in cfg.blocks.iter().enumerate() {
        for edge in &block.successors {
            let target = edge.target;
            if !dom.dominates(target, source) {
                continue;
            }
            let natural = loops.entry(target).or_default();
            natural.latches.insert(*block.instructions.last()?);
            natural.members.insert(target);
            let mut pending = vec![source];
            while let Some(member) = pending.pop() {
                if !natural.members.insert(member) || member == target {
                    continue;
                }
                pending.extend(dom.predecessors[member].iter().copied());
            }
        }
    }
    if loops.len() > 16 {
        return None;
    }
    // A selected pair is top-level and isolated from every other natural loop.
    // Other owners remain subject to the original Graph classifier and renderer.
    let prove_pair = |parent_index: usize,
                      parent: &NaturalLoop,
                      child_index: usize,
                      child: &NaturalLoop|
     -> Option<TerminalChildPlan> {
        if loops.iter().any(|(&header, other)| {
            header != parent_index
                && header != child_index
                && !other.members.is_disjoint(&parent.members)
        }) {
            return None;
        }
        if parent.members == child.members || child.latches.len() != 1 {
            return None;
        }
        let parent_header = cfg.blocks[parent_index].start;
        let child_header = cfg.blocks[child_index].start;
        // Completed allocations outside the selected members keep their ordinary
        // owner and original effect/exception order. The normal renderer still
        // proves exact constructor name, owner, proto and argument frame. This
        // plan proves adjacent receiver ownership and no constructor interior entry.
        // No allocation is admitted in the selected parent/child body.
        let ir = crate::native_ir::DecodedMethod::decode(code).ok()?;
        let decoded: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
        let mut prefix_allocation_pairs = Vec::new();
        for instruction in ir.instructions.iter().filter(|i| i.opcode == 0x22) {
            let pc = instruction.pc;
            // Allocation is delegated only to an ordinary, disjoint region. Its
            // receiver must finish immediately; no selected entry can straddle it.
            if parent.members.contains(cfg.block_at.get(&pc)?) {
                return None;
            }
            let receiver = instruction.writes.first()?.register;
            let invoke = decoded.get(&(pc + instruction.width))?;
            if !matches!(invoke.opcode, 0x70 | 0x76)
                || invoke.reads.first()?.register != receiver
                || parent.members.contains(cfg.block_at.get(&invoke.pc)?)
                || (pc < parent_header && invoke.pc + invoke.width > parent_header)
                || (pc < child_header && invoke.pc + invoke.width > child_header)
                || cfg.blocks.iter().any(|block| {
                    block.successors.iter().any(|edge| {
                        cfg.blocks[edge.target].start == invoke.pc
                            && block.instructions.last() != Some(&pc)
                    })
                })
            {
                return None;
            }
            prefix_allocation_pairs.push((pc, invoke.pc, receiver));
        }
        let member_pcs = |members: &BTreeSet<usize>| -> Vec<usize> {
            let mut pcs: Vec<_> = members
                .iter()
                .flat_map(|&block| cfg.blocks[block].instructions.iter().copied())
                .collect();
            pcs.sort_unstable();
            pcs
        };
        let parent_members = member_pcs(&parent.members);
        let child_members = member_pcs(&child.members);
        if parent_members.first() != Some(&parent_header)
            || child_members.first() != Some(&child_header)
        {
            return None;
        }
        let body_end = |pcs: &[usize]| -> Option<usize> {
            let mut next = *pcs.first()?;
            for &pc in pcs {
                if pc != next {
                    return None;
                }
                next = pc + widths[&pc];
            }
            Some(next)
        };
        let parent_body_end = body_end(&parent_members)?;
        let child_body_end = body_end(&child_members)?;
        if child_body_end != parent_body_end || child_header <= parent_header {
            return None;
        }
        // Other loop address intervals must not cross the selected owned body.
        // A foreign member hidden inside an interval cannot bypass its own renderer.
        if loops.iter().any(|(&header, other)| {
            if header == parent_index || header == child_index {
                return false;
            }
            let pcs = member_pcs(&other.members);
            let Some(&first) = pcs.first() else {
                return true;
            };
            let Some(&last) = pcs.last() else {
                return true;
            };
            first < parent_body_end && last + widths[&last] > parent_header
        }) {
            return None;
        }
        // Every selected entry must be its actual dominating header.
        for (source, block) in cfg.blocks.iter().enumerate() {
            for edge in &block.successors {
                for (header, members) in [
                    (parent_index, &parent.members),
                    (child_index, &child.members),
                ] {
                    if members.contains(&edge.target)
                        && !members.contains(&source)
                        && edge.target != header
                    {
                        return None;
                    }
                }
            }
        }
        let parent_guard = *cfg.blocks[parent_index].instructions.last()?;
        if !matches!(words[parent_guard] as u8, 0x32..=0x3d) {
            return None;
        }
        let parent_exit = branch_target(words, parent_guard)?;
        if parent_members.binary_search(&parent_exit).is_ok()
            || parent_exit < parent_body_end
            || !successors[&parent_guard].contains(&(parent_guard + widths[&parent_guard]))
        {
            return None;
        }
        let child_latch = *child.latches.first()?;
        if child_latch + widths[&child_latch] != child_body_end
            || branch_target(words, child_latch) != Some(child_header)
        {
            return None;
        }
        let parent_backedges: Vec<_> = parent.latches.iter().copied().collect();
        if parent_backedges
            .iter()
            .any(|pc| *pc + widths[pc] >= parent_body_end)
        {
            return None;
        }
        // Only consecutive header tests may continue the parent. Prefix operations
        // stay inside the child and are emitted before their original guard.
        let mut child_header_continue_guards = vec![];
        let mut pc = child_header;
        let mut prefix_budget = 32;
        while pc < child_latch && prefix_budget > 0 {
            prefix_budget -= 1;
            let op = words[pc] as u8;
            if matches!(op, 0x32..=0x3d) && branch_target(words, pc) == Some(parent_header) {
                child_header_continue_guards.push(pc);
            } else if !child_header_continue_guards.is_empty()
                || matches!(op, 0x27..=0x3d | 0x0e..=0x11)
            {
                break;
            }
            pc += widths[&pc];
        }
        if child_header_continue_guards.is_empty()
            || parent_backedges.iter().any(|pc| {
                if child_members.contains(pc) {
                    !child_header_continue_guards.contains(pc)
                } else {
                    // The existing parent context before the child owns this edge.
                    // Transfers from later ancestor body instructions stay excluded.
                    *pc <= parent_guard || *pc >= child_header
                }
            })
        {
            return None;
        }
        let terminal_fallthrough = if matches!(words[child_latch] as u8, 0x32..=0x3d) {
            Some(child_body_end)
        } else if matches!(words[child_latch] as u8, 0x28..=0x2a) {
            None
        } else {
            return None;
        };
        let mut terminal_tail = vec![];
        if let Some(start) = terminal_fallthrough {
            let mut pc = start;
            loop {
                if terminal_tail.len() == 16 || parent_members.binary_search(&pc).is_ok() {
                    return None;
                }
                terminal_tail.push(pc);
                let op = *words.get(pc)? as u8;
                if matches!(op, 0x0e..=0x11) {
                    break;
                }
                if matches!(op, 0x27..=0x3d) || successors.get(&pc)? != &vec![pc + widths[&pc]] {
                    return None;
                }
                pc += widths[&pc];
            }
            if !terminal_tail.contains(&parent_exit) {
                return None;
            }
        }
        let mut edges = vec![];
        for &from in &parent_members {
            if successors[&from].is_empty() {
                return None;
            }
            for &to in &successors[&from] {
                let owner = if from == child_latch && to == child_header {
                    EdgeOwner::ChildContinue
                } else if parent_backedges.contains(&from) && to == parent_header {
                    EdgeOwner::ParentContinue
                } else if !child_members.contains(&from) && to == parent_exit {
                    EdgeOwner::ParentBreak
                } else if from == child_latch && Some(to) == terminal_fallthrough {
                    EdgeOwner::Terminal
                } else if parent_members.binary_search(&to).is_ok() {
                    // All other cyclic transfers must stay within their owner;
                    // an interior ancestor continue cannot be disguised as internal.
                    if to <= from && to != child_header {
                        return None;
                    }
                    EdgeOwner::Internal
                } else {
                    return None;
                };
                edges.push(OwnedEdge { from, to, owner });
            }
        }
        Some(TerminalChildPlan {
            prefix_allocation_pairs,
            original_words: code.instructions.clone(),
            parent_header,
            parent_guard,
            parent_exit,
            parent_backedges,
            parent_body_end,
            parent_members,
            child_header,
            child_header_continue_guards,
            child_latch,
            child_body_end,
            child_members,
            terminal_fallthrough,
            terminal_tail,
            edges,
        })
    };
    let mut selected = None;
    for (&parent_index, parent) in &loops {
        for (&child_index, child) in &loops {
            if parent_index == child_index
                || parent.members == child.members
                || !child.members.is_subset(&parent.members)
            {
                continue;
            }
            if let Some(plan) = prove_pair(parent_index, parent, child_index, child) {
                if selected.is_some() {
                    return None;
                }
                selected = Some(plan);
            }
        }
    }
    selected
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
