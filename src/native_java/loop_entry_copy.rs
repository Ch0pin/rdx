//! Bounded node splitting for forward-DAG external entries into one ordinary loop.
//! Original targets and effects remain authoritative; navigation alone treats
//! an external copied suffix as a path to the header or a terminal return.
use crate::{native_cfg::ControlFlowGraph, native_dex::DexCode, native_dominators::DominatorTree};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EntryCopy {
    pub from: usize,
    pub target: usize,
    pub instructions: Vec<usize>,
    /// None means the copy returns, rather than supplying a header frame.
    pub header: Option<usize>,
    pub kind: CopyKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CopyKind {
    Header,
    InteriorReturn,
    PrefixReturn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EdgeOwner {
    Original,
    LoopContinue,
    LoopBreak,
    EntryCopy,
    TerminalCopy,
    PrefixTerminalCopy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EntryCopyPlan {
    pub header: usize,
    pub guard: usize,
    pub latch: usize,
    pub exit: usize,
    pub copies: Vec<EntryCopy>,
    pub scc_members: Vec<usize>,
    pub widths: Vec<(usize, usize)>,
    pub targets: Vec<(usize, usize)>,
    pub edges: Vec<(usize, usize, EdgeOwner)>,
    pub terminals: Vec<usize>,
    pub terminal_guard: Option<super::single_guard_entry::SingleGuardPlan>,
    pub iteration: Option<super::forward_iteration_entry::IterationPlan>,
}

pub(super) fn validate(code: &DexCode, plan: &EntryCopyPlan) -> bool {
    plan.terminal_guard
        .as_ref()
        .is_none_or(|guard| super::single_guard_entry::validate(code, guard))
        && plan
            .iteration
            .as_ref()
            .is_none_or(|p| super::forward_iteration_entry::validate(code, p))
        && select(code).as_ref() == Some(plan)
}

pub(super) fn select(code: &DexCode) -> Option<EntryCopyPlan> {
    select_forward_dag(code)
        .or_else(|| {
            let inner = super::single_guard_entry::select(code)?;
            let scc_members = inner
                .header_path
                .iter()
                .chain(&inner.continuation_path)
                .copied()
                .collect();
            Some(EntryCopyPlan {
                header: inner.header,
                guard: inner.guard,
                latch: inner.latch,
                exit: code.instructions.len(),
                copies: vec![EntryCopy {
                    from: inner.copy_from,
                    target: inner.copy_target,
                    instructions: inner.copy.clone(),
                    header: Some(inner.header),
                    kind: CopyKind::Header,
                }],
                scc_members,
                widths: inner.widths.clone(),
                targets: inner.targets.clone(),
                edges: inner
                    .edges
                    .iter()
                    .map(|(from, to, owner)| {
                        (
                            *from,
                            *to,
                            match owner {
                                super::single_guard_entry::EdgeOwner::EntryCopy => {
                                    EdgeOwner::EntryCopy
                                }
                                super::single_guard_entry::EdgeOwner::Continue => {
                                    EdgeOwner::LoopContinue
                                }
                                super::single_guard_entry::EdgeOwner::ReturnArm
                                    if *from == inner.guard =>
                                {
                                    EdgeOwner::LoopBreak
                                }
                                _ => EdgeOwner::Original,
                            },
                        )
                    })
                    .collect(),
                terminals: vec![*inner.terminal_path.last()?],
                terminal_guard: Some(inner),
                iteration: None,
            })
        })
        .or_else(|| {
            let inner = super::forward_iteration_entry::select(code)?;
            Some(EntryCopyPlan {
                header: inner.header,
                guard: usize::MAX,
                latch: inner.latch,
                exit: code.instructions.len(),
                copies: inner
                    .copies
                    .iter()
                    .map(|c| EntryCopy {
                        from: c.from,
                        target: c.target,
                        instructions: c.instructions.clone(),
                        header: c.header,
                        kind: if c.header.is_some() {
                            CopyKind::Header
                        } else {
                            CopyKind::PrefixReturn
                        },
                    })
                    .collect(),
                scc_members: inner.members.clone(),
                widths: inner.widths.clone(),
                targets: inner.targets.clone(),
                edges: inner
                    .edges
                    .iter()
                    .map(|&(from, to, owner)| {
                        (
                            from,
                            to,
                            match owner {
                                super::forward_iteration_entry::EdgeOwner::HeaderContinue => {
                                    EdgeOwner::LoopContinue
                                }
                                super::forward_iteration_entry::EdgeOwner::EntryCopy => {
                                    EdgeOwner::EntryCopy
                                }
                                super::forward_iteration_entry::EdgeOwner::PrefixTerminal => {
                                    EdgeOwner::PrefixTerminalCopy
                                }
                                _ => EdgeOwner::Original,
                            },
                        )
                    })
                    .collect(),
                terminals: vec![],
                terminal_guard: None,
                iteration: Some(inner),
            })
        })
}

fn select_forward_dag(code: &DexCode) -> Option<EntryCopyPlan> {
    if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 4096 {
        return None;
    }
    // Operand false positives only cost analysis; canonical CFG admission below
    // never trusts this cheap corpus filter.
    if !code.instructions.iter().enumerate().any(|(pc, &word)| {
        matches!(word as u8, 0x28..=0x2a)
            && branch_target(&code.instructions, pc).is_some_and(|to| to < pc)
    }) {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 128 {
        return None;
    }
    let words = &code.instructions;
    let mut widths = BTreeMap::new();
    let mut successors = BTreeMap::new();
    let mut targets = BTreeMap::new();
    let mut terminals = vec![];
    for block in &cfg.blocks {
        for (index, &pc) in block.instructions.iter().enumerate() {
            let end = block
                .instructions
                .get(index + 1)
                .copied()
                .unwrap_or(block.end);
            let op = words[pc] as u8;
            if matches!(op, 0x1d | 0x1e | 0x2b | 0x2c) || (op == 0 && words[pc] != 0) {
                return None;
            }
            widths.insert(pc, end - pc);
            if let Some(to) = branch_target(words, pc) {
                targets.insert(pc, to);
            }
            let next: Vec<_> = if end < block.end {
                vec![end]
            } else {
                block
                    .successors
                    .iter()
                    .map(|edge| cfg.blocks[edge.target].start)
                    .collect()
            };
            if next.is_empty() {
                terminals.push(pc);
            }
            successors.insert(pc, next);
        }
    }
    let mut end = 0;
    for (&pc, &width) in &widths {
        if pc != end {
            return None;
        }
        end = pc + width;
    }
    if end != words.len() {
        return None;
    }
    let mut predecessors: BTreeMap<_, Vec<_>> = widths.keys().map(|&pc| (pc, vec![])).collect();
    for (&pc, next) in &successors {
        for to in next {
            predecessors.get_mut(to)?.push(pc);
        }
    }
    let headers: BTreeSet<_> = targets
        .iter()
        .filter_map(|(&pc, &to)| (to < pc && matches!(words[pc] as u8, 0x28..=0x2a)).then_some(to))
        .collect();
    if headers.len() > 16 {
        return None;
    }
    let mut chosen = None;
    for header in headers {
        let forward = reachable(header, &successors)?;
        let reverse = reachable(header, &predecessors)?;
        let members: BTreeSet<_> = forward.intersection(&reverse).copied().collect();
        if members.len() < 2 || members.first() != Some(&header) {
            continue;
        }
        let latch = *members.last()?;
        let exit = latch + widths[&latch];
        if !matches!(words[latch] as u8, 0x28..=0x2a) || targets.get(&latch) != Some(&header) {
            continue;
        }
        let backedges: Vec<_> = members
            .iter()
            .filter(|&&pc| targets.get(&pc) == Some(&header))
            .copied()
            .collect();
        if backedges != vec![latch] || !iteration_is_dag(header, &members, &successors) {
            continue;
        }
        let block = cfg.block_at.get(&header).copied()?;
        let guard = *cfg.blocks[block].instructions.last()?;
        if !matches!(words[guard] as u8, 0x32..=0x3d) || targets.get(&guard) != Some(&exit) {
            continue;
        }
        let mut copies = vec![];
        let mut valid = true;
        for (&from, next) in &successors {
            for &to in next {
                if from >= header && from < exit || to <= header || to >= exit {
                    continue;
                }
                if from >= header || !matches!(words[from] as u8, 0x28..=0x2a) {
                    valid = false;
                    break;
                }
                let mut path = vec![];
                let destination = if !members.contains(&to) {
                    // First tranche: shared interior terminal returns only.
                    if !matches!(words[to] as u8, 0x0e..=0x11) {
                        valid = false;
                        break;
                    }
                    path.push(to);
                    None
                } else {
                    let Some(nodes) = forward_suffix(
                        SuffixGraph {
                            words,
                            widths: &widths,
                            successors: &successors,
                            predecessors: &predecessors,
                            members: &members,
                        },
                        to,
                        header,
                        latch,
                    ) else {
                        valid = false;
                        break;
                    };
                    path = nodes;
                    Some(header)
                };
                copies.push(EntryCopy {
                    from,
                    target: to,
                    instructions: path,
                    header: destination,
                    kind: if destination.is_some() {
                        CopyKind::Header
                    } else {
                        CopyKind::InteriorReturn
                    },
                });
            }
            if !valid {
                break;
            }
        }
        if !valid
            || copies.is_empty()
            || copies.len() > 8
            || !copies.iter().any(|copy| copy.header.is_some())
        {
            continue;
        }
        // Pure terminal prefix leaves can sit after the loop. Copy these
        // separately so nonterminal prefix arms merge at the actual header.
        for (&from, &to) in &targets {
            if from < header && to >= exit {
                let Some(path) = terminal_path(words, &widths, to) else {
                    valid = false;
                    break;
                };
                copies.push(EntryCopy {
                    from,
                    target: to,
                    instructions: path,
                    header: None,
                    kind: CopyKind::PrefixReturn,
                });
            }
        }
        if !valid || copies.len() > 8 {
            continue;
        }
        // Prove the original loop becomes single-entry after edge-specific
        // copies. This projection collapses copies only for navigation; the
        // renderer must emit every retained original instruction first.
        let mut projected = cfg.clone();
        for copy in &copies {
            let source = *cfg.block_at.get(&copy.from)?;
            if cfg.blocks[source].instructions.last() != Some(&copy.from) {
                valid = false;
                break;
            }
            if copy.header.is_some() {
                projected.blocks[source].successors = vec![crate::native_cfg::Edge {
                    target: block,
                    kind: crate::native_cfg::EdgeKind::Normal,
                }];
            } else {
                let target_block = *cfg.block_at.get(&copy.target)?;
                projected.blocks[source]
                    .successors
                    .retain(|edge| edge.target != target_block);
            }
        }
        if !valid {
            continue;
        }
        let dom = DominatorTree::compute_with_work_limit(&projected, 100_000).ok()?;
        if widths
            .keys()
            .any(|&pc| pc >= header && pc < exit && !dom.dominates(block, cfg.block_at[&pc]))
        {
            continue;
        }
        let mut edges = vec![];
        for (&from, next) in &successors {
            for &to in next {
                let owner = if let Some(copy) = copies
                    .iter()
                    .find(|copy| copy.from == from && copy.target == to)
                {
                    if copy.header.is_some() {
                        EdgeOwner::EntryCopy
                    } else if copy.kind == CopyKind::PrefixReturn {
                        EdgeOwner::PrefixTerminalCopy
                    } else {
                        EdgeOwner::TerminalCopy
                    }
                } else if from == latch && to == header {
                    EdgeOwner::LoopContinue
                } else if from == guard && to == exit {
                    EdgeOwner::LoopBreak
                } else {
                    EdgeOwner::Original
                };
                edges.push((from, to, owner));
            }
        }
        if !valid {
            continue;
        }
        let plan = EntryCopyPlan {
            header,
            guard,
            latch,
            exit,
            copies,
            scc_members: members.into_iter().collect(),
            widths: widths.iter().map(|(&pc, &width)| (pc, width)).collect(),
            targets: targets.iter().map(|(&pc, &to)| (pc, to)).collect(),
            edges,
            terminals: terminals.clone(),
            terminal_guard: None,
            iteration: None,
        };
        // Multiple competing intervals need a separate composition proof.
        if chosen.is_some() {
            return None;
        }
        chosen = Some(plan);
    }
    chosen
}

// All reachable copied nodes are explicitly owned. Physical instructions
// skipped by a goto are never admitted merely because they lie in the span.
struct SuffixGraph<'a> {
    words: &'a [u16],
    widths: &'a BTreeMap<usize, usize>,
    successors: &'a BTreeMap<usize, Vec<usize>>,
    predecessors: &'a BTreeMap<usize, Vec<usize>>,
    members: &'a BTreeSet<usize>,
}

fn forward_suffix(
    graph: SuffixGraph<'_>,
    start: usize,
    header: usize,
    latch: usize,
) -> Option<Vec<usize>> {
    let SuffixGraph {
        words,
        widths,
        successors,
        predecessors,
        members,
    } = graph;
    // Only the initial single-successor stem may run backward in address
    // order. Every edge after its first branch stays forward. A second pass
    // below proves that even the permitted stem contains no graph cycle.
    let mut stem = BTreeSet::new();
    let mut pc = start;
    while pc != header
        && !matches!(*words.get(pc)? as u8, 0x32..=0x3d)
        && successors.get(&pc)?.len() == 1
    {
        if stem.len() == 32 || !stem.insert(pc) {
            return None;
        }
        pc = successors[&pc][0];
    }
    let mut nodes = BTreeSet::new();
    let mut pending = vec![start];
    while let Some(pc) = pending.pop() {
        if nodes.contains(&pc) {
            continue;
        }
        if nodes.len() == 32 || !members.contains(&pc) || pc == header {
            return None;
        }
        let op = *words.get(pc)? as u8;
        if matches!(op, 0x0e..=0x11 | 0x1d | 0x1e | 0x22..=0x27 | 0x2b | 0x2c) {
            return None;
        }
        // A branch or split entry must not manufacture a pending call result.
        // Its producer remains the sole, physically adjacent raw predecessor.
        if matches!(op, 0x0a..=0x0c) {
            let previous = predecessors.get(&pc)?;
            if previous.len() != 1 || pc == start {
                return None;
            }
            let producer = previous[0];
            if producer + widths.get(&producer)? != pc
                || !matches!(words[producer] as u8, 0x6e..=0x72 | 0x74..=0x78 | 0xfa..=0xfd)
            {
                return None;
            }
        }
        nodes.insert(pc);
        let next = successors.get(&pc)?;
        if pc == latch {
            if next.as_slice() != [header] || !matches!(op, 0x28..=0x2a) {
                return None;
            }
            continue;
        }
        if next.is_empty() || (next.len() == 2 && !matches!(op, 0x32..=0x3d)) || next.len() > 2 {
            return None;
        }
        for &to in next {
            if to > latch
                || to == header
                || (to <= pc && !(stem.contains(&pc) && matches!(op, 0x28..=0x2a)))
            {
                return None;
            }
            pending.push(to);
        }
    }
    if !nodes.contains(&latch)
        || nodes.last() != Some(&latch)
        || !iteration_is_dag(header, &nodes, successors)
    {
        return None;
    }
    Some(nodes.into_iter().collect())
}

fn terminal_path(
    words: &[u16],
    widths: &BTreeMap<usize, usize>,
    start: usize,
) -> Option<Vec<usize>> {
    let mut path = vec![];
    let mut pc = start;
    while path.len() < 8 {
        let op = *words.get(pc)? as u8;
        path.push(pc);
        if matches!(op, 0x0e..=0x11) {
            return Some(path);
        }
        if !matches!(op, 0x00..=0x09 | 0x12..=0x19) {
            return None;
        }
        pc += widths.get(&pc)?;
    }
    None
}

fn reachable(start: usize, edges: &BTreeMap<usize, Vec<usize>>) -> Option<BTreeSet<usize>> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![start];
    while let Some(pc) = pending.pop() {
        if seen.insert(pc) {
            pending.extend(edges.get(&pc)?.iter().copied());
        }
    }
    Some(seen)
}

fn iteration_is_dag(
    header: usize,
    members: &BTreeSet<usize>,
    edges: &BTreeMap<usize, Vec<usize>>,
) -> bool {
    let mut incoming: BTreeMap<_, usize> = members
        .iter()
        .filter(|&&pc| pc != header)
        .map(|&pc| (pc, 0))
        .collect();
    for (&from, next) in edges {
        if from != header && members.contains(&from) {
            for to in next {
                if let Some(count) = incoming.get_mut(to) {
                    *count += 1;
                }
            }
        }
    }
    let mut ready: Vec<_> = incoming
        .iter()
        .filter_map(|(&pc, &count)| (count == 0).then_some(pc))
        .collect();
    let mut visited = 0;
    while let Some(pc) = ready.pop() {
        visited += 1;
        for to in &edges[&pc] {
            if let Some(count) = incoming.get_mut(to) {
                *count -= 1;
                if *count == 0 {
                    ready.push(*to);
                }
            }
        }
    }
    visited == incoming.len()
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
