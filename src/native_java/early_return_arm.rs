//! A canonical pure terminal arm can be emitted before continuing the other
//! edge in the current frame, avoiding recursive nesting of acyclic guard chains.
use crate::{native_cfg::ControlFlowGraph, native_dex::DexCode, native_ir::DecodedMethod};
// One/two-guard diamonds keep their established source plans. A longer proved
// sequence is flattened from its first guard, before recursive frames grow.
pub(crate) const MIN_GUARDS: usize = 3;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub branch: usize,
    pub branch_instruction: crate::native_ir::Instruction,
    pub entry: usize,
    pub continuation: usize,
    pub taken: bool,
    pub terminal_end: usize,
    pub instructions: Vec<usize>,
    pub decoded: Vec<crate::native_ir::Instruction>,
}
#[cfg(test)]
#[allow(dead_code)] // Direct single-arm proof fixture API; production selects sequences.
pub(crate) fn select(code: &DexCode, branch: usize, stop: usize) -> Option<Plan> {
    if code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() > 4096
        || stop > code.instructions.len()
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    select_decoded(&ir, &cfg, branch, stop)
}

pub(crate) fn sequence(code: &DexCode, branch: usize, stop: usize) -> Option<Vec<Plan>> {
    if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 4096 {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    sequence_decoded(&ir, &cfg, branch, stop)
}

pub(crate) fn sequence_decoded(
    ir: &DecodedMethod,
    cfg: &ControlFlowGraph,
    mut branch: usize,
    stop: usize,
) -> Option<Vec<Plan>> {
    let mut plans = Vec::new();
    for index in 0..512 {
        let Some(plan) = select_decoded(ir, cfg, branch, stop) else {
            break;
        };
        let continuation = plan.continuation;
        plans.push(plan);
        let Some(next) = next_guard(ir, cfg, continuation, stop) else {
            break;
        };
        if next <= branch {
            return None;
        }
        if index == 511 {
            return None;
        }
        branch = next;
    }
    (plans.len() >= MIN_GUARDS).then_some(plans)
}

// Prefix effects stay at their original instruction positions. This walk
// only proves a unique forward route to the next guard; it emits no prefix.
fn next_guard(
    ir: &DecodedMethod,
    cfg: &ControlFlowGraph,
    mut pc: usize,
    stop: usize,
) -> Option<usize> {
    for _ in 0..128 {
        if pc >= stop {
            return None;
        }
        let i = ir.instructions.iter().find(|i| i.pc == pc)?;
        if matches!(i.opcode, 0x32..=0x3d) {
            return Some(pc);
        }
        if matches!(i.opcode, 0x0d..=0x11 | 0x1d | 0x1e | 0x27 | 0x2b | 0x2c)
            || i.payload_target.is_some()
            || i.width == 0
        {
            return None;
        }
        let next = if matches!(i.opcode, 0x28..=0x2a) {
            i.branch_target?
        } else {
            if i.branch_target.is_some() {
                return None;
            }
            pc + i.width
        };
        if next <= pc || next >= stop {
            return None;
        }
        let b = cfg.blocks.get(*cfg.block_at.get(&pc)?)?;
        if pc + i.width < b.end {
            if next != pc + i.width {
                return None;
            }
        } else if b.successors.len() != 1
            || b.successors[0].kind != crate::native_cfg::EdgeKind::Normal
            || cfg.blocks.get(b.successors[0].target)?.start != next
        {
            return None;
        }
        pc = next;
    }
    None
}

// Shared rendering consumes its authoritative IR and CFG, including when raw
// branch words have been poisoned by contract tests. Never rediscover a raw
// plan in preference to those established canonical instructions.
pub(crate) fn select_decoded(
    ir: &DecodedMethod,
    cfg: &ControlFlowGraph,
    branch: usize,
    stop: usize,
) -> Option<Plan> {
    let end = ir.instructions.last().map(|i| i.pc + i.width)?;
    if end > 4096
        || stop > end
        || cfg.blocks.len() > 512
        || cfg
            .blocks
            .iter()
            .flat_map(|b| &b.successors)
            .any(|e| e.kind != crate::native_cfg::EdgeKind::Normal)
    {
        return None;
    }
    let instruction = ir.instructions.iter().find(|i| i.pc == branch)?;
    if !matches!(instruction.opcode, 0x32..=0x3d) || instruction.width != 2 {
        return None;
    }
    let target = instruction.branch_target?;
    let fallthrough = branch + instruction.width;
    let (entry, continuation, taken, path) = if let Some(path) = pure_tail(ir, fallthrough, stop) {
        (fallthrough, target, false, path)
    } else {
        (target, fallthrough, true, pure_tail(ir, target, stop)?)
    };
    if entry <= branch
        || continuation <= branch
        || continuation >= stop
        || path.contains(&continuation)
    {
        return None;
    }
    let block = &cfg.blocks[*cfg.block_at.get(&branch)?];
    let mut successors: Vec<_> = block
        .successors
        .iter()
        .map(|e| cfg.blocks[e.target].start)
        .collect();
    successors.sort_unstable();
    let mut expected = vec![target, fallthrough];
    expected.sort_unstable();
    if block.instructions.last() != Some(&branch) || successors != expected {
        return None;
    }
    for (index, &pc) in path.iter().enumerate() {
        let instruction = ir.instructions.iter().find(|i| i.pc == pc)?;
        let b = &cfg.blocks[*cfg.block_at.get(&pc)?];
        let expected = path.get(index + 1).copied();
        if let Some(next) = expected {
            let actual = if pc + instruction.width < b.end {
                vec![pc + instruction.width]
            } else {
                b.successors
                    .iter()
                    .map(|e| cfg.blocks[e.target].start)
                    .collect()
            };
            if actual != [next] {
                return None;
            }
        } else if !matches!(instruction.opcode, 0x0e..=0x11) || !b.successors.is_empty() {
            return None;
        }
    }
    let last = *path.last()?;
    let width = ir.instructions.iter().find(|i| i.pc == last)?.width;
    let decoded = path
        .iter()
        .map(|pc| ir.instructions.iter().find(|i| i.pc == *pc).cloned())
        .collect::<Option<Vec<_>>>()?;
    Some(Plan {
        branch,
        branch_instruction: instruction.clone(),
        entry,
        continuation,
        taken,
        terminal_end: last + width,
        instructions: path,
        decoded,
    })
}
fn pure_tail(ir: &DecodedMethod, mut pc: usize, stop: usize) -> Option<Vec<usize>> {
    let mut path = vec![];
    while path.len() < 16 {
        if pc >= stop {
            return None;
        }
        let instruction = ir.instructions.iter().find(|i| i.pc == pc)?;
        let op = instruction.opcode;
        let width = instruction.width;
        if width == 0
            || pc + width > stop
            || instruction.may_throw
            || instruction.payload_target.is_some()
        {
            return None;
        }
        path.push(pc);
        if matches!(op, 0x0e..=0x11) {
            return Some(path);
        }
        if matches!(op, 0x28..=0x2a) {
            let next = instruction.branch_target?;
            if next <= pc {
                return None;
            }
            pc = next;
        } else if matches!(op,0x00..=0x09|0x12..=0x19) && instruction.branch_target.is_none() {
            pc += width;
        } else {
            return None;
        }
    }
    None
}
