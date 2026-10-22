//! A conditional loop latch may skip its guard's default assignment with a
//! forward goto. Every instruction in that exit span is pure, every edge is
//! canonical, and all entries are owned by the original loop interval.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_ir::DecodedMethod,
};
pub(crate) fn admits(code: &DexCode, header: usize, tail: usize, exit: usize) -> bool {
    prove(code, header, tail, exit).is_some()
}
fn prove(code: &DexCode, header: usize, tail: usize, exit: usize) -> Option<()> {
    if code.instructions.len() > 4096
        || code.registers > 256
        || usize::from(code.tries) != code.try_regions.len()
        || code.try_regions.len() > 16
        || code
            .try_regions
            .iter()
            .map(|region| region.catches.len())
            .sum::<usize>()
            > 64
        || !(header < tail && tail < exit && exit < code.instructions.len())
        || exit - tail > 64
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 512
        || cfg.blocks[*cfg.block_at.get(&header)?].start != header
        || (!code.try_regions.is_empty() && cfg.blocks[*cfg.block_at.get(&tail)?].start != tail)
        || cfg.blocks[*cfg.block_at.get(&exit)?].start != exit
    {
        return None;
    }
    let span: Vec<_> = ir
        .instructions
        .iter()
        .filter(|i| (tail..exit).contains(&i.pc))
        .collect();
    let mut end = tail;
    for i in &span {
        if i.pc != end
            || i.may_throw
            || !matches!(i.opcode,0x00..=0x09|0x12..=0x19|0x28..=0x2a)
            || i.opcode == 0 && code.instructions[i.pc] != 0
        {
            return None;
        }
        end = i.pc + i.width;
        let next = if matches!(i.opcode, 0x28..=0x2a) {
            i.branch_target?
        } else {
            end
        };
        if next <= i.pc || next > exit {
            return None;
        }
        let b = &cfg.blocks[*cfg.block_at.get(&i.pc)?];
        let actual = if end < b.end {
            vec![end]
        } else {
            // DEX CFG conservatively retains protection on pure instructions.
            // Check those original dispatch edges without projecting a new CFG;
            // the independently decoded nonthrowing span uses its normal edge.
            for edge in b
                .successors
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Exceptional)
            {
                let handler = cfg.blocks[edge.target].start;
                if (tail..=exit).contains(&handler)
                    || !code.try_regions.iter().any(|region| {
                        region.start as usize <= i.pc
                            && i.pc < region.end as usize
                            && region
                                .catches
                                .iter()
                                .any(|(_, target)| *target as usize == handler)
                    })
                {
                    return None;
                }
            }
            b.successors
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Normal)
                .map(|e| cfg.blocks[e.target].start)
                .collect()
        };
        if actual != [next] {
            return None;
        }
    }
    if span.is_empty() || end != exit {
        return None;
    }
    for b in &cfg.blocks {
        let from = *b.instructions.last()?;
        for edge in &b.successors {
            let to = cfg.blocks.get(edge.target)?.start;
            if (tail..exit).contains(&to)
                && !(tail..exit).contains(&from)
                && (edge.kind != EdgeKind::Normal || !(header..tail).contains(&from))
            {
                return None;
            }
        }
    }
    Some(())
}
