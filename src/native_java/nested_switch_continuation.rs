//! G06: canonical ownership for switch iterations containing nested natural loops.
//! The direct header latch need not be the last physical iteration instruction.
//! Shared abrupt exits can have independent callers, but cannot reenter the
//! natural loop. Every retained cycle must have a dominating natural header.
use super::switch_continuation_tail::Plan;
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_dominators::DominatorTree,
    native_ir::{DecodedMethod, Instruction},
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::collections::{BTreeMap, BTreeSet};

const WORK: usize = 2_000_000;
pub(super) struct CodeIdentity {
    words: Vec<u16>,
    registers: u16,
    ins: u16,
    outs: u16,
}
impl CodeIdentity {
    pub(super) fn new(code: &DexCode) -> Self {
        Self {
            words: code.instructions.clone(),
            registers: code.registers,
            ins: code.ins,
            outs: code.outs,
        }
    }
    pub(super) fn matches(&self, code: &DexCode) -> bool {
        code.tries == 0
            && code.try_regions.is_empty()
            && self.words == code.instructions
            && self.registers == code.registers
            && self.ins == code.ins
            && self.outs == code.outs
    }
}
fn diagnostic(header: usize, stage: &str) {
    if std::env::var_os("RDX_NESTED_LOOP_DIAGNOSTIC").is_some() {
        eprintln!("nested-loop {header:#x}: {stage}");
    }
}

pub(super) fn select_all(code: &DexCode) -> Vec<Plan> {
    select(code).unwrap_or_default()
}

fn select(code: &DexCode) -> Option<Vec<Plan>> {
    if code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() > 4096
        || code.registers > 256
        || code.ins > code.registers
    {
        return None;
    }
    if !code
        .instructions
        .iter()
        .any(|word| matches!(*word as u8, 0x2b | 0x2c))
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    if ir.instructions.len() > 2048
        || ir
            .instructions
            .iter()
            .any(|i| matches!(i.opcode, 0x1d | 0x1e))
    {
        return None;
    }
    let switches: Vec<_> = ir
        .instructions
        .iter()
        .filter(|i| matches!(i.opcode, 0x2b | 0x2c))
        .map(|i| i.pc)
        .collect();
    if switches.is_empty() || switches.len() > 8 {
        return None;
    }
    let at: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 512
        || cfg
            .blocks
            .iter()
            .any(|b| b.successors.iter().any(|e| e.kind != EdgeKind::Normal))
    {
        return None;
    }
    let dom = DominatorTree::compute_with_work_limit(&cfg, WORK / 4).ok()?;
    let mut work = WORK - WORK / 4;
    let mut inputs = None;
    let mut feedback: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (from, block) in cfg.blocks.iter().enumerate() {
        for edge in &block.successors {
            if dom.dominates(edge.target, from) {
                feedback.entry(edge.target).or_default().push(from);
            }
        }
    }
    if feedback.len() > 64 {
        return None;
    }
    let mut result = Vec::new();
    for (&hb, latches) in &feedback {
        let header = cfg.blocks[hb].start;
        let mut natural = BTreeSet::from([hb]);
        let mut pending = latches.clone();
        let mut valid = true;
        while let Some(block) = pending.pop() {
            work = work.checked_sub(1)?;
            if natural.contains(&block) {
                continue;
            }
            if block >= cfg.blocks.len() || !dom.dominates(hb, block) {
                valid = false;
                break;
            }
            natural.insert(block);
            pending.extend(dom.predecessors[block].iter().copied());
        }
        if !valid {
            diagnostic(header, "natural ownership");
            continue;
        }
        let owned_switches: Vec<_> = switches
            .iter()
            .copied()
            .filter(|pc| natural.contains(&cfg.block_at[pc]))
            .collect();
        if owned_switches.is_empty() {
            continue;
        }
        let Some(latch) = latches
            .iter()
            .map(|b| *cfg.blocks[*b].instructions.last().unwrap())
            .max()
        else {
            continue;
        };
        if latches.iter().any(|b| {
            let i = at[cfg.blocks[*b].instructions.last().unwrap()];
            !matches!(i.opcode, 0x28..=0x2a) || i.branch_target != Some(header)
        }) {
            diagnostic(header, "conditional feedback");
            continue;
        }
        let natural_end = natural.iter().map(|b| cfg.blocks[*b].end).max()?;
        if natural_end <= latch + at[&latch].width {
            continue;
        }
        // Only a real outward guard beyond every natural iteration node can
        // become the normal continuation. Null/error exits are abrupt paths.
        let mut exits = BTreeSet::new();
        for &block in &natural {
            for edge in &cfg.blocks[block].successors {
                work = work.checked_sub(1)?;
                if !natural.contains(&edge.target) {
                    let target = cfg.blocks[edge.target].start;
                    if target >= natural_end && !linear_terminal(target, &at, &mut work)? {
                        exits.insert(target);
                    }
                }
            }
        }
        if exits.len() != 1 {
            diagnostic(header, "unique forward normal exit");
            continue;
        }
        let exit = *exits.first()?;
        let guards: Vec<_> = natural
            .iter()
            .filter_map(|&block| {
                let pc = *cfg.blocks[block].instructions.last()?;
                let i = at[&pc];
                (matches!(i.opcode, 0x32..=0x3d)
                    && i.branch_target == Some(exit)
                    && cfg
                        .block_at
                        .get(&(pc + i.width))
                        .is_some_and(|b| natural.contains(b))
                    && latches.iter().all(|latch| dom.dominates(block, *latch)))
                .then_some(pc)
            })
            .collect();
        if guards.len() != 1 {
            diagnostic(header, "unique dominating normal guard");
            continue;
        }
        let guard = guards[0];
        if !header_prefix(header, guard, &at, &mut work)? {
            diagnostic(header, "nonlinear header prefix");
            continue;
        }
        let mut blocks = BTreeSet::new();
        let mut terminal_owned = BTreeSet::new();
        let mut pending = vec![hb];
        while let Some(block) = pending.pop() {
            work = work.checked_sub(1)?;
            if blocks.contains(&block) {
                continue;
            }
            let start = cfg.blocks[block].start;
            if start == exit {
                continue;
            }
            if start < header || start >= exit {
                if let Some(path) = terminal_path(start, &at, &mut work)? {
                    terminal_owned.extend(path);
                } else {
                    valid = false;
                    break;
                }
                continue;
            }
            blocks.insert(block);
            for edge in &cfg.blocks[block].successors {
                if edge.target != hb {
                    pending.push(edge.target);
                }
            }
        }
        if !valid || !natural.is_subset(&blocks) {
            diagnostic(header, "iteration extent");
            continue;
        }
        if blocks.iter().any(|b| {
            cfg.blocks[*b].successors.iter().any(|edge| {
                cfg.blocks[edge.target].start == exit
                    && cfg.blocks[*b].instructions.last() != Some(&guard)
            })
        }) {
            diagnostic(header, "non-guard normal exit frame");
            continue;
        }
        // An independently entered escape island can be duplicated by a Java
        // branch only when it cannot flow back into any natural iteration node.
        let escaping: BTreeSet<_> = blocks.difference(&natural).copied().collect();
        for &block in &escaping {
            if cfg.blocks[block]
                .successors
                .iter()
                .any(|edge| natural.contains(&edge.target))
            {
                valid = false;
                break;
            }
        }
        if !valid {
            diagnostic(header, "shared escape reentry");
            continue;
        }
        // Contracting natural feedback edges must leave an acyclic graph.
        // This proves nested reducible loops without granting arbitrary cycles.
        let mut incoming: BTreeMap<_, usize> = blocks.iter().map(|b| (*b, 0)).collect();
        let mut dag: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for &from in &blocks {
            for edge in &cfg.blocks[from].successors {
                work = work.checked_sub(1)?;
                if blocks.contains(&edge.target) && !dom.dominates(edge.target, from) {
                    dag.entry(from).or_default().push(edge.target);
                    *incoming.get_mut(&edge.target)? += 1;
                }
            }
        }
        let mut ready: Vec<_> = incoming
            .iter()
            .filter_map(|(b, n)| (*n == 0).then_some(*b))
            .collect();
        let mut count = 0;
        while let Some(block) = ready.pop() {
            work = work.checked_sub(1)?;
            count += 1;
            for next in dag.get(&block).into_iter().flatten() {
                let n = incoming.get_mut(next)?;
                *n -= 1;
                if *n == 0 {
                    ready.push(*next);
                }
            }
        }
        if count != blocks.len() {
            diagnostic(header, "irreducible iteration cycle");
            continue;
        }
        let owned: BTreeSet<_> = blocks
            .iter()
            .flat_map(|b| cfg.blocks[*b].instructions.iter().copied())
            .collect();
        // Keep input provenance complete over late tails and nested islands,
        // including phis and every dependency of a value consumed there.
        if inputs.is_none() {
            inputs = Some(InputProof::build(code, &ir, &cfg, &mut work)?);
        }
        let mut provenance = owned.clone();
        provenance.extend(terminal_owned);
        if !inputs.as_ref()?.complete(&provenance) {
            diagnostic(header, "incomplete SSA input");
            continue;
        }
        let mut edges = BTreeSet::new();
        for (from, block) in cfg.blocks.iter().enumerate() {
            let pc = *block.instructions.last()?;
            for edge in &block.successors {
                let target = cfg.blocks[edge.target].start;
                if blocks.contains(&from) || escaping.contains(&edge.target) {
                    edges.insert((pc, target));
                }
            }
        }
        diagnostic(header, "certified nested switch iteration");
        for switch in owned_switches {
            result.push(Plan {
                header,
                guard,
                latch,
                exit,
                switch,
                owned: owned.clone(),
                edges: edges.clone(),
            });
        }
    }
    Some(result)
}

fn linear_terminal(
    pc: usize,
    at: &BTreeMap<usize, &Instruction>,
    work: &mut usize,
) -> Option<bool> {
    Some(terminal_path(pc, at, work)?.is_some())
}
fn terminal_path(
    mut pc: usize,
    at: &BTreeMap<usize, &Instruction>,
    work: &mut usize,
) -> Option<Option<Vec<usize>>> {
    let mut path = Vec::new();
    for _ in 0..64 {
        *work = work.checked_sub(1)?;
        let Some(i) = at.get(&pc) else {
            return Some(None);
        };
        path.push(pc);
        if matches!(i.opcode, 0x0e..=0x11 | 0x27) {
            return Some(Some(path));
        }
        if matches!(i.opcode, 0x2b..=0x3d | 0x0d) {
            return Some(None);
        }
        pc = if matches!(i.opcode, 0x28..=0x2a) {
            let target = i.branch_target?;
            if target <= pc {
                return Some(None);
            }
            target
        } else {
            pc + i.width
        };
    }
    Some(None)
}

fn header_prefix(
    mut pc: usize,
    guard: usize,
    at: &BTreeMap<usize, &Instruction>,
    work: &mut usize,
) -> Option<bool> {
    for _ in 0..256 {
        *work = work.checked_sub(1)?;
        if pc == guard {
            return Some(true);
        }
        let i = at.get(&pc)?;
        if pc > guard || matches!(i.opcode, 0x0e..=0x11 | 0x27 | 0x2b | 0x2c) {
            return Some(false);
        }
        if matches!(i.opcode, 0x32..=0x3d) {
            let target = i.branch_target?;
            if target <= guard || !linear_terminal(target, at, work)? {
                return Some(false);
            }
        }
        pc = if matches!(i.opcode, 0x28..=0x2a) {
            let target = i.branch_target?;
            if target <= pc {
                return Some(false);
            }
            target
        } else {
            pc + i.width
        };
    }
    Some(false)
}

struct InputProof {
    reads: BTreeMap<usize, Vec<usize>>,
    incomplete: BTreeSet<usize>,
}
impl InputProof {
    fn build(
        code: &DexCode,
        ir: &DecodedMethod,
        cfg: &ControlFlowGraph,
        work: &mut usize,
    ) -> Option<Self> {
        // Reserve fixed analysis work once, shared by every selected loop.
        let reserve = *work / 2;
        *work = work.checked_sub(reserve)?;
        let ssa = SsaMethod::build_with_local_and_dominator_limits(
            code,
            ir,
            cfg,
            reserve / 2,
            reserve / 2,
        )
        .ok()?;
        let reads: BTreeMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
        let phis: BTreeMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
        let mut dependents = vec![Vec::new(); ssa.definitions.len()];
        let mut pending = Vec::new();
        for (id, definition) in ssa.definitions.iter().enumerate() {
            *work = work.checked_sub(1)?;
            let dependencies: Vec<_> = match &definition.kind {
                DefinitionKind::Undefined => {
                    pending.push(id);
                    vec![]
                }
                DefinitionKind::Parameter => vec![],
                DefinitionKind::Instruction { pc, .. } => reads
                    .get(pc)?
                    .reads
                    .iter()
                    .flat_map(|r| r.words.iter().copied())
                    .collect(),
                DefinitionKind::Phi { .. } => phis
                    .get(&id)?
                    .incoming
                    .iter()
                    .map(|(_, value)| *value)
                    .collect(),
            };
            for dependency in dependencies {
                *work = work.checked_sub(1)?;
                dependents.get_mut(dependency)?.push(id);
            }
        }
        let mut incomplete = BTreeSet::new();
        while let Some(id) = pending.pop() {
            *work = work.checked_sub(1)?;
            if incomplete.insert(id) {
                pending.extend(dependents[id].iter().copied());
            }
        }
        Some(Self {
            reads: reads
                .iter()
                .map(|(pc, i)| {
                    (
                        *pc,
                        i.reads
                            .iter()
                            .flat_map(|r| r.words.iter().copied())
                            .collect(),
                    )
                })
                .collect(),
            incomplete,
        })
    }
    fn complete(&self, owned: &BTreeSet<usize>) -> bool {
        owned.iter().all(|pc| {
            self.reads
                .get(pc)
                .is_some_and(|reads| reads.iter().all(|id| !self.incomplete.contains(id)))
        })
    }
}
