//! Consecutive pure comparisons share one original taken arm, in DEX order.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::{DexCode, DexTryRegion},
    native_ir::{DecodedMethod, Instruction},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub branches: Vec<Instruction>,
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

pub(crate) fn plan(code: &DexCode, start: usize) -> Option<Plan> {
    let target_at = |pc: usize| -> Option<usize> {
        if !matches!(*code.instructions.get(pc)? as u8, 0x32..=0x3d) {
            return None;
        }
        usize::try_from(pc as i64 + i64::from(*code.instructions.get(pc + 1)? as i16)).ok()
    };
    // Most branches stop before decoding or constructing another CFG.
    let target = target_at(start)?;
    if target_at(start.checked_add(2)?) != Some(target)
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
    let mut branches = Vec::new();
    let mut next = start;
    for instruction in ir.instructions.iter().skip(first) {
        if instruction.pc != next
            || !matches!(instruction.opcode, 0x32..=0x3d)
            || instruction.branch_target != Some(target)
        {
            break;
        }
        if branches.len() == 8
            || instruction.width != 2
            || instruction.may_throw
            || !instruction.writes.is_empty()
            || instruction.payload_target.is_some()
            || instruction.reads.len() != if instruction.opcode <= 0x37 { 2 } else { 1 }
            || instruction
                .reads
                .iter()
                .any(|read| read.kind.word_count() != 1 || read.register >= code.registers)
            || code.try_regions.iter().any(|region| {
                (region.start as usize <= start && start < region.end as usize)
                    != (region.start as usize <= next && next < region.end as usize)
            })
        {
            return None;
        }
        branches.push(instruction.clone());
        next += instruction.width;
    }
    if branches.len() < 2
        || next >= target
        || target >= code.instructions.len()
        || ir
            .instructions
            .binary_search_by_key(&target, |i| i.pc)
            .is_err()
    {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 512 {
        return None;
    }
    for (index, instruction) in branches.iter().enumerate() {
        let block_index = *cfg.block_at.get(&instruction.pc)?;
        let block = &cfg.blocks[block_index];
        if block.instructions.last() != Some(&instruction.pc)
            || (index > 0 && block.instructions != [instruction.pc])
        {
            return None;
        }
        let mut actual: Vec<_> = block
            .successors
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Normal)
            .map(|edge| cfg.blocks[edge.target].start)
            .collect();
        let mut expected = vec![instruction.pc + instruction.width, target];
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
    Some(Plan {
        branches,
        target,
        fallthrough: next,
        words: code.instructions.clone(),
        regions: code.try_regions.clone(),
        frame: (code.registers, code.ins, code.outs, code.tries),
    })
}
