//! Exact raw constant provenance at one original DEX read. No type assignment.
use crate::{
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
    native_types::{AssignmentBound, InferredTypes},
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Domain {
    Boolean,
    Raw32,
    Raw64,
}

pub(crate) fn proves_read(
    ir: &DecodedMethod,
    ssa: &SsaMethod,
    types: &InferredTypes,
    pc: usize,
    register: usize,
    domain: Domain,
) -> bool {
    if ir.instructions.len() > 8192
        || ssa.definitions.len() > 32768
        || types.values.len() != ssa.definitions.len()
    {
        return false;
    }
    let width = if domain == Domain::Raw64 { 2 } else { 1 };
    let Some(raw) = ir.instructions.iter().find(|i| i.pc == pc) else {
        return false;
    };
    let Some(read) = ssa.instructions.iter().find(|i| i.pc == pc) else {
        return false;
    };
    let raw_reads: Vec<_> = raw
        .reads
        .iter()
        .filter(|r| usize::from(r.register) == register)
        .collect();
    let reads: Vec<_> = read
        .reads
        .iter()
        .filter(|r| usize::from(r.register) == register)
        .collect();
    if raw_reads.len() != 1
        || reads.len() != 1
        || raw_reads[0].kind.word_count() != width
        || reads[0].words.len() != width
    {
        return false;
    }
    let instructions: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let operands: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    let phis: HashMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    if instructions.len() != ir.instructions.len()
        || operands.len() != ssa.instructions.len()
        || phis.len() != ssa.phis.len()
    {
        return false;
    }
    let mut pending = vec![reads[0].words.clone()];
    let mut seen = HashSet::new();
    let mut budget = 65536;
    let mut roots = 0;
    while let Some(words) = pending.pop() {
        if budget == 0 {
            return false;
        }
        budget -= 1;
        if !seen.insert(words.clone()) {
            continue;
        }
        if words.len() != width {
            return false;
        }
        for &id in &words {
            let Some(v) = types.values.get(id) else {
                return false;
            };
            if v.bounds_truncated || v.assignment.is_empty() || !v.assignment.iter().all(|a|matches!(a,AssignmentBound::Literal{bits,wide} if *wide==(width==2) && (domain!=Domain::Boolean || matches!(bits,0|1)))){return false;}
        }
        let Some(low) = ssa.definitions.get(words[0]) else {
            return false;
        };
        if width == 2 {
            let Some(high) = ssa.definitions.get(words[1]) else {
                return false;
            };
            if low.register.checked_add(1) != Some(high.register)
                || types.values[words[0]].assignment != types.values[words[1]].assignment
            {
                return false;
            }
            match (&low.kind, &high.kind) {
                (
                    DefinitionKind::Instruction { pc: a, word: 0, .. },
                    DefinitionKind::Instruction { pc: b, word: 1, .. },
                ) if a == b => {}
                (DefinitionKind::Phi { block: a }, DefinitionKind::Phi { block: b }) if a == b => {}
                _ => return false,
            }
        }
        match low.kind {
            DefinitionKind::Parameter | DefinitionKind::Undefined => return false,
            DefinitionKind::Phi { block } => {
                let Some(phi) = phis.get(&words[0]) else {
                    return false;
                };
                if phi.block != block
                    || phi.register != low.register
                    || ssa.reachable.get(block) != Some(&true)
                    || phi.incoming.is_empty()
                {
                    return false;
                }
                let high = if width == 2 {
                    let Some(high) = phis.get(&words[1]) else {
                        return false;
                    };
                    if high.block != block || high.incoming.len() != phi.incoming.len() {
                        return false;
                    }
                    Some(high)
                } else {
                    None
                };
                let mut incoming = 0;
                for (index, &(from, id)) in phi.incoming.iter().enumerate() {
                    let high_id = if let Some(high) = high {
                        let (other, high_id) = high.incoming[index];
                        if from != other {
                            return false;
                        }
                        Some(high_id)
                    } else {
                        None
                    };
                    if from.is_some_and(|b| ssa.reachable.get(b) != Some(&true)) {
                        continue;
                    }
                    incoming += 1;
                    let mut next = vec![id];
                    if let Some(high) = high_id {
                        next.push(high);
                    }
                    pending.push(next);
                }
                if incoming == 0 {
                    return false;
                }
            }
            DefinitionKind::Instruction { pc, word, .. } => {
                if word != 0 {
                    return false;
                }
                let Some(raw) = instructions.get(&pc) else {
                    return false;
                };
                let Some(ins) = operands.get(&pc) else {
                    return false;
                };
                if ins.writes.len() != 1
                    || ins.writes[0].words != words
                    || raw.writes.len() != 1
                    || raw.writes[0].kind.word_count() != width
                {
                    return false;
                }
                if (width == 1 && matches!(raw.opcode, 0x12..=0x15))
                    || (width == 2 && matches!(raw.opcode, 0x16..=0x19))
                {
                    let Some(bits) = raw.literal else {
                        return false;
                    };
                    if types.values[words[0]].assignment.as_slice()
                        != [AssignmentBound::Literal {
                            bits,
                            wide: width == 2,
                        }]
                        || (domain == Domain::Boolean && !matches!(bits, 0 | 1))
                    {
                        return false;
                    }
                    roots += 1;
                } else if (width == 1 && matches!(raw.opcode, 0x01..=0x03))
                    || (width == 2 && matches!(raw.opcode, 0x04..=0x06))
                {
                    if raw.reads.len() != 1
                        || raw.reads[0].kind.word_count() != width
                        || ins.reads.len() != 1
                        || ins.reads[0].words.len() != width
                    {
                        return false;
                    }
                    pending.push(ins.reads[0].words.clone());
                } else {
                    return false;
                }
            }
        }
    }
    roots != 0
}
