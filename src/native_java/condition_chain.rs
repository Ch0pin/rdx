//! Consecutive pure comparisons share one original taken arm, in DEX order.
//! Constants loaded between the comparisons feed the next comparison only, and
//! the last comparison may branch past the shared arm instead of into it.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::{DexCode, DexTryRegion},
    native_ir::{DecodedMethod, Instruction},
    native_ssa::SsaMethod,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub branches: Vec<Instruction>,
    /// Narrow constants between branches, in DEX order. Each is read by the
    /// following branch alone and is dead on every other path.
    pub constants: Vec<Instruction>,
    /// The last branch falls into the shared arm and jumps past it.
    pub inverted: bool,
    pub target: usize,
    pub fallthrough: usize,
    words: Vec<u16>,
    regions: Vec<DexTryRegion>,
    frame: (u16, u16, u16, u16),
}

#[cfg(test)]
#[allow(dead_code)] // Exercised by the source-included integration proof fixtures.
pub(crate) fn validate(code: &DexCode, candidate: &Plan) -> bool {
    candidate
        .branches
        .first()
        .is_some_and(|first| plan(code, first.pc).as_ref() == Some(candidate))
}

pub(crate) fn matches(code: &DexCode, candidate: &Plan) -> bool {
    code.instructions == candidate.words
        && code.try_regions == candidate.regions
        && (code.registers, code.ins, code.outs, code.tries) == candidate.frame
}

const MAX_BRANCHES: usize = 8;
const MAX_CONSTANTS_PER_BRANCH: usize = 4;

fn constant_width(code: &DexCode, pc: usize) -> Option<usize> {
    match *code.instructions.get(pc)? as u8 {
        0x12 => Some(1),
        0x13 | 0x15 => Some(2),
        0x14 => Some(3),
        _ => None,
    }
}

pub(crate) fn plan(code: &DexCode, start: usize) -> Option<Plan> {
    let opcode_at = |pc: usize| code.instructions.get(pc).map(|word| *word as u8);
    let target_at = |pc: usize| -> Option<usize> {
        if !matches!(opcode_at(pc)?, 0x32..=0x3d) {
            return None;
        }
        usize::try_from(pc as i64 + i64::from(*code.instructions.get(pc + 1)? as i16)).ok()
    };
    // Most branches stop before decoding or constructing another CFG: the
    // next branch, after at most a few constants, shares the target or falls
    // into it.
    let target = target_at(start)?;
    let mut probe = start.checked_add(2)?;
    for _ in 0..MAX_CONSTANTS_PER_BRANCH {
        match constant_width(code, probe) {
            Some(width) => probe = probe.checked_add(width)?,
            None => break,
        }
    }
    let continues = target_at(probe) == Some(target)
        || (matches!(opcode_at(probe), Some(0x32..=0x3d)) && probe + 2 == target);
    if !continues
        || code.instructions.len() > 4096
        || code.registers > 256
        || usize::from(code.tries) != code.try_regions.len()
        || code.try_regions.len() > 16
        || code
            .try_regions
            .iter()
            .map(|region| region.catches.len())
            .sum::<usize>()
            > 64
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let first = ir
        .instructions
        .binary_search_by_key(&start, |i| i.pc)
        .ok()?;
    // Every chain instruction shares the protection of the first branch.
    let protected = |pc: usize| {
        code.try_regions.iter().all(|region| {
            (region.start as usize..region.end as usize).contains(&start)
                == (region.start as usize..region.end as usize).contains(&pc)
        })
    };
    let mut branches: Vec<Instruction> = Vec::new();
    let mut constants = Vec::new();
    let mut buffered = Vec::new();
    let mut inverted = false;
    let mut cursor = start;
    let mut after_branch = start;
    for instruction in ir.instructions.iter().skip(first) {
        if instruction.pc != cursor {
            break;
        }
        if matches!(instruction.opcode, 0x12..=0x15) && !branches.is_empty() {
            if buffered.len() == MAX_CONSTANTS_PER_BRANCH
                || instruction.may_throw
                || !instruction.reads.is_empty()
                || instruction.writes.len() != 1
                || instruction.writes[0].kind.word_count() != 1
                || instruction.writes[0].register >= code.registers
                || instruction.branch_target.is_some()
                || instruction.payload_target.is_some()
                || !protected(instruction.pc)
            {
                break;
            }
            buffered.push(instruction.clone());
            cursor += instruction.width;
            continue;
        }
        if !matches!(instruction.opcode, 0x32..=0x3d) {
            break;
        }
        let shared = instruction.branch_target == Some(target);
        let last_inverted =
            !shared && !branches.is_empty() && instruction.pc + instruction.width == target;
        if !shared && !last_inverted {
            break;
        }
        if branches.len() == MAX_BRANCHES
            || instruction.width != 2
            || instruction.may_throw
            || !instruction.writes.is_empty()
            || instruction.payload_target.is_some()
            || instruction.reads.len() != if instruction.opcode <= 0x37 { 2 } else { 1 }
            || instruction
                .reads
                .iter()
                .any(|read| read.kind.word_count() != 1 || read.register >= code.registers)
            || !protected(instruction.pc)
        {
            return None;
        }
        constants.append(&mut buffered);
        branches.push(instruction.clone());
        cursor += instruction.width;
        after_branch = cursor;
        if last_inverted {
            inverted = true;
            break;
        }
    }
    let is_instruction = |pc: usize| {
        pc < code.instructions.len() && ir.instructions.binary_search_by_key(&pc, |i| i.pc).is_ok()
    };
    let fallthrough = if inverted {
        let exit = branches.last()?.branch_target?;
        if exit <= target || !is_instruction(exit) {
            return None;
        }
        exit
    } else {
        after_branch
    };
    if branches.len() < 2 || (!inverted && fallthrough >= target) || !is_instruction(target) {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 512 {
        return None;
    }
    for (index, instruction) in branches.iter().enumerate() {
        let block_index = *cfg.block_at.get(&instruction.pc)?;
        let block = &cfg.blocks[block_index];
        if block.instructions.last() != Some(&instruction.pc) {
            return None;
        }
        if index > 0 {
            let previous = branches[index - 1].pc;
            let mut expected: Vec<usize> = constants
                .iter()
                .map(|constant| constant.pc)
                .filter(|&pc| previous < pc && pc < instruction.pc)
                .collect();
            expected.push(instruction.pc);
            if block.instructions != expected {
                return None;
            }
        }
        let mut actual: Vec<_> = block
            .successors
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Normal)
            .map(|edge| cfg.blocks[edge.target].start)
            .collect();
        let mut expected = vec![
            instruction.pc + instruction.width,
            instruction.branch_target?,
        ];
        actual.sort_unstable();
        expected.sort_unstable();
        if actual != expected {
            return None;
        }
        if index > 0 {
            let previous = *cfg.block_at.get(&branches[index - 1].pc)?;
            let incoming: Vec<_> = cfg
                .blocks
                .iter()
                .enumerate()
                .flat_map(|(from, block)| {
                    block.successors.iter().filter_map(move |edge| {
                        (edge.target == block_index).then_some((from, edge.kind))
                    })
                })
                .collect();
            if incoming != [(previous, EdgeKind::Normal)] {
                return None;
            }
        }
    }
    if !constants.is_empty() {
        // Each constant is read by the next branch alone and reaches no merge,
        // so evaluating it on every chain path is unobservable.
        let ssa = SsaMethod::build(code, &ir, &cfg).ok()?;
        for constant in &constants {
            let register = constant.writes[0].register;
            let consumer = branches.iter().find(|branch| branch.pc > constant.pc)?;
            let value = ssa.value_id(constant.pc, register, false)?;
            if !ssa.only_uses(value, &[(consumer.pc, register)])
                || ssa.value_id(consumer.pc, register, true) != Some(value)
                || !ssa.in_no_phi(value)
            {
                return None;
            }
        }
    }
    Some(Plan {
        branches,
        constants,
        inverted,
        target,
        fallthrough,
        words: code.instructions.clone(),
        regions: code.try_regions.clone(),
        frame: (code.registers, code.ins, code.outs, code.tries),
    })
}
