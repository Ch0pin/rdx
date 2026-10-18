//! Re-express a proven narrow-zero SSA read as Java null after physical local
//! materialization has consumed the literal spelling. No numerical value is cast.
use crate::{
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
    native_types::{AssignmentBound, InferredTypes},
};
use std::collections::{HashMap, HashSet};
pub(crate) fn at_read(
    ir: &DecodedMethod,
    ssa: &SsaMethod,
    types: &InferredTypes,
    pc: usize,
    register: usize,
) -> bool {
    prove(ir, ssa, types, pc, register, "null").is_some()
}
/// A Java int local may retain DEX literal alternatives from a narrower use.
/// This proves a value-preserving conversion at this exact operand read only.
pub(crate) fn narrow_literal_at_read(
    ir: &DecodedMethod,
    ssa: &SsaMethod,
    types: &InferredTypes,
    pc: usize,
    register: usize,
    descriptor: &str,
) -> bool {
    matches!(descriptor, "Z" | "B" | "S" | "C")
        && prove(ir, ssa, types, pc, register, descriptor).is_some()
}
fn fits(bits: i64, descriptor: &str) -> bool {
    match descriptor {
        "null" => bits == 0,
        "Z" => matches!(bits, 0 | 1),
        "B" => (-128..=127).contains(&bits),
        "S" => (-32768..=32767).contains(&bits),
        "C" => (0..=65535).contains(&bits),
        _ => false,
    }
}
fn prove(
    ir: &DecodedMethod,
    ssa: &SsaMethod,
    types: &InferredTypes,
    pc: usize,
    register: usize,
    descriptor: &str,
) -> Option<()> {
    if ssa.definitions.len() > 32768
        || ir.instructions.len() > 8192
        || ssa.graph.blocks.len() > 16384
        || types.values.len() != ssa.definitions.len()
        || ssa.reachable.len() != ssa.graph.blocks.len()
        || types.wide_pair_issues != 0
    {
        return None;
    }
    let raw: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let instructions: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    let phis: HashMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    if raw.len() != ir.instructions.len()
        || instructions.len() != ssa.instructions.len()
        || phis.len() != ssa.phis.len()
    {
        return None;
    }
    let instruction = instructions.get(&pc)?;
    let mut reads = instruction
        .reads
        .iter()
        .filter(|r| usize::from(r.register) == register);
    let read = reads.next()?;
    if read.words.len() != 1
        || reads.any(|other| other.words != read.words)
        || ssa.reachable.get(*ssa.graph.block_at.get(&pc)?) != Some(&true)
    {
        return None;
    }
    let mut preds = vec![HashSet::new(); ssa.graph.blocks.len()];
    let mut budget = 65536usize;
    for (from, b) in ssa.graph.blocks.iter().enumerate() {
        for edge in &b.successors {
            budget = budget.checked_sub(1)?;
            preds.get_mut(edge.target)?.insert(from);
        }
    }
    let mut pending = vec![read.words[0]];
    let mut seen = HashSet::new();
    let mut roots = 0;
    while let Some(id) = pending.pop() {
        budget = budget.checked_sub(1)?;
        if !seen.insert(id) {
            continue;
        }
        let ty = types.values.get(id)?;
        if ty.assignment_bounds_truncated
            || ty.assignment.is_empty()
            || !ty.assignment.iter().all(|bound| {
                matches!(bound,
                AssignmentBound::Literal { bits, wide: false } if fits(*bits, descriptor))
            })
        {
            return None;
        }
        let definition = ssa.definitions.get(id)?;
        match definition.kind {
            DefinitionKind::Instruction { pc, word: 0, block } => {
                if ssa.reachable.get(block) != Some(&true) {
                    return None;
                }
                let ins = raw.get(&pc)?;
                let effect = instructions.get(&pc)?;
                if effect.writes.len() != 1
                    || effect.writes[0].register != definition.register
                    || effect.writes[0].words.as_slice() != [id]
                {
                    return None;
                }
                match ins.opcode {
                    0x12..=0x15 if ins.literal.is_some_and(|bits| fits(bits, descriptor)) => {
                        roots += 1;
                    }
                    0x01..=0x03 if effect.reads.len() == 1 && effect.reads[0].words.len() == 1 => {
                        pending.push(effect.reads[0].words[0])
                    }
                    0x07..=0x09
                        if descriptor == "null"
                            && effect.reads.len() == 1
                            && effect.reads[0].words.len() == 1 =>
                    {
                        pending.push(effect.reads[0].words[0])
                    }
                    _ => return None,
                }
            }
            DefinitionKind::Phi { block } => {
                let phi = phis.get(&id)?;
                if phi.block != block
                    || phi.register != definition.register
                    || phi.incoming.is_empty()
                    || ssa.reachable.get(block) != Some(&true)
                {
                    return None;
                }
                let mut actual = HashSet::new();
                let mut listed = HashSet::new();
                for &(from, value) in &phi.incoming {
                    budget = budget.checked_sub(1)?;
                    if !listed.insert(from) || ssa.definitions.get(value)?.register != phi.register
                    {
                        return None;
                    }
                    let from = from?;
                    if !preds[block].contains(&from) {
                        return None;
                    }
                    if ssa.reachable.get(from) == Some(&true) {
                        actual.insert(from);
                        pending.push(value);
                    }
                }
                let expected: HashSet<_> = preds[block]
                    .iter()
                    .copied()
                    .filter(|&b| ssa.reachable[b])
                    .collect();
                if actual != expected || actual.is_empty() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (roots != 0).then_some(())
}
