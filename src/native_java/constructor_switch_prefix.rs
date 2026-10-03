//! A closed switch over independent constructor prologues, followed by one
//! identical parent delegation and suffix. This is a proof, not a renderer:
//! callers must still compare the live runtime frame values after each prefix.
use crate::{
    native_calls::{BoundCalls, CallKind, CallTarget},
    native_cfg::EdgeKind,
    native_dex::{DexClass, DexMethod},
    native_ir::{Instruction, PoolKind},
    native_method::MethodAnalysis,
    native_ssa::{DefinitionKind, SsaInstruction},
    native_types::{AssignmentBound, InferredTypes, TypeResolution},
};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Arm {
    pub start: usize,
    /// Prefix is [start, delegation_start); all ranges use raw code-unit PCs.
    pub delegation_start: usize,
    pub delegation_end: usize,
    pub post_start: usize,
    pub end: usize,
    pub prefix_written_words: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub switch_pc: usize,
    pub selector_register: usize,
    pub receiver_register: usize,
    /// Raw payload order; targets may share one Arm. Default is arms[0].
    pub cases: Vec<(i32, usize)>,
    pub arms: Vec<Arm>,
    /// Distinct physical words, in first-read order in the common suffix.
    /// A renderer compares only these words, never dead prefix temporaries.
    pub needed_tail_words: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Identity {
    Parameter(u16),
    Literal(i64, bool),
    String(u32),
}

struct Proof<'a, 'm> {
    analysis: &'a MethodAnalysis<'m>,
    types: &'a InferredTypes,
    ir: HashMap<usize, &'a Instruction>,
    ssa: HashMap<usize, &'a SsaInstruction>,
    identities: HashMap<usize, Option<Identity>>,
    work: usize,
}

impl Proof<'_, '_> {
    fn complete(&self, id: usize) -> bool {
        self.types.values.get(id).is_some_and(|v| {
            !v.bounds_truncated
                && !v.assignment.is_empty()
                && !v
                    .assignment
                    .iter()
                    .any(|a| matches!(a, AssignmentBound::Undefined | AssignmentBound::Unknown))
                && matches!(v.resolution, TypeResolution::Resolved(_))
        })
    }

    fn identity(&mut self, id: usize, depth: usize) -> Option<Identity> {
        if depth > 32 || self.work == 0 || !self.complete(id) {
            return None;
        }
        self.work -= 1;
        if let Some(value) = self.identities.get(&id) {
            return value.clone();
        }
        // A recursive phi cannot establish an entry identity. Mark before
        // descent so cycles fail instead of relying on a depth cutoff alone.
        self.identities.insert(id, None);
        let definition = self.analysis.ssa().definitions.get(id)?;
        let value = match definition.kind {
            DefinitionKind::Parameter => Some(Identity::Parameter(definition.register)),
            DefinitionKind::Undefined => None,
            DefinitionKind::Phi { .. } => {
                let phi = self.analysis.ssa().phis.iter().find(|p| p.result == id)?;
                let incoming: Vec<_> = phi
                    .incoming
                    .iter()
                    .filter(|(from, _)| {
                        from.is_none_or(|b| {
                            self.analysis
                                .ssa()
                                .reachable
                                .get(b)
                                .copied()
                                .unwrap_or(false)
                        })
                    })
                    .map(|(_, value)| *value)
                    .collect();
                let first = self.identity(*incoming.first()?, depth + 1)?;
                for value in incoming.into_iter().skip(1) {
                    if self.identity(value, depth + 1).as_ref() != Some(&first) {
                        return None;
                    }
                }
                Some(first)
            }
            DefinitionKind::Instruction { pc, word, .. } => {
                if word != 0 {
                    return None;
                }
                let instruction = *self.ir.get(&pc)?;
                match instruction.opcode {
                    0x01..=0x09 => {
                        let values = *self.ssa.get(&pc)?;
                        // Wide copies require pair provenance, which this
                        // first tranche deliberately does not invent.
                        if values.reads.len() != 1 || values.reads[0].words.len() != 1 {
                            return None;
                        }
                        self.identity(values.reads[0].words[0], depth + 1)
                    }
                    0x12..=0x19 => Some(Identity::Literal(
                        instruction.literal?,
                        matches!(instruction.opcode, 0x16..=0x19),
                    )),
                    0x1a | 0x1b => {
                        let reference = instruction.reference?;
                        (reference.kind == PoolKind::String)
                            .then_some(Identity::String(reference.index))
                    }
                    _ => None,
                }
            }
        };
        self.identities.insert(id, value.clone());
        value
    }
}

fn i32_at(words: &[u16], pc: usize) -> Option<i32> {
    Some((u32::from(*words.get(pc)?) | u32::from(*words.get(pc + 1)?) << 16) as i32)
}

fn cases(words: &[u16], instruction: &Instruction) -> Option<Vec<(i32, usize)>> {
    let payload = instruction.payload_target?;
    let count = usize::from(*words.get(payload + 1)?);
    if count == 0 || count > 32 {
        return None;
    }
    let packed = match (instruction.opcode, *words.get(payload)?) {
        (0x2b, 0x0100) => true,
        (0x2c, 0x0200) => false,
        _ => return None,
    };
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let key = if packed {
            i32_at(words, payload + 2)?.checked_add(i32::try_from(index).ok()?)?
        } else {
            i32_at(words, payload + 2 + index * 2)?
        };
        let offset = if packed {
            payload + 4 + index * 2
        } else {
            payload + 2 + count * 2 + index * 2
        };
        let target = i64::try_from(instruction.pc).ok()? + i64::from(i32_at(words, offset)?);
        let target = usize::try_from(target).ok()?;
        if target >= words.len() || result.last().is_some_and(|(previous, _)| *previous >= key) {
            return None;
        }
        result.push((key, target));
    }
    Some(result)
}

/// Select only a bounded, closed, forward switch whose prefixes cannot use or
/// overwrite uninitialized this, and whose suffix entry values are identical.
/// None leaves all existing rendering and constructor guards in force.
pub(crate) fn select(class: &DexClass, method: &DexMethod, pc: usize) -> Option<Plan> {
    let code = method.code.as_ref()?;
    if method.name.as_ref() != "<init>"
        || method.return_type.as_ref() != "V"
        || method.access_flags & 0x8 != 0
        || method.declaring_type != class.descriptor
        || code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() > 4096
        || code.registers > 256
        || code.ins == 0
    {
        return None;
    }
    let this = code.registers.checked_sub(code.ins)?;
    let analysis = MethodAnalysis::build(class, method).ok()?;
    if analysis.ssa().definitions.len() > 16384 {
        return None;
    }
    let types = analysis.infer_types().ok()?;
    if types.wide_pair_issues != 0 {
        return None;
    }
    let bound = BoundCalls::bind(code, analysis.instructions(), &class.symbols).ok()?;
    let ir: HashMap<_, _> = analysis
        .instructions()
        .instructions
        .iter()
        .map(|i| (i.pc, i))
        .collect();
    let instruction = *ir.get(&pc)?;
    if instruction.width != 3 || instruction.reads.len() != 1 {
        return None;
    }
    let cases = cases(&code.instructions, instruction)?;
    let cfg = analysis.blocks();
    // Exclude loops and exceptional control throughout this first tranche.
    for block in &cfg.blocks {
        for edge in &block.successors {
            let target = cfg.blocks.get(edge.target)?;
            if edge.kind != EdgeKind::Normal || target.start <= *block.instructions.last()? {
                return None;
            }
        }
    }
    let mut proof = Proof {
        analysis: &analysis,
        types: &types,
        ir,
        ssa: analysis
            .ssa()
            .instructions
            .iter()
            .map(|i| (i.pc, i))
            .collect(),
        identities: HashMap::new(),
        work: 65536,
    };
    let selector_register = usize::from(instruction.reads[0].register);
    let selector = *proof.ssa.get(&pc)?;
    if selector.reads.len() != 1
        || selector.reads[0].words.len() != 1
        || !proof.complete(selector.reads[0].words[0])
        || proof.identity(selector.reads[0].words[0], 0) == Some(Identity::Parameter(this))
    {
        return None;
    }
    let mut starts = vec![pc + instruction.width];
    for &(_, start) in &cases {
        if start <= pc {
            return None;
        }
        if !starts.contains(&start) {
            starts.push(start);
        }
    }
    let mut arms: Vec<Arm> = Vec::with_capacity(starts.len());
    let mut common_entries: Option<Vec<(usize, Identity)>> = None;
    for start in starts {
        let mut at = start;
        let mut written = BTreeSet::new();
        let mut delegation = None;
        for step in 0..=256 {
            if step == 256 {
                return None;
            }
            let instruction = *proof.ir.get(&at)?;
            if matches!(instruction.opcode, 0x70 | 0x76) {
                let call = bound.calls.iter().find(|c| c.pc == at)?;
                if let CallTarget::Method {
                    declaring_type,
                    name,
                    ..
                } = &call.target
                    && name.as_ref() == "<init>"
                {
                    let receiver = analysis
                        .calls()
                        .calls
                        .iter()
                        .find(|c| c.pc == at)?
                        .receiver
                        .as_ref()?;
                    if receiver.words.len() != 1
                        || proof.identity(receiver.words[0], 0) != Some(Identity::Parameter(this))
                        || call.receiver.as_ref()?.register != this
                        || call.kind != CallKind::Direct
                        || Some(declaring_type.as_ref()) != class.superclass.as_deref()
                        || !call.arguments.is_empty()
                        || call.return_type.as_ref() != "V"
                    {
                        return None;
                    }
                    delegation = Some((at, at + instruction.width));
                    break;
                }
            }
            if matches!(instruction.opcode, 0x0e..=0x11 | 0x1d..=0x1e | 0x22..=0x25 | 0x27..=0x3d) {
                return None;
            }
            let values = *proof.ssa.get(&at)?;
            for read in &values.reads {
                for &id in &read.words {
                    if !proof.complete(id)
                        || proof.identity(id, 0) == Some(Identity::Parameter(this))
                    {
                        return None;
                    }
                }
            }
            for write in &values.writes {
                for offset in 0..write.words.len() {
                    let word = usize::from(write.register) + offset;
                    if word == usize::from(this) {
                        return None;
                    }
                    written.insert(word);
                }
            }
            at += instruction.width;
        }
        let delegation = delegation?;
        at = delegation.1;
        let mut defined = HashSet::new();
        let mut needed = HashSet::new();
        let mut entries = Vec::new();
        for step in 0..=256 {
            if step == 256 {
                return None;
            }
            let instruction = *proof.ir.get(&at)?;
            if matches!(instruction.opcode, 0x0f..=0x11 | 0x1d..=0x1e | 0x26..=0x3d) {
                return None;
            }
            let values = *proof.ssa.get(&at)?;
            for read in &values.reads {
                for (offset, &id) in read.words.iter().enumerate() {
                    let word = usize::from(read.register) + offset;
                    if !defined.contains(&word) && needed.insert(word) {
                        entries.push((word, proof.identity(id, 0)?));
                    }
                }
            }
            for write in &values.writes {
                for offset in 0..write.words.len() {
                    defined.insert(usize::from(write.register) + offset);
                }
            }
            at += instruction.width;
            if instruction.opcode == 0x0e {
                break;
            }
        }
        let arm = Arm {
            start,
            delegation_start: delegation.0,
            delegation_end: delegation.1,
            post_start: delegation.1,
            end: at,
            prefix_written_words: written.into_iter().collect(),
        };
        if let Some(first) = arms.first() {
            if code.instructions.get(first.post_start..first.end)?
                != code.instructions.get(arm.post_start..arm.end)?
                || common_entries.as_ref()? != &entries
            {
                return None;
            }
        } else {
            common_entries = Some(entries);
        }
        // No arm may overlap another; no outside edge may enter an interior
        // instruction or bypass that arm's prefix/delegation.
        if arms
            .iter()
            .any(|other| arm.start < other.end && other.start < arm.end)
        {
            return None;
        }
        for block in &cfg.blocks {
            for edge in &block.successors {
                let from = *block.instructions.last()?;
                let to = cfg.blocks.get(edge.target)?.start;
                if (arm.start..arm.end).contains(&to)
                    && !(arm.start..arm.end).contains(&from)
                    && !(from == pc && to == arm.start)
                {
                    return None;
                }
            }
        }
        arms.push(arm);
    }
    Some(Plan {
        switch_pc: pc,
        selector_register,
        receiver_register: usize::from(this),
        cases,
        arms,
        needed_tail_words: common_entries?.into_iter().map(|(word, _)| word).collect(),
    })
}
