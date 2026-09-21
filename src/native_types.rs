//! Initial SSA type-bound propagation adapted from JADX TypeInferenceVisitor and
//! TypeUpdate (Apache-2.0), commit 28ff15e4ae69950aebea110a13e5ab895d234dfc.
//! Assignment bounds and use requirements remain separate: DEX literal bits may
//! require different Java expressions at different uses. Phi edges propagate
//! assignments forwards and requirements backwards, never equating subtypes.
//! This is NOT the complete JADX inference pass: hierarchy joins, generic types,
//! general backwards array inference and conversion insertion remain unresolved.
use crate::native_call_values::SsaCalls;
use crate::native_dex::{DexMethod, DexSymbols};
use crate::native_ir::{DecodedMethod, PoolKind};
use crate::native_ssa::{DefinitionKind, SsaMethod};
use anyhow::{Context, Result, ensure};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

const LIMIT: usize = 1_000_000;
const ALTERNATIVES: usize = 8;
const REF: &str = "<reference>";
const INT: &str = "<int-like>";
const ARRAY: &str = "<array>";
const ARRAY_USES: [&str; 7] = [
    "<array32>",
    "<array64>",
    "<array-ref>",
    "<array-Z>",
    "<array-B>",
    "<array-C>",
    "<array-S>",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssignmentBound {
    Type(Arc<str>),
    Literal { bits: i64, wide: bool },
    Undefined,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeResolution {
    Resolved(Arc<str>),
    Unresolved(&'static str),
    Conflict(&'static str),
}
#[derive(Debug, Clone)]
pub struct ValueType {
    pub assignment: Vec<AssignmentBound>,
    pub required_types: Vec<Arc<str>>,
    pub resolution: TypeResolution,
    /// Excludes unused initial undefined register seeds from counters.
    pub active: bool,
    /// Bounded alternatives were exceeded. No retained subset is treated as complete.
    pub bounds_truncated: bool,
}
#[derive(Debug)]
pub struct InferredTypes {
    pub values: Vec<ValueType>,
    pub resolved_values: usize,
    pub unresolved_values: usize,
    pub conflicting_values: usize,
    /// Wide pairs whose SSA definition provenance cannot be established as coherent.
    pub wide_pair_issues: usize,
}
#[derive(Clone)]
struct ArrayConstraint {
    opcode: u8,
    words: Vec<usize>,
}
struct Solver {
    values: Vec<ValueType>,
    forward: Vec<Vec<usize>>,
    backward: Vec<Vec<usize>>,
    arrays: Vec<Vec<ArrayConstraint>>,
    dynamic_outputs: Vec<bool>,
    work: usize,
    entries: usize,
}
impl Solver {
    fn tick(&mut self) -> Result<()> {
        ensure!(self.work > 0, "type inference exceeds work budget");
        self.work -= 1;
        Ok(())
    }
    fn entry(&mut self) -> Result<()> {
        self.entries += 1;
        ensure!(
            self.entries <= LIMIT,
            "type inference exceeds storage budget"
        );
        Ok(())
    }
    fn assigned(&mut self, value: usize, bound: AssignmentBound) -> Result<bool> {
        self.tick()?;
        let v = self
            .values
            .get_mut(value)
            .context("type value outside SSA")?;
        if v.assignment.contains(&bound) {
            return Ok(false);
        }
        if v.assignment.len() == ALTERNATIVES {
            let changed = !v.bounds_truncated;
            v.bounds_truncated = true;
            return Ok(changed);
        }
        v.assignment.push(bound);
        self.entry()?;
        Ok(true)
    }
    fn required(&mut self, value: usize, ty: Arc<str>) -> Result<bool> {
        self.tick()?;
        let v = self
            .values
            .get_mut(value)
            .context("type value outside SSA")?;
        if v.required_types.contains(&ty) {
            return Ok(false);
        }
        if v.required_types.len() == ALTERNATIVES {
            let changed = !v.bounds_truncated;
            v.bounds_truncated = true;
            return Ok(changed);
        }
        v.required_types.push(ty);
        self.entry()?;
        Ok(true)
    }
    fn assign_words(&mut self, words: &[usize], ty: &str) -> Result<()> {
        for &word in words {
            self.assigned(word, AssignmentBound::Type(Arc::from(ty)))?;
        }
        Ok(())
    }
    fn require_words(&mut self, words: &[usize], ty: &str) -> Result<()> {
        for &word in words {
            self.required(word, Arc::from(ty))?;
        }
        Ok(())
    }
    fn edge(&mut self, src: usize, dst: usize) -> Result<()> {
        ensure!(
            src < self.values.len() && dst < self.values.len(),
            "type edge outside SSA"
        );
        self.entry()?;
        self.forward[src].push(dst);
        self.backward[dst].push(src);
        Ok(())
    }
    fn array_constraint(&mut self, array: usize, opcode: u8, words: &[usize]) -> Result<()> {
        self.entry()?;
        for &word in words {
            ensure!(word < self.values.len(), "array operand outside SSA");
            self.entry()?;
            if opcode <= 0x4a {
                self.dynamic_outputs[word] = true;
            }
        }
        self.arrays
            .get_mut(array)
            .context("array value outside SSA")?
            .push(ArrayConstraint {
                opcode,
                words: words.to_vec(),
            });
        Ok(())
    }
    fn propagate(&mut self) -> Result<()> {
        let mut queue: VecDeque<_> = (0..self.values.len()).collect();
        let mut queued = vec![true; self.values.len()];
        while let Some(value) = queue.pop_front() {
            self.tick()?;
            queued[value] = false;
            let assigned = self.values[value].assignment.clone();
            let required = self.values[value].required_types.clone();
            let truncated = self.values[value].bounds_truncated;
            // JADX TypeUpdate's array listeners: a changed array bound updates
            // the element result/use, rather than equating the array and element.
            for constraint in self.arrays[value].clone() {
                self.tick()?;
                let get = constraint.opcode <= 0x4a;
                for word in constraint.words {
                    let mut changed = false;
                    for bound in &assigned {
                        self.tick()?;
                        match bound {
                            AssignmentBound::Type(array) => {
                                if let Some(component) = array
                                    .strip_prefix('[')
                                    .filter(|c| array_opcode_accepts(constraint.opcode, c))
                                {
                                    if get {
                                        changed |= self.assigned(
                                            word,
                                            AssignmentBound::Type(Arc::from(component)),
                                        )?;
                                    } else {
                                        // An object store has a runtime ArrayStoreException
                                        // check. Do not impose a covariant array's narrower
                                        // runtime component class on every stored SSA use.
                                        changed |= self.required(
                                            word,
                                            Arc::from(if reference(component) {
                                                REF
                                            } else {
                                                component
                                            }),
                                        )?;
                                    }
                                } else if get {
                                    changed |= self.assigned(word, AssignmentBound::Unknown)?;
                                }
                            }
                            AssignmentBound::Literal {
                                bits: 0,
                                wide: false,
                            } => {
                                // Null throws before producing an element; it contributes
                                // no successful component type to a join with a real array.
                            }
                            _ if get => changed |= self.assigned(word, AssignmentBound::Unknown)?,
                            _ => {}
                        }
                    }
                    if truncated && !self.values[word].bounds_truncated {
                        self.values[word].bounds_truncated = true;
                        changed = true;
                    }
                    if changed && !queued[word] {
                        queue.push_back(word);
                        queued[word] = true;
                    }
                }
            }
            for dst in self.forward[value].clone() {
                self.tick()?;
                let mut changed = false;
                for bound in &assigned {
                    changed |= self.assigned(dst, bound.clone())?;
                }
                if truncated && !self.values[dst].bounds_truncated {
                    self.values[dst].bounds_truncated = true;
                    changed = true;
                }
                if changed && !queued[dst] {
                    queue.push_back(dst);
                    queued[dst] = true;
                }
            }
            for src in self.backward[value].clone() {
                self.tick()?;
                let mut changed = false;
                for ty in &required {
                    changed |= self.required(src, ty.clone())?;
                }
                if truncated && !self.values[src].bounds_truncated {
                    self.values[src].bounds_truncated = true;
                    changed = true;
                }
                if changed && !queued[src] {
                    queue.push_back(src);
                    queued[src] = true;
                }
            }
        }
        Ok(())
    }
}
impl InferredTypes {
    pub fn infer(
        method: &DexMethod,
        ir: &DecodedMethod,
        ssa: &SsaMethod,
        calls: &SsaCalls,
        symbols: &DexSymbols,
    ) -> Result<Self> {
        Self::infer_with_work_limit(method, ir, ssa, calls, symbols, 20_000_000)
    }
    pub fn infer_with_work_limit(
        method: &DexMethod,
        ir: &DecodedMethod,
        ssa: &SsaMethod,
        calls: &SsaCalls,
        symbols: &DexSymbols,
        work_limit: usize,
    ) -> Result<Self> {
        let code = method
            .code
            .as_ref()
            .context("type inference requires code")?;
        ensure!(
            ssa.definitions.len() <= LIMIT
                && ssa.instructions.len() <= LIMIT
                && ssa.phis.len() <= LIMIT
                && ir.instructions.len() <= LIMIT
                && calls.calls.len() <= LIMIT,
            "type inference input exceeds budget"
        );
        let count = ssa.definitions.len();
        let mut solver = Solver {
            values: vec![
                ValueType {
                    assignment: vec![],
                    required_types: vec![],
                    resolution: TypeResolution::Unresolved("no assignment bound"),
                    active: false,
                    bounds_truncated: false
                };
                count
            ],
            forward: vec![vec![]; count],
            backward: vec![vec![]; count],
            arrays: vec![vec![]; count],
            dynamic_outputs: vec![false; count],
            work: work_limit,
            entries: 0,
        };
        ensure!(code.ins <= code.registers, "input words exceed registers");
        let mut params = HashMap::new();
        let mut reg = usize::from(code.registers - code.ins);
        ensure!(
            method.parameters.len() <= usize::from(code.ins),
            "parameter count exceeds input words"
        );
        let mut wide_parameters = std::collections::HashSet::new();
        let mut descriptors: Vec<&str> = Vec::new();
        if method.access_flags & 8 == 0 {
            descriptors.push(&method.declaring_type);
        }
        descriptors.extend(method.parameters.iter().map(AsRef::as_ref));
        for ty in descriptors {
            solver.tick()?;
            ensure!(
                reg + width(ty) <= usize::from(code.registers),
                "signature exceeds input words"
            );
            if width(ty) == 2 {
                wide_parameters.insert(reg as u16);
            }
            for _ in 0..width(ty) {
                params.insert(reg, ty);
                reg += 1;
            }
        }
        ensure!(
            reg == usize::from(code.registers),
            "method signature input word mismatch"
        );
        for (id, def) in ssa.definitions.iter().enumerate() {
            solver.tick()?;
            match def.kind {
                DefinitionKind::Parameter => {
                    solver.assigned(
                        id,
                        AssignmentBound::Type(Arc::from(
                            *params
                                .get(&usize::from(def.register))
                                .context("parameter lacks signature type")?,
                        )),
                    )?;
                }
                DefinitionKind::Undefined => {
                    solver.assigned(id, AssignmentBound::Undefined)?;
                }
                _ => {}
            }
        }
        let at: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
        let mut wide_pair_issues = 0;
        let phi_map: HashMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
        let mut checked_pairs = HashMap::new();
        for instruction in &ssa.instructions {
            solver.tick()?;
            let insn = at
                .get(&instruction.pc)
                .context("SSA instruction missing from typed IR")?;
            for operand in instruction.reads.iter().chain(&instruction.writes) {
                for &value in &operand.words {
                    solver
                        .values
                        .get_mut(value)
                        .context("operand outside SSA")?
                        .active = true;
                }
                if operand.words.len() == 2 {
                    let key = (operand.words[0], operand.words[1]);
                    let coherent = match checked_pairs.get(&key) {
                        Some(ok) => *ok,
                        None => {
                            let ok = coherent_pair(
                                ssa,
                                &phi_map,
                                &wide_parameters,
                                &operand.words,
                                &mut solver.work,
                            )?;
                            checked_pairs.insert(key, ok);
                            ok
                        }
                    };
                    if !coherent {
                        wide_pair_issues += 1;
                    }
                }
            }
            let read =
                |idx: usize| -> &[usize] { instruction.reads.get(idx).map_or(&[], |v| &v.words) };
            let write =
                |idx: usize| -> &[usize] { instruction.writes.get(idx).map_or(&[], |v| &v.words) };
            let op = insn.opcode;
            match op {
                0x01..=0x09 => {
                    ensure!(read(0).len() == write(0).len(), "move word count mismatch");
                    for (&src, &dst) in read(0).iter().zip(write(0)) {
                        solver.edge(src, dst)?;
                    }
                }
                0x0d => {
                    let mut found = false;
                    for region in &code.try_regions {
                        solver.tick()?;
                        for (ty, target) in region.catches.iter() {
                            solver.tick()?;
                            if *target as usize == insn.pc {
                                solver.assign_words(
                                    write(0),
                                    ty.as_deref().unwrap_or("Ljava/lang/Throwable;"),
                                )?;
                                found = true;
                            }
                        }
                    }
                    if !found {
                        for &v in write(0) {
                            solver.assigned(v, AssignmentBound::Unknown)?;
                        }
                    }
                }
                0x0f..=0x11 => {
                    solver.require_words(read(0), &method.return_type)?;
                }
                0x12..=0x19 => {
                    for &value in write(0) {
                        solver.assigned(
                            value,
                            AssignmentBound::Literal {
                                bits: insn.literal.context("constant lacks literal")?,
                                wide: op >= 0x16,
                            },
                        )?;
                    }
                }
                0x1a | 0x1b => solver.assign_words(write(0), "Ljava/lang/String;")?,
                0x1c => solver.assign_words(write(0), "Ljava/lang/Class;")?,
                0x1d | 0x1e | 0x26 | 0x27 => solver.require_words(read(0), REF)?,
                0x1f | 0x22 | 0x23 => {
                    let reference = insn
                        .reference
                        .context("typed instruction lacks type reference")?;
                    ensure!(
                        reference.kind == PoolKind::Type,
                        "invalid type reference kind"
                    );
                    let ty = symbols
                        .types
                        .get(reference.index as usize)
                        .context("type reference out of bounds")?;
                    solver.assign_words(write(0), ty)?;
                    if op == 0x1f {
                        solver.require_words(read(0), REF)?;
                    }
                    if op == 0x23 {
                        solver.require_words(read(0), INT)?;
                    }
                }
                0x20 | 0x21 => {
                    solver.require_words(read(0), if op == 0x21 { ARRAY } else { REF })?;
                    solver.assign_words(write(0), if op == 0x20 { "Z" } else { "I" })?;
                }
                0x2b | 0x2c => solver.require_words(read(0), INT)?,
                0x2d..=0x31 => {
                    let ty = match op {
                        0x2d | 0x2e => "F",
                        0x2f | 0x30 => "D",
                        _ => "J",
                    };
                    for operand in &instruction.reads {
                        solver.require_words(&operand.words, ty)?;
                    }
                    solver.assign_words(write(0), "I")?;
                }
                // Equality permits null and boolean; relational comparisons require integer operands.
                0x34..=0x37 | 0x3a..=0x3d => {
                    for operand in &instruction.reads {
                        solver.require_words(&operand.words, INT)?;
                    }
                }
                0x44..=0x51 => {
                    solver.require_words(read(0), ARRAY_USES[usize::from((op - 0x44) % 7)])?;
                    solver.require_words(read(1), INT)?;
                    let array = *read(0)
                        .first()
                        .context("array instruction lacks array operand")?;
                    solver.array_constraint(
                        array,
                        op,
                        if op <= 0x4a { write(0) } else { read(2) },
                    )?;
                    if op == 0x4d {
                        solver.require_words(read(2), REF)?;
                    }
                }
                0x52..=0x6d => {
                    let reference = insn
                        .reference
                        .context("field instruction lacks reference")?;
                    ensure!(
                        reference.kind == PoolKind::Field,
                        "invalid field reference kind"
                    );
                    let (owner, ty, _) = symbols
                        .fields
                        .get(reference.index as usize)
                        .context("field reference out of bounds")?;
                    let ty = symbols
                        .types
                        .get(usize::from(*ty))
                        .context("field type out of bounds")?;
                    let owner = symbols
                        .types
                        .get(usize::from(*owner))
                        .context("field owner out of bounds")?;
                    let is_put = matches!(op, 0x59..=0x5f | 0x67..=0x6d);
                    if is_put {
                        solver.require_words(read(usize::from(op <= 0x5f)), ty)?;
                    } else {
                        solver.assign_words(write(0), ty)?;
                    }
                    if op <= 0x5f {
                        solver.require_words(read(0), owner)?;
                    }
                }
                0x7b..=0x8f => {
                    let (input, output) = unary_types(op);
                    solver.require_words(read(0), input)?;
                    solver.assign_words(write(0), output)?;
                }
                0x90..=0xcf => {
                    let base = if op >= 0xb0 { op - 0x20 } else { op };
                    let ty = match base {
                        0x90..=0x9a => "I",
                        0x9b..=0xa5 => "J",
                        0xa6..=0xaa => "F",
                        _ => "D",
                    };
                    for (index, operand) in instruction.reads.iter().enumerate() {
                        let req = if (index == 1 && matches!(base, 0xa3..=0xa5)) || ty == "I" {
                            INT
                        } else {
                            ty
                        };
                        solver.require_words(&operand.words, req)?;
                    }
                    if !matches!(base, 0x95..=0x97) {
                        solver.assign_words(write(0), ty)?;
                    }
                }
                0xd0..=0xe2 => {
                    solver.require_words(read(0), INT)?;
                    // Boolean bitwise operations require a dynamic constraint, not an assumed int.
                    if !matches!(op, 0xd5..=0xd7 | 0xdd..=0xdf) {
                        solver.assign_words(write(0), "I")?;
                    }
                }
                0xfe => solver.assign_words(write(0), "Ljava/lang/invoke/MethodHandle;")?,
                0xff => solver.assign_words(write(0), "Ljava/lang/invoke/MethodType;")?,
                _ => {}
            }
        }
        for phi in &ssa.phis {
            solver.tick()?;
            solver
                .values
                .get_mut(phi.result)
                .context("phi outside SSA")?
                .active = true;
            for &(_, incoming) in &phi.incoming {
                solver.edge(incoming, phi.result)?;
                solver.values[incoming].active = true;
            }
        }
        for call in &calls.calls {
            solver.tick()?;
            for argument in call.receiver.iter().chain(&call.arguments) {
                if argument.words.len() == 2
                    && !coherent_pair(
                        ssa,
                        &phi_map,
                        &wide_parameters,
                        &argument.words,
                        &mut solver.work,
                    )?
                {
                    wide_pair_issues += 1;
                }
                solver.require_words(&argument.words, &argument.descriptor)?;
            }
            if let Some(result) = &call.result {
                if result.words.len() == 2
                    && !coherent_pair(
                        ssa,
                        &phi_map,
                        &wide_parameters,
                        &result.words,
                        &mut solver.work,
                    )?
                {
                    wide_pair_issues += 1;
                }
                solver.assign_words(&result.words, &result.descriptor)?;
            }
        }
        for id in 0..count {
            if solver.values[id].assignment.is_empty()
                && solver.backward[id].is_empty()
                && !solver.dynamic_outputs[id]
            {
                solver.assigned(id, AssignmentBound::Unknown)?;
            }
        }
        solver.propagate()?;
        // Only after the first fixed point can empty dynamic producers be
        // classified as unknown (null-only arrays, unsupported/cyclic seeds).
        let mut seeded = false;
        for id in 0..count {
            if solver.values[id].assignment.is_empty() {
                seeded |= solver.assigned(id, AssignmentBound::Unknown)?;
            }
        }
        if seeded {
            solver.propagate()?;
        }
        let mut output = Self {
            values: solver.values,
            resolved_values: 0,
            unresolved_values: 0,
            conflicting_values: 0,
            wide_pair_issues,
        };
        for value in &mut output.values {
            value.resolution = resolve(value, symbols);
            if value.active {
                match value.resolution {
                    TypeResolution::Resolved(_) => output.resolved_values += 1,
                    TypeResolution::Unresolved(_) => output.unresolved_values += 1,
                    TypeResolution::Conflict(_) => output.conflicting_values += 1,
                }
            }
        }
        Ok(output)
    }
}
fn array_opcode_accepts(op: u8, component: &str) -> bool {
    match if op >= 0x4b { op - 7 } else { op } {
        0x44 => matches!(component, "I" | "F"),
        0x45 => matches!(component, "J" | "D"),
        0x46 => component.starts_with('[') || component.starts_with('L'),
        0x47 => component == "Z",
        0x48 => component == "B",
        0x49 => component == "C",
        0x4a => component == "S",
        _ => false,
    }
}
fn width(ty: &str) -> usize {
    if matches!(ty, "J" | "D") { 2 } else { 1 }
}
fn reference(ty: &str) -> bool {
    ty.starts_with('L')
        || ty.starts_with('[')
        || ty == REF
        || ty == ARRAY
        || ARRAY_USES.contains(&ty)
}
fn int_like(ty: &str) -> bool {
    matches!(ty, "Z" | "B" | "C" | "S" | "I" | INT)
}
fn unary_types(op: u8) -> (&'static str, &'static str) {
    match op {
        0x7b | 0x7c => (INT, "I"),
        0x7d | 0x7e => ("J", "J"),
        0x7f => ("F", "F"),
        0x80 => ("D", "D"),
        0x81 => (INT, "J"),
        0x82 => (INT, "F"),
        0x83 => (INT, "D"),
        0x84 => ("J", "I"),
        0x85 => ("J", "F"),
        0x86 => ("J", "D"),
        0x87 => ("F", "I"),
        0x88 => ("F", "J"),
        0x89 => ("F", "D"),
        0x8a => ("D", "I"),
        0x8b => ("D", "J"),
        0x8c => ("D", "F"),
        0x8d => (INT, "B"),
        0x8e => (INT, "C"),
        _ => (INT, "S"),
    }
}
fn literal_fits(bits: i64, wide: bool, ty: &str) -> bool {
    if wide {
        return matches!(ty, "J" | "D");
    }
    match ty {
        "Z" => matches!(bits, 0 | 1),
        "B" => i8::try_from(bits).is_ok(),
        "C" => u16::try_from(bits).is_ok(),
        "S" => i16::try_from(bits).is_ok(),
        "I" | "F" | INT => true,
        _ => bits == 0 && reference(ty),
    }
}
fn resolve(value: &ValueType, symbols: &DexSymbols) -> TypeResolution {
    use TypeResolution::*;
    if value.bounds_truncated {
        return Unresolved("type bound alternative limit");
    }
    if value.assignment.is_empty() {
        return Unresolved("no assignment bound");
    }
    let mut candidates = Vec::<Arc<str>>::new();
    let mut has_literal = false;
    let mut hierarchy = false;
    for bound in &value.assignment {
        match bound {
            AssignmentBound::Undefined => return Unresolved("undefined incoming SSA value"),
            AssignmentBound::Unknown => return Unresolved("unresolved assignment producer"),
            AssignmentBound::Literal { bits, wide } => {
                has_literal = true;
                for required in &value.required_types {
                    if !literal_fits(*bits, *wide, required) {
                        return Conflict("literal incompatible with required type");
                    }
                }
            }
            AssignmentBound::Type(ty) => {
                if !candidates.contains(ty) {
                    candidates.push(ty.clone());
                }
                for required in &value.required_types {
                    if required.as_ref() == ARRAY {
                        if ty.starts_with('[') {
                            continue;
                        }
                        return Conflict("array instruction requires array type");
                    }
                    if let Some(index) = ARRAY_USES.iter().position(|r| *r == required.as_ref()) {
                        if ty.strip_prefix('[').is_some_and(|component| {
                            array_opcode_accepts(0x44 + index as u8, component)
                        }) {
                            continue;
                        }
                        return Conflict("array component incompatible with opcode");
                    }
                    if ty == required
                        || (required.as_ref() == INT && int_like(ty))
                        || (required.as_ref() == REF && reference(ty))
                    {
                        continue;
                    }
                    if reference(ty) && reference(required) {
                        match symbols.hierarchy.get().map(|h| h.assignable(ty, required)) {
                            Some(crate::native_hierarchy::Relation::Proven) => {}
                            Some(crate::native_hierarchy::Relation::Disproven) => {
                                if symbols.hierarchy.get().is_some_and(|h| {
                                    h.assignable(required, ty)
                                        == crate::native_hierarchy::Relation::Proven
                                }) {
                                    return Unresolved("reference narrowing conversion required");
                                }
                                // Assignability alone does not decide Java castability
                                // (especially interface calls). Preserve this unsatisfied
                                // use bound for a later conversion/castability pass.
                                return Unresolved("reference conversion requires validation");
                            }
                            _ => hierarchy = true,
                        }
                    } else if int_like(ty) && int_like(required) {
                        if !matches!(
                            (ty.as_ref(), required.as_ref()),
                            ("B", "S" | "I") | ("C" | "S", "I")
                        ) {
                            return Unresolved("integer or boolean conversion required");
                        }
                    } else {
                        return Conflict("assignment incompatible with required type");
                    }
                }
            }
        }
    }
    if candidates.iter().any(|t| reference(t)) && candidates.iter().any(|t| !reference(t)) {
        return Conflict("primitive and reference phi inputs");
    }
    if candidates.len() > 1 {
        // A bound already present at the join may be a proven supertype of all
        // other incoming bounds. Preserve each incoming assignment; choose only
        // the phi's output type. General least-upper-bound discovery is separate.
        let selected = symbols.hierarchy.get().and_then(|h| {
            candidates.iter().position(|candidate| {
                reference(candidate)
                    && candidates.iter().all(|source| {
                        h.assignable(source, candidate) == crate::native_hierarchy::Relation::Proven
                    })
            })
        });
        if let Some(index) = selected {
            let selected = candidates[index].clone();
            candidates.clear();
            candidates.push(selected);
        } else {
            return Unresolved(if candidates.iter().all(|t| reference(t)) {
                "reference join requires hierarchy"
            } else {
                "multiple assignment types"
            });
        }
    }
    if hierarchy {
        return Unresolved("reference compatibility requires hierarchy");
    }
    if let Some(ty) = candidates.first() {
        if has_literal && value.assignment.iter().any(|b| matches!(b, AssignmentBound::Literal{bits,wide} if !literal_fits(*bits,*wide,ty))) {
            // A narrow primitive and a 32-bit integer literal can join as int,
            // for example charAt's character versus a negative sentinel.
            if matches!(ty.as_ref(),"B"|"C"|"S"|"I") && value.assignment.iter().all(|b| match b {
                AssignmentBound::Literal{bits,wide} => !wide && i32::try_from(*bits).is_ok(),
                AssignmentBound::Type(t) => matches!(t.as_ref(),"B"|"C"|"S"|"I"),
                _ => false,
            }) { return Resolved(Arc::from("I")); }
            return Conflict("literal incompatible with phi assignment");
        }
        return Resolved(ty.clone());
    }
    if value.required_types.iter().any(|t| reference(t))
        && value.required_types.iter().any(|t| !reference(t))
    {
        return Unresolved("literal has reference and primitive uses");
    }
    let exact: Vec<_> = value
        .required_types
        .iter()
        .filter(|t| !t.starts_with('<'))
        .collect();
    if exact.len() == 1 {
        return Resolved((*exact[0]).clone());
    }
    Unresolved(if exact.len() > 1 {
        "literal has multiple use types"
    } else {
        "literal type remains ambiguous"
    })
}
/// Compare wide halves structurally, including paired phi incoming edges. This
/// is a conservative proof: unrelated compatible words are not silently merged.
fn coherent_pair(
    ssa: &SsaMethod,
    phis: &HashMap<usize, &crate::native_ssa::Phi>,
    wide_parameters: &std::collections::HashSet<u16>,
    words: &[usize],
    work: &mut usize,
) -> Result<bool> {
    let mut todo = vec![(words[0], words[1])];
    let mut seen = std::collections::HashSet::new();
    while let Some((low, high)) = todo.pop() {
        ensure!(*work > 0, "type inference exceeds work budget");
        *work -= 1;
        if !seen.insert((low, high)) {
            continue;
        }
        let a = ssa.definitions.get(low).context("wide value outside SSA")?;
        let b = ssa
            .definitions
            .get(high)
            .context("wide value outside SSA")?;
        if a.register.checked_add(1) != Some(b.register) {
            return Ok(false);
        }
        match (&a.kind, &b.kind) {
            (DefinitionKind::Parameter, DefinitionKind::Parameter)
                if wide_parameters.contains(&a.register) => {}
            (
                DefinitionKind::Instruction { pc: p, word: 0, .. },
                DefinitionKind::Instruction { pc: q, word: 1, .. },
            ) if p == q => {}
            (DefinitionKind::Phi { block: p }, DefinitionKind::Phi { block: q }) if p == q => {
                let a = phis.get(&low).context("wide phi missing")?;
                let b = phis.get(&high).context("wide phi missing")?;
                if a.incoming.len() != b.incoming.len() {
                    return Ok(false);
                }
                for ((pa, va), (pb, vb)) in a.incoming.iter().zip(&b.incoming) {
                    if pa != pb {
                        return Ok(false);
                    }
                    todo.push((*va, *vb));
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}
