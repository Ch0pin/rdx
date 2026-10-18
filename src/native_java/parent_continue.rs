//! Bounded canonical child loop whose sole nonterminal exit continues its parent.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_dominators::DominatorTree,
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Feedback {
    pub from: usize,
    pub block: usize,
    pub successors: Vec<usize>,
    pub phi_inputs: Vec<(u16, usize)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Sibling {
    pub header: usize,
    pub latch: usize,
    pub exit: usize,
    pub nodes: Vec<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub parent: usize,
    pub parent_guard: usize,
    pub exit: usize,
    pub bound: usize,
    pub child: usize,
    pub child_guard: usize,
    pub child_latch: usize,
    /// A conditional child feedback edge may fall through to one original
    /// straight-line success suffix before continuing the parent.
    pub success_tail: Vec<usize>,
    pub siblings: Vec<Sibling>,
    pub child_feedback: Vec<Feedback>,
    pub parent_nodes: Vec<usize>,
    pub child_nodes: Vec<usize>,
    pub terminal_nodes: Vec<usize>,
    pub parent_phi_inputs: Vec<(u16, usize)>,
    pub parent_success_phi_inputs: Vec<(u16, usize)>,
    pub edges: Vec<(usize, usize)>,
    pub widths: Vec<(usize, usize)>,
    pub targets: Vec<(usize, usize)>,
    pub words: Vec<u16>,
    pub registers: u16,
    pub ins: u16,
    pub outs: u16,
}
pub(super) fn validate(code: &DexCode, plan: &Plan) -> bool {
    select_all(code).is_some_and(|plans| plans.contains(plan))
}
pub(super) fn validate_all(code: &DexCode, plans: &[Plan]) -> bool {
    select_all(code).as_deref() == Some(plans)
}
pub(super) fn select_all(code: &DexCode) -> Option<Vec<Plan>> {
    if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 2048 {
        return None;
    }
    // Width-aware negative filter; only the canonical CFG proof admits edges.
    if !has_candidate(&code.instructions) {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 128 {
        return None;
    }
    let ins: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let payloads = switch_payload_layout(code, &ir)?;
    let mut edges = BTreeMap::new();
    for b in &cfg.blocks {
        for (index, pc) in b.instructions.iter().enumerate() {
            let next = if let Some(next) = b.instructions.get(index + 1) {
                vec![*next]
            } else {
                if b.successors.iter().any(|e| e.kind != EdgeKind::Normal) {
                    return None;
                }
                b.successors
                    .iter()
                    .map(|e| cfg.blocks[e.target].start)
                    .collect()
            };
            edges.insert(*pc, next);
        }
    }
    if edges.len() != ins.len()
        && ins.iter().any(|(pc, instruction)| {
            !(edges.contains_key(pc)
                || (instruction.opcode == 0
                    && instruction.width == 1
                    && payloads.iter().any(|(start, _)| *start == *pc + 1)
                    && ir.instructions.iter().any(|previous| {
                        previous.pc + previous.width == *pc
                            && matches!(previous.opcode, 0x0e..=0x11 | 0x27..=0x2a)
                    })))
        })
    {
        return None;
    }
    let backward = super::forward_iteration_entry::cyclic_backward_edges(
        &edges,
        edges
            .iter()
            .flat_map(|(f, ts)| ts.iter().filter(move |t| *t <= f).map(move |t| (*f, *t)))
            .collect(),
    )?;
    let headers: BTreeSet<_> = backward.iter().map(|(_, t)| *t).collect();
    let candidates: Vec<_> = headers
        .iter()
        .filter_map(|child| {
            let block = cfg.blocks.get(*cfg.block_at.get(child)?)?;
            let guard = ins.get(block.instructions.last()?)?;
            let parent = guard.branch_target?;
            (parent < *child && headers.contains(&parent) && matches!(guard.opcode, 0x32..=0x3d))
                .then_some((parent, *child))
        })
        .collect();
    if candidates.is_empty() || candidates.len() > 16 {
        return None;
    }
    let dom = DominatorTree::compute_with_work_limit(&cfg, 100_000).ok()?;
    let mut ssa_cache = None;
    let mut ssa_work = 0usize;
    let mut prove = |parent: usize, child: usize, work: &mut usize| -> Option<Plan> {
        let child_block = &cfg.blocks[*cfg.block_at.get(&child)?];
        let child_guard = *child_block.instructions.last()?;
        // Only exact invocation/result or narrow field-load headers precede the guard.
        if child_guard != child && !narrow_field_guard(&child_block.instructions, &ins) {
            let [call, result, test] = child_block.instructions[..] else {
                return None;
            };
            let invoke = ins[&call];
            if !matches!(invoke.opcode, 0x6e..=0x72 | 0x74..=0x78)
                || ins[&result].opcode != 0x0a
                || test != child_guard
                || invoke.pc + invoke.width != result
                || result + ins[&result].width != child_guard
                || invoke.branch_target.is_some()
            {
                return None;
            }
        }
        let guard = ins[&child_guard];
        if !matches!(guard.opcode, 0x32..=0x3d) || guard.branch_target != Some(parent) {
            return None;
        }
        let child_latches: Vec<_> = backward
            .iter()
            .filter(|(_, t)| *t == child)
            .map(|(f, _)| *f)
            .collect();
        if child_latches.is_empty() || child_latches.len() > 2 {
            return None;
        }
        let child_latch = *child_latches.last()?;
        if child_latches
            .iter()
            .any(|pc| !matches!(ins[pc].opcode, 0x28..=0x2a | 0x32..=0x3d))
        {
            return None;
        }
        let (pb, cb, lb) = (
            cfg.block_at[&parent],
            cfg.block_at[&child],
            cfg.block_at[&child_latch],
        );
        if cfg.blocks[pb].start != parent
            || cfg.blocks[cb].start != child
            || !dom.dominates(pb, cb)
            || !dom.dominates(cb, lb)
        {
            return None;
        }
        let parent_block = &cfg.blocks[pb];
        if parent_block.instructions.len() > 8 {
            return None;
        }
        let parent_guard = *parent_block.instructions.last()?;
        let pg = ins[&parent_guard];
        if !matches!(pg.opcode, 0x32..=0x3d) {
            return None;
        }
        let exit = pg.branch_target?;
        if exit <= child_latch {
            return None;
        }
        let success_tail = child_success_tail(child_latch, parent, exit, &ins, &edges)?;
        let child_bound = success_tail.last().copied().unwrap_or(child_latch);
        let child_nodes = reachable(child, &edges, &BTreeSet::from([parent]))?;
        if child_nodes.len() > 64
            || child_nodes
                .iter()
                .any(|pc| *pc < child || *pc > child_bound)
        {
            return None;
        }
        if edges.iter().any(|(from, ts)| {
            !child_nodes.contains(from)
                && ts.iter().any(|to| child_nodes.contains(to) && *to != child)
        }) {
            return None;
        }
        let child_exits: Vec<_> = child_nodes
            .iter()
            .flat_map(|f| {
                edges[f]
                    .iter()
                    .filter(|t| !child_nodes.contains(t))
                    .map(move |t| (*f, *t))
            })
            .collect();
        let mut expected_exits = vec![(child_guard, parent)];
        if let Some(&last) = success_tail.last() {
            expected_exits.push((last, parent));
        }
        if child_exits != expected_exits {
            return None;
        }
        let mut child_dag = edges.clone();
        for feedback in &child_latches {
            child_dag.get_mut(feedback)?.retain(|t| *t != child);
        }
        if !dag(&child_nodes, &child_dag) {
            return None;
        }
        let parent_nodes = reachable(parent, &edges, &BTreeSet::from([exit]))?;
        if parent_nodes.len() > 256
            || (!payloads.is_empty()
                && (parent_nodes
                    .iter()
                    .any(|pc| ins[pc].payload_target.is_some())
                    || payloads
                        .iter()
                        .any(|(start, end)| *start < exit && *end > parent)))
            || !child_nodes.is_subset(&parent_nodes)
            || parent_nodes.iter().any(|pc| *pc < parent || *pc >= exit)
        {
            return None;
        }
        if parent_nodes
            .iter()
            .any(|pc| !dom.dominates(pb, cfg.block_at[pc]))
            || edges.iter().any(|(from, ts)| {
                !parent_nodes.contains(from)
                    && ts
                        .iter()
                        .any(|to| parent_nodes.contains(to) && *to != parent)
            })
        {
            return None;
        }
        let exits: Vec<_> = parent_nodes
            .iter()
            .flat_map(|f| {
                edges[f]
                    .iter()
                    .filter(|t| !parent_nodes.contains(t))
                    .map(move |t| (*f, *t))
            })
            .collect();
        if exits != vec![(parent_guard, exit)] {
            return None;
        }
        let siblings = independent_siblings(
            parent,
            child,
            &parent_nodes,
            &child_nodes,
            &headers,
            &backward,
            &ins,
            &edges,
            &cfg,
            &dom,
        )?;
        if backward
            .iter()
            .any(|(from, to)| (*to == parent || *to == child) && !parent_nodes.contains(from))
        {
            return None;
        }
        let bound = *parent_nodes.last()?;
        if !matches!(ins[&bound].opcode, 0x28..=0x2a)
            || !matches!(ins[&bound].branch_target,Some(t)if t==parent||t==child)
        {
            return None;
        }
        let mut parent_dag = edges.clone();
        for (from, to) in &backward {
            parent_dag.get_mut(from)?.retain(|t| t != to);
        }
        if !dag(&parent_nodes, &parent_dag) {
            return None;
        }
        if ssa_cache.is_none() {
            ssa_cache = Some(SsaMethod::build_with_work_limit(code, &ir, &cfg, 100_000).ok()?);
        }
        let ssa = ssa_cache.as_ref()?;
        let reads: BTreeMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
        let phis: BTreeMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
        let mut pending: Vec<_> = parent_nodes
            .iter()
            .flat_map(|pc| reads[pc].reads.iter().flat_map(|r| r.words.iter().copied()))
            .collect();
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            *work += 1;
            if *work > 100_000 {
                return None;
            }
            if !seen.insert(id) {
                continue;
            }
            match &ssa.definitions.get(id)?.kind {
                DefinitionKind::Undefined => return None,
                DefinitionKind::Parameter => {}
                DefinitionKind::Instruction { pc, .. } => pending.extend(
                    reads
                        .get(pc)?
                        .reads
                        .iter()
                        .flat_map(|r| r.words.iter().copied()),
                ),
                DefinitionKind::Phi { .. } => {
                    pending.extend(phis.get(&id)?.incoming.iter().map(|(_, v)| *v))
                }
            }
        }
        // Wide live inputs are complete SSA pairs, including each predecessor
        // of a loop phi. A narrow overwrite cannot retain a stale second word.
        let mut pending_pairs: Vec<_> = parent_nodes
            .iter()
            .flat_map(|pc| reads[pc].reads.iter())
            .filter_map(|operand| match operand.words[..] {
                [low, high] => Some((low, high)),
                _ => None,
            })
            .collect();
        let mut seen_pairs = BTreeSet::new();
        while let Some((low, high)) = pending_pairs.pop() {
            *work += 1;
            if *work > 100_000 {
                return None;
            }
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
                    let source = reads.get(low_pc)?;
                    if !source
                        .writes
                        .iter()
                        .any(|operand| operand.words == [low, high])
                    {
                        return None;
                    }
                    pending_pairs.extend(source.reads.iter().filter_map(|operand| {
                        match operand.words[..] {
                            [low, high] => Some((low, high)),
                            _ => None,
                        }
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
        let mut child_feedback = Vec::new();
        for feedback in &child_latches {
            let feedback_block = cfg.block_at[feedback];
            let mut phi_inputs = Vec::new();
            for phi in &ssa.phis {
                if phi.block == cb && seen.contains(&phi.result) {
                    let values: Vec<_> = phi
                        .incoming
                        .iter()
                        .filter(|(block, _)| *block == Some(feedback_block))
                        .map(|(_, value)| *value)
                        .collect();
                    let [value] = values[..] else {
                        return None;
                    };
                    phi_inputs.push((phi.register, value));
                }
            }
            child_feedback.push(Feedback {
                from: *feedback,
                block: feedback_block,
                successors: edges[feedback].clone(),
                phi_inputs,
            });
        }
        // The original guard block owns the parent feedback frame.
        let guard_block = cfg.block_at[&child_guard];
        let mut parent_phi_inputs = Vec::new();
        for phi in &ssa.phis {
            if phi.block == pb && seen.contains(&phi.result) {
                let values: Vec<_> = phi
                    .incoming
                    .iter()
                    .filter(|(b, _)| *b == Some(guard_block))
                    .map(|(_, v)| *v)
                    .collect();
                let [v] = values[..] else { return None };
                parent_phi_inputs.push((phi.register, v));
            }
        }
        let mut parent_success_phi_inputs = Vec::new();
        if let Some(last) = success_tail.last() {
            let success_block = cfg.block_at[last];
            for phi in &ssa.phis {
                if phi.block == pb && seen.contains(&phi.result) {
                    let values: Vec<_> = phi
                        .incoming
                        .iter()
                        .filter(|(from, _)| *from == Some(success_block))
                        .map(|(_, value)| *value)
                        .collect();
                    let [value] = values[..] else { return None };
                    parent_success_phi_inputs.push((phi.register, value));
                }
            }
        }
        let terminal_nodes = parent_nodes
            .iter()
            .copied()
            .filter(|pc| edges[pc].is_empty())
            .collect();
        Some(Plan {
            parent,
            parent_guard,
            exit,
            bound,
            child,
            child_guard,
            child_latch,
            success_tail,
            siblings,
            child_feedback,
            parent_nodes: parent_nodes.into_iter().collect(),
            child_nodes: child_nodes.into_iter().collect(),
            terminal_nodes,
            parent_phi_inputs,
            parent_success_phi_inputs,
            edges: edges
                .iter()
                .flat_map(|(f, ts)| ts.iter().map(move |t| (*f, *t)))
                .collect(),
            widths: ir.instructions.iter().map(|i| (i.pc, i.width)).collect(),
            targets: ir
                .instructions
                .iter()
                .filter_map(|i| i.branch_target.map(|t| (i.pc, t)))
                .collect(),
            words: code.instructions.clone(),
            registers: code.registers,
            ins: code.ins,
            outs: code.outs,
        })
    };
    let mut plans = Vec::new();
    for (parent, child) in candidates {
        if let Some(plan) = prove(parent, child, &mut ssa_work) {
            if plans.len() >= 2
                || plans
                    .iter()
                    .map(|p: &Plan| p.parent_nodes.len())
                    .sum::<usize>()
                    + plan.parent_nodes.len()
                    > 256
                || plans.iter().any(|other: &Plan| {
                    !(plan.bound < other.parent || other.bound < plan.parent)
                        || plan
                            .parent_nodes
                            .iter()
                            .any(|pc| other.parent_nodes.contains(pc))
                })
            {
                return None;
            }
            plans.push(plan);
        }
        if ssa_work > 100_000 {
            return None;
        }
    }
    (!plans.is_empty()).then_some(plans)
}

fn child_success_tail(
    latch: usize,
    parent: usize,
    exit: usize,
    instructions: &BTreeMap<usize, &crate::native_ir::Instruction>,
    edges: &BTreeMap<usize, Vec<usize>>,
) -> Option<Vec<usize>> {
    let feedback = instructions.get(&latch)?;
    if matches!(feedback.opcode, 0x28..=0x2a) {
        return Some(vec![]);
    }
    if !matches!(feedback.opcode, 0x32..=0x3d) {
        return None;
    }
    let start = latch + feedback.width;
    let mut pc = start;
    let mut tail = Vec::new();
    loop {
        if tail.len() >= 16 || pc >= exit {
            return None;
        }
        let instruction = instructions.get(&pc)?;
        tail.push(pc);
        if matches!(instruction.opcode, 0x28..=0x2a) {
            if instruction.branch_target != Some(parent) || edges.get(&pc)?.as_slice() != [parent] {
                return None;
            }
            break;
        }
        let next = pc + instruction.width;
        if instruction.branch_target.is_some()
            || instruction.payload_target.is_some()
            || matches!(instruction.opcode, 0x0d..=0x11 | 0x1d | 0x1e | 0x27)
            || edges.get(&pc)?.as_slice() != [next]
        {
            return None;
        }
        pc = next;
    }
    // The success frame belongs to the conditional feedback fallthrough only.
    // Every other child exit retains its original independently checked frame.
    if edges.iter().any(|(from, targets)| {
        targets.iter().any(|to| {
            tail.contains(to) && !tail.contains(from) && !(*from == latch && *to == start)
        })
    }) {
        return None;
    }
    Some(tail)
}

// Earlier sibling loops retain their own ordinary renderer and checked frames.
// Admit only disjoint, single-entry natural pretest loops with one forward
// continuation. They cannot jump into the selected child or back to the parent.
#[allow(clippy::too_many_arguments)]
fn independent_siblings(
    parent: usize,
    child: usize,
    parent_nodes: &BTreeSet<usize>,
    child_nodes: &BTreeSet<usize>,
    headers: &BTreeSet<usize>,
    backward: &[(usize, usize)],
    instructions: &BTreeMap<usize, &crate::native_ir::Instruction>,
    edges: &BTreeMap<usize, Vec<usize>>,
    cfg: &ControlFlowGraph,
    dom: &DominatorTree,
) -> Option<Vec<Sibling>> {
    let mut siblings: Vec<Sibling> = Vec::new();
    for &header in headers {
        if header == parent || header == child || !parent_nodes.contains(&header) {
            continue;
        }
        if siblings.len() >= 8 || header <= parent || header >= child {
            return None;
        }
        let hb = *cfg.block_at.get(&header)?;
        let block = cfg.blocks.get(hb)?;
        if block.start != header || block.instructions.len() > 8 {
            return None;
        }
        let guard = instructions.get(block.instructions.last()?)?;
        if !matches!(guard.opcode, 0x32..=0x3d) {
            return None;
        }
        let exit = guard.branch_target?;
        let latches: Vec<_> = backward
            .iter()
            .filter(|(_, to)| *to == header)
            .map(|(from, _)| *from)
            .collect();
        let latch = *latches.last()?;
        if latches.len() > 8 || exit <= latch || exit > child {
            return None;
        }
        let nodes = reachable(header, edges, &BTreeSet::from([exit]))?;
        if nodes.len() > 128
            || !nodes.is_subset(parent_nodes)
            || !nodes.is_disjoint(child_nodes)
            || nodes
                .iter()
                .any(|pc| *pc < header || *pc > latch || !dom.dominates(hb, cfg.block_at[pc]))
            || latches.iter().any(|pc| !nodes.contains(pc))
            || siblings.iter().any(|other| other.exit > header)
            || edges.iter().any(|(from, targets)| {
                if nodes.contains(from) {
                    targets.iter().any(|to| !nodes.contains(to) && *to != exit)
                } else {
                    targets.iter().any(|to| nodes.contains(to) && *to != header)
                }
            })
        {
            return None;
        }
        let mut dag_edges = edges.clone();
        for from in &latches {
            dag_edges.get_mut(from)?.retain(|to| *to != header);
        }
        if !dag(&nodes, &dag_edges) {
            return None;
        }
        siblings.push(Sibling {
            header,
            latch,
            exit,
            nodes: nodes.into_iter().collect(),
        });
    }
    Some(siblings)
}

fn reachable(
    start: usize,
    edges: &BTreeMap<usize, Vec<usize>>,
    stop: &BTreeSet<usize>,
) -> Option<BTreeSet<usize>> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![start];
    let mut work = 0;
    while let Some(pc) = pending.pop() {
        work += 1;
        if work > 65_536 {
            return None;
        }
        if stop.contains(&pc) || !seen.insert(pc) {
            continue;
        }
        pending.extend(edges.get(&pc)?);
    }
    Some(seen)
}
fn dag(nodes: &BTreeSet<usize>, edges: &BTreeMap<usize, Vec<usize>>) -> bool {
    let mut indegree: BTreeMap<_, usize> = nodes.iter().map(|pc| (*pc, 0)).collect();
    for pc in nodes {
        for t in &edges[pc] {
            if let Some(v) = indegree.get_mut(t) {
                *v += 1
            }
        }
    }
    let mut todo: Vec<_> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(pc, _)| *pc)
        .collect();
    let mut count = 0;
    while let Some(pc) = todo.pop() {
        count += 1;
        for t in &edges[&pc] {
            if let Some(v) = indegree.get_mut(t) {
                *v -= 1;
                if *v == 0 {
                    todo.push(*t)
                }
            }
        }
    }
    count == nodes.len()
}

fn has_candidate(words: &[u16]) -> bool {
    let mut pc = 0usize;
    let mut backedges = 0;
    let mut conditional = false;
    let mut feedback = false;
    while pc < words.len() {
        if matches!(words[pc], 0x0100 | 0x0200) {
            let Some(count) = words.get(pc + 1).copied().map(usize::from) else {
                return false;
            };
            let width = if words[pc] == 0x0100 {
                4 + 2 * count
            } else {
                2 + 4 * count
            };
            if !pc.is_multiple_of(2) || count > 1024 || pc + width > words.len() {
                return false;
            }
            pc += width;
            continue;
        }
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
            | 0x2a..=0x2c
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
            conditional |= matches!(op, 0x32..=0x3d);
            feedback |= matches!(op, 0x28..=0x2a);
        }
        pc += width;
    }
    backedges >= 2 && conditional && feedback
}

// Exactly one narrow numeric field read feeds its following guard. Rendering
// keeps the read (including volatile access and null faults) inside each iteration.
fn narrow_field_guard(
    pcs: &[usize],
    ins: &BTreeMap<usize, &crate::native_ir::Instruction>,
) -> bool {
    let [load, test] = pcs else {
        return false;
    };
    let load = ins[load];
    let test = ins[test];
    matches!(load.opcode, 0x52 | 0x55..=0x58)
        && load.pc + load.width == test.pc
        && load.writes.len() == 1
        && test
            .reads
            .iter()
            .any(|operand| operand.register == load.writes[0].register)
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

// Switch dispatch remains in the original CFG outside the selected pair.
// Every skipped word is a uniquely referenced, validated switch payload.
fn switch_payload_layout(code: &DexCode, ir: &DecodedMethod) -> Option<Vec<(usize, usize)>> {
    if ir
        .instructions
        .iter()
        .all(|instruction| instruction.payload_target.is_none())
    {
        let mut end = 0;
        for instruction in &ir.instructions {
            if instruction.pc != end || matches!(instruction.opcode, 0x1d | 0x1e) {
                return None;
            }
            end = instruction.pc + instruction.width;
        }
        return (end == code.instructions.len()).then_some(Vec::new());
    }
    let mut referenced = BTreeSet::new();
    for instruction in &ir.instructions {
        if matches!(instruction.opcode, 0x1d | 0x1e) {
            return None;
        }
        if let Some(target) = instruction.payload_target
            && (!matches!(instruction.opcode, 0x2b | 0x2c) || !referenced.insert(target))
        {
            return None;
        }
    }
    let instructions: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let mut payloads = Vec::new();
    let mut pc = 0usize;
    while pc < code.instructions.len() {
        if let Some(instruction) = instructions.get(&pc) {
            pc = pc.checked_add(instruction.width)?;
            continue;
        }
        if !pc.is_multiple_of(2) || !referenced.remove(&pc) {
            return None;
        }
        let count = usize::from(*code.instructions.get(pc + 1)?);
        if count > 1024 {
            return None;
        }
        let width = match code.instructions[pc] {
            0x0100 => 4usize.checked_add(count.checked_mul(2)?)?,
            0x0200 => 2usize.checked_add(count.checked_mul(4)?)?,
            _ => return None,
        };
        let end = pc.checked_add(width)?;
        if end > code.instructions.len() || instructions.range(pc..end).next().is_some() {
            return None;
        }
        payloads.push((pc, end));
        pc = end;
    }
    referenced.is_empty().then_some(payloads)
}
