//! Use-specific proof that a raw throw reads narrow zero on every reaching path.
//! Never assigns a type or synthesizes a register definition.
use crate::{
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
    native_types::{AssignmentBound, InferredTypes},
};
use std::collections::{HashMap, HashSet};

pub(crate) fn proves_read(
    ir: &DecodedMethod,
    ssa: &SsaMethod,
    types: &InferredTypes,
    pc: usize,
    register: usize,
) -> bool {
    if ir.instructions.len() > 8192
        || ssa.definitions.len() > 32768
        || types.values.len() != ssa.definitions.len()
        || types.wide_pair_issues != 0
    {
        return false;
    }
    let Some(raw) = ir.instructions.iter().find(|i| i.pc == pc) else {
        return false;
    };
    let Some(read) = ssa.instructions.iter().find(|i| i.pc == pc) else {
        return false;
    };
    if raw.opcode != 0x27
        || raw.width != 1
        || raw.reads.len() != 1
        || usize::from(raw.reads[0].register) != register
        || raw.reads[0].kind.word_count() != 1
        || read.reads.len() != 1
        || read.reads[0].words.len() != 1
        || usize::from(read.reads[0].register) != register
    {
        return false;
    }
    let instructions: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let operands: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    let phis: HashMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    let mut work = vec![read.reads[0].words[0]];
    let mut seen = HashSet::new();
    let mut budget = 65536;
    let mut literal_root = false;
    while let Some(id) = work.pop() {
        if budget == 0 {
            return false;
        }
        budget -= 1;
        if !seen.insert(id) {
            continue;
        }
        let Some(ty) = types.values.get(id) else {
            return false;
        };
        if ty.bounds_truncated
            || ty.assignment.as_slice()
                != [AssignmentBound::Literal {
                    bits: 0,
                    wide: false,
                }]
        {
            return false;
        }
        let Some(definition) = ssa.definitions.get(id) else {
            return false;
        };
        match definition.kind {
            DefinitionKind::Parameter | DefinitionKind::Undefined => return false,
            DefinitionKind::Phi { .. } => {
                let Some(phi) = phis.get(&id) else {
                    return false;
                };
                let mut incoming = false;
                for &(from, value) in &phi.incoming {
                    if from.is_some_and(|block| !ssa.reachable.get(block).copied().unwrap_or(false))
                    {
                        continue;
                    }
                    incoming = true;
                    work.push(value);
                }
                if !incoming {
                    return false;
                }
            }
            DefinitionKind::Instruction { pc, word, .. } => {
                if word != 0 {
                    return false;
                }
                let Some(instruction) = instructions.get(&pc) else {
                    return false;
                };
                match instruction.opcode {
                    0x12..=0x15
                        if instruction.literal == Some(0)
                            && instruction.writes.len() == 1
                            && instruction.writes[0].kind.word_count() == 1 =>
                    {
                        literal_root = true;
                    }
                    0x01..=0x03 | 0x07..=0x09 => {
                        let Some(values) = operands.get(&pc) else {
                            return false;
                        };
                        if values.reads.len() != 1
                            || values.reads[0].words.len() != 1
                            || values.writes.len() != 1
                            || values.writes[0].words.len() != 1
                        {
                            return false;
                        }
                        work.push(values.reads[0].words[0]);
                    }
                    _ => return false,
                }
            }
        }
    }
    // A closed phi/copy cycle alone cannot establish an initialized zero.
    literal_root
}
