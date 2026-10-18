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
    pub arguments: Vec<crate::native_calls::TypedRegister>,
    pub suffix_entries: Vec<(usize, Identity)>,
    pub needed_tail_words: Vec<usize>,
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
    pub separate_suffixes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Identity {
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
    this_aliases: HashMap<usize, Option<bool>>,
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

    // None means the bounded proof could not establish absence of this.
    // This is independent of identity(), whose None also represents ordinary
    // computed values and therefore cannot certify receiver exclusion.
    fn may_alias_this(&mut self, id: usize, receiver: u16, depth: usize) -> Option<bool> {
        if depth > 32 || self.work == 0 || !self.complete(id) {
            return None;
        }
        self.work -= 1;
        if let Some(result) = self.this_aliases.get(&id) {
            return *result;
        }
        self.this_aliases.insert(id, None);
        let definition = self.analysis.ssa().definitions.get(id)?;
        let result = match definition.kind {
            DefinitionKind::Parameter => Some(definition.register == receiver),
            DefinitionKind::Undefined => None,
            DefinitionKind::Phi { .. } => {
                let incoming: Vec<_> = self
                    .analysis
                    .ssa()
                    .phis
                    .iter()
                    .find(|phi| phi.result == id)?
                    .incoming
                    .iter()
                    .filter(|(from, _)| {
                        from.is_none_or(|block| {
                            self.analysis
                                .ssa()
                                .reachable
                                .get(block)
                                .copied()
                                .unwrap_or(false)
                        })
                    })
                    .map(|(_, value)| *value)
                    .collect();
                if incoming.is_empty() {
                    return None;
                }
                let mut aliases = false;
                for value in incoming {
                    aliases |= self.may_alias_this(value, receiver, depth + 1)?;
                }
                Some(aliases)
            }
            DefinitionKind::Instruction { pc, word, .. } => {
                let instruction = *self.ir.get(&pc)?;
                if matches!(instruction.opcode, 0x01..=0x09 | 0x1f) {
                    let values = *self.ssa.get(&pc)?;
                    if values.reads.len() != 1 {
                        return None;
                    }
                    let source = *values.reads[0].words.get(word)?;
                    self.may_alias_this(source, receiver, depth + 1)
                } else {
                    Some(false)
                }
            }
        };
        self.this_aliases.insert(id, result);
        result
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

// A final instance field can be assigned once per constructor path, only
// through the exact uninitialized/current receiver. Resolve pool aliases to
// the class field index before counting assignments.
fn final_write(
    proof: &mut Proof<'_, '_>,
    class: &DexClass,
    instruction: &Instruction,
    this: u16,
) -> Option<Option<usize>> {
    if !matches!(instruction.opcode, 0x59..=0x5f | 0x67..=0x6d) {
        return Some(None);
    }
    let reference = instruction.reference?;
    let &(owner, ty, name) = class.symbols.fields.get(reference.index as usize)?;
    if class.symbols.types.get(owner as usize)?.as_ref() != class.descriptor.as_ref() {
        return Some(None);
    }
    let name = class.symbols.strings.get(name as usize)?;
    let ty = class.symbols.types.get(ty as usize)?;
    let mut fields = class
        .fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.name.as_ref() == name.as_str() && field.field_type == *ty);
    let (index, field) = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    if field.access_flags & 0x10 == 0 {
        return Some(None);
    }
    if field.is_static || !matches!(instruction.opcode, 0x59..=0x5f) {
        return None;
    }
    let values = *proof.ssa.get(&instruction.pc)?;
    let receiver = values.reads.first()?;
    if receiver.words.len() != 1
        || proof.identity(receiver.words[0], 0) != Some(Identity::Parameter(this))
    {
        return None;
    }
    Some(Some(index))
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
        this_aliases: HashMap::new(),
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
    let mut prelude_final_writes = HashSet::new();
    let prelude: Vec<_> = proof
        .ir
        .values()
        .copied()
        .filter(|instruction| instruction.pc < pc)
        .collect();
    for instruction in prelude {
        if let Some(field) = final_write(&mut proof, class, instruction, this)?
            && !prelude_final_writes.insert(field)
        {
            return None;
        }
    }
    let mut arms: Vec<Arm> = Vec::with_capacity(starts.len());
    let mut common_entries: Option<Vec<(usize, Identity)>> = None;
    let mut separate_suffixes = false;
    let mut common_field_writes = None;
    let mut common_argument_types = None;
    for start in starts {
        let mut at = start;
        let mut written = BTreeSet::new();
        let mut final_writes = prelude_final_writes.clone();
        let mut delegation = None;
        for step in 0..=256 {
            if step == 256 {
                return None;
            }
            let instruction = *proof.ir.get(&at)?;
            if let Some(field) = final_write(&mut proof, class, instruction, this)?
                && !final_writes.insert(field)
            {
                return None;
            }
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
                        || call.return_type.as_ref() != "V"
                    {
                        return None;
                    }
                    if call.arguments.len() > 16 {
                        return None;
                    }
                    if !call.arguments.is_empty()
                        && proof.ir.values().any(|instruction| {
                            (instruction.pc < pc || (start..at).contains(&instruction.pc))
                                && matches!(instruction.opcode, 0x59..=0x5f | 0x67..=0x6d)
                        })
                    {
                        return None;
                    }
                    if !call.arguments.is_empty() {
                        let reads: Vec<_> = proof
                            .ssa
                            .iter()
                            .filter(|(read_pc, _)| **read_pc < pc || (start..at).contains(read_pc))
                            .flat_map(|(_, values)| {
                                values
                                    .reads
                                    .iter()
                                    .flat_map(|read| read.words.iter().copied())
                            })
                            .collect();
                        for id in reads {
                            if proof.may_alias_this(id, this, 0) != Some(false) {
                                return None;
                            }
                        }
                    }
                    // The common emitter reads physical registers. Distinct
                    // typed carriers cannot share one word without losing a
                    // conversion, so this bounded slice rejects overlaps.
                    let mut argument_words = BTreeSet::new();
                    for argument in &call.arguments {
                        let width = if matches!(argument.descriptor.as_ref(), "J" | "D") {
                            2
                        } else {
                            1
                        };
                        for word in
                            usize::from(argument.register)..usize::from(argument.register) + width
                        {
                            if !argument_words.insert(word) {
                                return None;
                            }
                        }
                    }
                    let argument_types: Vec<_> = call
                        .arguments
                        .iter()
                        .map(|arg| arg.descriptor.clone())
                        .collect();
                    if let Some(types) = &common_argument_types {
                        if types != &argument_types {
                            return None;
                        }
                    } else {
                        common_argument_types = Some(argument_types);
                    }
                    let raw_call = analysis.calls().calls.iter().find(|c| c.pc == at)?;
                    for argument in &raw_call.arguments {
                        for &id in &argument.words {
                            if !proof.complete(id)
                                || proof.may_alias_this(id, this, 0) != Some(false)
                            {
                                return None;
                            }
                        }
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
        let mut field_writes = Vec::new();
        for step in 0..=256 {
            if step == 256 {
                return None;
            }
            let instruction = *proof.ir.get(&at)?;
            if matches!(instruction.opcode, 0x0f..=0x11 | 0x1d..=0x1e | 0x26..=0x3d) {
                return None;
            }
            if let Some(field) = final_write(&mut proof, class, instruction, this)?
                && !final_writes.insert(field)
            {
                return None;
            }
            if matches!(instruction.opcode, 0x59..=0x5f | 0x67..=0x6d) {
                let reference = instruction.reference?;
                if reference.kind != PoolKind::Field {
                    return None;
                }
                field_writes.push((instruction.opcode, reference.index));
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
        if let Some(expected) = &common_field_writes {
            if expected != &field_writes {
                return None;
            }
        } else {
            common_field_writes = Some(field_writes);
        }
        let arm = Arm {
            start,
            delegation_start: delegation.0,
            delegation_end: delegation.1,
            post_start: delegation.1,
            end: at,
            prefix_written_words: written.into_iter().collect(),
            arguments: bound
                .calls
                .iter()
                .find(|c| c.pc == delegation.0)?
                .arguments
                .clone(),
            suffix_entries: entries.clone(),
            needed_tail_words: entries.iter().map(|(word, _)| *word).collect(),
        };
        if let Some(first) = arms.first() {
            if code.instructions.get(first.post_start..first.end)?
                != code.instructions.get(arm.post_start..arm.end)?
                || common_entries.as_ref()? != &entries
            {
                separate_suffixes = true;
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
        separate_suffixes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use std::sync::Arc;

    #[test]
    fn receiver_exclusion_fails_closed_at_work_and_depth_limits() {
        let method = DexMethod {
            declaring_type: "Lsample/Proof;".into(),
            name: "<init>".into(),
            parameters: vec!["Ljava/lang/Object;".into()],
            return_type: "V".into(),
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 3,
                ins: 2,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x1007, 0x000e],
                offset: 0,
            }),
        };
        let class = DexClass {
            descriptor: method.declaring_type.clone(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![method],
            symbols: Arc::new(DexSymbols::default()),
        };
        let analysis = MethodAnalysis::build(&class, &class.methods[0]).unwrap();
        let types = analysis.infer_types().unwrap();
        let receiver = analysis
            .ssa()
            .definitions
            .iter()
            .position(|definition| {
                definition.kind == DefinitionKind::Parameter && definition.register == 1
            })
            .unwrap();
        let input = analysis
            .ssa()
            .definitions
            .iter()
            .position(|definition| {
                definition.kind == DefinitionKind::Parameter && definition.register == 2
            })
            .unwrap();
        let alias = analysis
            .ssa()
            .definitions
            .iter()
            .position(|definition| {
                matches!(
                    definition.kind,
                    DefinitionKind::Instruction { pc: 0, word: 0, .. }
                )
            })
            .unwrap();
        let mut proof = Proof {
            analysis: &analysis,
            types: &types,
            ir: analysis
                .instructions()
                .instructions
                .iter()
                .map(|instruction| (instruction.pc, instruction))
                .collect(),
            ssa: analysis
                .ssa()
                .instructions
                .iter()
                .map(|instruction| (instruction.pc, instruction))
                .collect(),
            identities: HashMap::new(),
            this_aliases: HashMap::new(),
            work: 0,
        };
        assert_eq!(proof.may_alias_this(receiver, 1, 0), None);
        assert_eq!(proof.may_alias_this(input, 1, 0), None);
        proof.work = 32;
        assert_eq!(proof.may_alias_this(receiver, 1, 0), Some(true));
        assert_eq!(proof.may_alias_this(alias, 1, 0), Some(true));
        assert_eq!(proof.may_alias_this(input, 1, 0), Some(false));
        assert_eq!(proof.may_alias_this(input, 1, 33), None);
        proof.work = 0;
        assert_eq!(proof.may_alias_this(input, 1, 0), None);
    }
}
