//! Read-specific identity proof, independent of rendered Java local names.
//! This does not authorize dispatch, initialization, or erasing a cast/effect.
use crate::{
    native_dex::DexMethod,
    native_ir::{DecodedMethod, ValueKind},
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::collections::{HashMap, HashSet};

pub(crate) fn proves_read(
    method: &DexMethod,
    ir: &DecodedMethod,
    ssa: &SsaMethod,
    pc: usize,
    register: usize,
) -> bool {
    let Some(code) = method.code.as_ref() else {
        return false;
    };
    let input_words = 1 + method
        .parameters
        .iter()
        .map(|t| {
            if matches!(t.as_ref(), "J" | "D") {
                2
            } else {
                1
            }
        })
        .sum::<usize>();
    if method.access_flags & 8 != 0
        || code.ins > code.registers
        || usize::from(code.ins) != input_words
        || ir.instructions.len() > 8192
        || ssa.definitions.len() > 32768
        || ssa.graph.blocks.len() > 16384
        || ssa.reachable.len() != ssa.graph.blocks.len()
    {
        return false;
    }
    let Some(raw) = ir.instructions.iter().find(|i| i.pc == pc) else {
        return false;
    };
    let Some(read) = ssa.instructions.iter().find(|i| i.pc == pc) else {
        return false;
    };
    if !matches!(raw.opcode, 0x6f | 0x70 | 0x75 | 0x76)
        || raw
            .reads
            .first()
            .is_none_or(|r| usize::from(r.register) != register || r.kind.word_count() != 1)
        || read
            .reads
            .first()
            .is_none_or(|r| usize::from(r.register) != register || r.words.len() != 1)
    {
        return false;
    }
    let Some(&block) = ssa.graph.block_at.get(&pc) else {
        return false;
    };
    if !ssa.reachable.get(block).copied().unwrap_or(false) {
        return false;
    }
    proves_value(method, ir, ssa, read.reads[0].words[0])
}

pub(crate) fn proves_value(
    method: &DexMethod,
    ir: &DecodedMethod,
    ssa: &SsaMethod,
    value: usize,
) -> bool {
    let Some(code) = method.code.as_ref() else {
        return false;
    };
    let input_words = 1 + method
        .parameters
        .iter()
        .map(|t| {
            if matches!(t.as_ref(), "J" | "D") {
                2
            } else {
                1
            }
        })
        .sum::<usize>();
    if method.access_flags & 8 != 0
        || code.ins > code.registers
        || usize::from(code.ins) != input_words
        || ir.instructions.len() > 8192
        || ssa.definitions.len() > 32768
        || ssa.graph.blocks.len() > 16384
        || ssa.reachable.len() != ssa.graph.blocks.len()
    {
        return false;
    }
    let this_register = code.registers - code.ins;
    let instructions: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let operands: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    let phis: HashMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    let mut predecessors = vec![HashSet::new(); ssa.graph.blocks.len()];
    let mut budget = 65536usize;
    for (from, block) in ssa.graph.blocks.iter().enumerate() {
        for edge in &block.successors {
            if budget == 0 || edge.target >= predecessors.len() {
                return false;
            }
            budget -= 1;
            if ssa.reachable[from] && ssa.reachable[edge.target] {
                predecessors[edge.target].insert(Some(from));
            }
        }
    }
    if predecessors.is_empty() || !ssa.reachable[0] {
        return false;
    }
    predecessors[0].insert(None);
    let mut work = vec![value];
    let mut seen = HashSet::new();
    let mut has_this_root = false;
    while let Some(id) = work.pop() {
        if budget == 0 {
            return false;
        }
        budget -= 1;
        if !seen.insert(id) {
            continue;
        }
        let Some(definition) = ssa.definitions.get(id) else {
            return false;
        };
        match definition.kind {
            DefinitionKind::Parameter => {
                if definition.register != this_register || id != usize::from(this_register) {
                    return false;
                }
                has_this_root = true;
            }
            DefinitionKind::Undefined => return false,
            DefinitionKind::Phi { block } => {
                let Some(phi) = phis.get(&id) else {
                    return false;
                };
                if phi.block != block
                    || phi.register != definition.register
                    || block >= predecessors.len()
                    || !ssa.reachable[block]
                {
                    return false;
                }
                let mut actual = HashSet::new();
                for &(from, value) in &phi.incoming {
                    if budget == 0 {
                        return false;
                    }
                    budget -= 1;
                    if from.is_some_and(|b| b >= ssa.reachable.len()) {
                        return false;
                    }
                    if from.is_some_and(|b| !ssa.reachable[b]) {
                        continue;
                    }
                    if !actual.insert(from) {
                        return false;
                    }
                    work.push(value);
                }
                if actual.is_empty() || actual != predecessors[block] {
                    return false;
                }
            }
            DefinitionKind::Instruction { pc, word, block } => {
                let Some(raw) = instructions.get(&pc) else {
                    return false;
                };
                let Some(values) = operands.get(&pc) else {
                    return false;
                };
                if word != 0
                    || block >= ssa.reachable.len()
                    || !ssa.reachable[block]
                    || !matches!(raw.opcode, 0x07..=0x09)
                    || raw.reads.len() != 1
                    || raw.writes.len() != 1
                    || raw.reads[0].kind != ValueKind::Reference
                    || raw.writes[0].kind != ValueKind::Reference
                    || values.reads.len() != 1
                    || values.reads[0].words.len() != 1
                    || values.writes.len() != 1
                    || values.writes[0].words.as_slice() != [id]
                    || values.writes[0].register != definition.register
                {
                    return false;
                }
                work.push(values.reads[0].words[0]);
            }
        }
    }
    // Closed copy/phi cycles without entry-this cannot establish identity.
    has_this_root
}

// One bounded propagation for possible-this uses. Mixed this/other phis are
// possible-this, never proven same-this. Instruction effects are not traversed.
pub(crate) fn possible_values(
    method: &DexMethod,
    ir: &DecodedMethod,
    ssa: &SsaMethod,
) -> Option<HashSet<usize>> {
    let code = method.code.as_ref()?;
    if method.access_flags & 8 != 0
        || code.ins > code.registers
        || ssa.definitions.len() > 32768
        || ir.instructions.len() > 8192
    {
        return None;
    }
    let this_register = code.registers - code.ins;
    let root = usize::from(this_register);
    if ssa.definitions.get(root)?.register != this_register
        || ssa.definitions[root].kind != DefinitionKind::Parameter
    {
        return None;
    }
    let raw: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let at: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
    let phis: HashMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    let mut dependents = vec![vec![]; ssa.definitions.len()];
    let mut budget = 65536;
    for (id, definition) in ssa.definitions.iter().enumerate() {
        let inputs: Vec<_> = match definition.kind {
            DefinitionKind::Phi { .. } => phis
                .get(&id)?
                .incoming
                .iter()
                .filter(|(from, _)| {
                    from.is_none_or(|b| ssa.reachable.get(b).copied().unwrap_or(false))
                })
                .map(|(_, value)| *value)
                .collect(),
            DefinitionKind::Instruction { pc, word: 0, .. }
                if matches!(raw.get(&pc)?.opcode, 0x07..=0x09) =>
            {
                let values = at.get(&pc)?;
                if values.reads.len() != 1 || values.reads[0].words.len() != 1 {
                    return None;
                }
                vec![values.reads[0].words[0]]
            }
            _ => vec![],
        };
        for input in inputs {
            if budget == 0 {
                return None;
            }
            budget -= 1;
            dependents.get_mut(input)?.push(id);
        }
    }
    let mut work = vec![root];
    let mut seen = HashSet::new();
    while let Some(value) = work.pop() {
        if budget == 0 {
            return None;
        }
        budget -= 1;
        if !seen.insert(value) {
            continue;
        }
        work.extend(&dependents[value]);
    }
    Some(seen)
}
