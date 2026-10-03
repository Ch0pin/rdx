//! Choose double storage for a coherent loop phi seeded by raw wide bits.
//! This types the Java header local; it never numerically converts a long value.
use crate::{
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
    native_types::{AssignmentBound, InferredTypes, TypeResolution},
};
use std::collections::{HashMap, HashSet};
pub(crate) fn double_header(
    ir: &DecodedMethod,
    ssa: &SsaMethod,
    types: &InferredTypes,
    header: usize,
    register: usize,
    bits: u64,
) -> bool {
    if ir.instructions.len() > 8192
        || ssa.definitions.len() > 32768
        || types.values.len() != ssa.definitions.len()
        || types.wide_pair_issues != 0
    {
        return false;
    }
    let mut heads = ssa.phis.iter().filter(|p| {
        usize::from(p.register) == register
            && ssa
                .graph
                .blocks
                .get(p.block)
                .is_some_and(|b| b.start == header)
    });
    let Some(head) = heads.next() else {
        return false;
    };
    if heads.next().is_some() || head.incoming.is_empty() {
        return false;
    }
    let mut tails = ssa
        .phis
        .iter()
        .filter(|p| usize::from(p.register) == register + 1 && p.block == head.block);
    let Some(tail) = tails.next() else {
        return false;
    };
    if tails.next().is_some() {
        return false;
    }
    let target = &types.values[head.result];
    if target.resolution != TypeResolution::Resolved("D".into())
        || !target.assignment.contains(&AssignmentBound::Literal {
            bits: bits as i64,
            wide: true,
        })
        || !target
            .assignment
            .contains(&AssignmentBound::Type("D".into()))
        || target.required_types.is_empty()
    {
        return false;
    }
    let phis: HashMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    let raw: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let instructions: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    if phis.len() != ssa.phis.len()
        || raw.len() != ir.instructions.len()
        || instructions.len() != ssa.instructions.len()
    {
        return false;
    }
    let mut pending = vec![(head.result, tail.result)];
    let mut seen = HashSet::new();
    let mut budget = 65536;
    let mut anchors = 0;
    while let Some((lo, hi)) = pending.pop() {
        if budget == 0 {
            return false;
        }
        budget -= 1;
        if !seen.insert((lo, hi)) {
            continue;
        }
        let Some(a) = ssa.definitions.get(lo) else {
            return false;
        };
        let Some(b) = ssa.definitions.get(hi) else {
            return false;
        };
        if a.register.checked_add(1) != Some(b.register) {
            return false;
        }
        for id in [lo, hi] {
            let Some(t) = types.values.get(id) else {
                return false;
            };
            if t.bounds_truncated
                || t.assignment.is_empty()
                || !t.assignment.iter().all(|a| {
                    matches!(a, AssignmentBound::Literal { wide: true, .. })
                        || matches!(a,AssignmentBound::Type(ty)if ty.as_ref()=="D")
                })
                || !t.required_types.iter().all(|ty| ty.as_ref() == "D")
            {
                return false;
            }
        }
        if types.values[lo].assignment != types.values[hi].assignment {
            return false;
        }
        match (&a.kind, &b.kind) {
            (DefinitionKind::Phi { block: x }, DefinitionKind::Phi { block: y }) if x == y => {
                let Some(a) = phis.get(&lo) else {
                    return false;
                };
                let Some(b) = phis.get(&hi) else {
                    return false;
                };
                if a.block != *x
                    || b.block != *x
                    || a.incoming.len() != b.incoming.len()
                    || a.incoming.is_empty()
                    || ssa.reachable.get(*x) != Some(&true)
                {
                    return false;
                }
                let mut count = 0;
                for (&(from, lo), &(other, hi)) in a.incoming.iter().zip(&b.incoming) {
                    if from != other {
                        return false;
                    }
                    if from.is_some_and(|p| ssa.reachable.get(p) != Some(&true)) {
                        continue;
                    }
                    count += 1;
                    pending.push((lo, hi));
                }
                if count == 0 {
                    return false;
                }
            }
            (
                DefinitionKind::Instruction { pc: a, word: 0, .. },
                DefinitionKind::Instruction { pc: b, word: 1, .. },
            ) if a == b => {
                let Some(ins) = instructions.get(a) else {
                    return false;
                };
                let Some(raw) = raw.get(a) else {
                    return false;
                };
                if ins.writes.len() != 1
                    || ins.writes[0].words != [lo, hi]
                    || raw.writes.len() != 1
                    || raw.writes[0].kind.word_count() != 2
                {
                    return false;
                }
                if matches!(raw.opcode, 0x04..=0x06) {
                    if ins.reads.len() != 1 || ins.reads[0].words.len() != 2 {
                        return false;
                    }
                    pending.push((ins.reads[0].words[0], ins.reads[0].words[1]));
                } else if matches!(raw.opcode, 0x16..=0x19) {
                    let Some(bits) = raw.literal else {
                        return false;
                    };
                    if types.values[lo].assignment.as_slice()
                        != [AssignmentBound::Literal { bits, wide: true }]
                    {
                        return false;
                    }
                    anchors += 1;
                } else if types.values[lo].assignment.as_slice()
                    == [AssignmentBound::Type("D".into())]
                {
                    anchors += 1;
                } else {
                    return false;
                }
            }
            (DefinitionKind::Parameter, DefinitionKind::Parameter)
                if types.values[lo].assignment.as_slice()
                    == [AssignmentBound::Type("D".into())] =>
            {
                anchors += 1;
            }
            _ => return false,
        }
    }
    anchors != 0
}
