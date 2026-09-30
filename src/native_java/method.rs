//! Conservative structured DEX to Java lowering. Every effectful expression is
//! materialized immediately; registers hold immutable source values, not delayed
//! expressions. Unsupported instructions reject the entire method.
#[path = "allocation.rs"]
mod allocation;
#[path = "allocation_lowering.rs"]
mod allocation_lowering;
#[path = "cleanup.rs"]
mod cleanup;
#[path = "concat.rs"]
mod concat;
#[path = "finally_regions.rs"]
mod finally_regions;
#[path = "numeric.rs"]
mod numeric;
#[path = "operations.rs"]
mod operations;
#[path = "receiver_cleanup.rs"]
mod receiver_cleanup;
#[path = "shrink.rs"]
mod shrink;
#[path = "synchronized.rs"]
mod synchronized;
#[path = "throwing.rs"]
mod throwing;
use super::{java_type, names};
use crate::{
    engine::CodeLink,
    native_dex::{DexClass, DexMethod},
};
use anyhow::{Context, Result, bail, ensure};
use numeric::{Kind, Literal};

pub(super) fn validate_exception_type(class: &DexClass, ty: &str) -> Result<()> {
    throwing::validate_type(class, ty)
}

pub(super) struct MethodBody {
    pub inferred_throws: Vec<String>,
    pub text: String,
    pub links: Vec<CodeLink>,
}

pub(super) fn readable_code(
    mut code: crate::engine::DecompiledCode,
) -> crate::engine::DecompiledCode {
    let mut body = MethodBody {
        text: code.source,
        links: code.links,
        inferred_throws: vec![],
    };
    cleanup::readable(&mut body);
    code.source = body.text;
    code.links = body.links;
    code.source_hash = crate::engine::source_identity(&code.source);
    code
}
#[derive(Clone, PartialEq, Eq)]
struct Value {
    text: String,
    ty: String,
    literal: Option<i32>,
    wide_literal: Option<u64>,
    // Dynamic selection among untyped DEX constants, never a typed int computation.
    raw_bits32: bool,
}
#[derive(Clone, Default)]
struct Output {
    text: String,
    links: Vec<CodeLink>,
    sequence: usize,
    chars: usize,
    indent: usize,
    receiver_locals: std::collections::HashSet<String>,
    inferred_throws: std::collections::BTreeSet<String>,
    last_local: Option<EmittedLocal>,
}
#[derive(Clone)]
struct EmittedLocal {
    value: Value,
    expression: String,
    refs: Vec<(usize, usize, String)>,
    byte_start: usize,
    char_start: usize,
    link_start: usize,
    indent: usize,
}
impl Output {
    fn append(&mut self, child: Output) {
        self.last_local = None;
        self.receiver_locals.extend(child.receiver_locals);
        self.inferred_throws.extend(child.inferred_throws);
        for mut link in child.links {
            link.start += self.chars;
            link.end += self.chars;
            self.links.push(link);
        }
        self.chars += child.chars;
        self.text.push_str(&child.text);
    }
    fn line(&mut self, text: &str, refs: &[(usize, usize, String)]) {
        self.last_local = None;
        let spaces = 8 + self.indent * 4;
        let base = self.chars + spaces;
        self.chars += spaces + 1 + text.chars().count();
        self.text.push_str(&" ".repeat(spaces));
        self.text.push_str(text);
        self.text.push('\n');
        for (start, len, label) in refs {
            self.links.push(CodeLink {
                start: base + start,
                end: base + start + len,
                label: label.clone(),
            });
        }
    }
    fn local(
        &mut self,
        ty: &str,
        expression: &str,
        refs: &[(usize, usize, String)],
    ) -> Result<Value> {
        let name = format!("v{}", self.sequence);
        self.sequence += 1;
        if expression != "null" && !expression.contains("new ") {
            self.receiver_locals.insert(name.clone());
        }
        let prefix = format!("{} {name} = ", java_type(ty)?);
        let mut adjusted: Vec<_> = refs
            .iter()
            .map(|(start, len, label)| (start + prefix.chars().count(), *len, label.clone()))
            .collect();
        if let Some(label) = class_label(ty) {
            adjusted.push((0, java_type(ty)?.chars().count(), label));
        }
        let (byte_start, char_start, link_start) = (self.text.len(), self.chars, self.links.len());
        self.line(&format!("{prefix}{expression};"), &adjusted);
        ensure!(
            self.text.len() <= 4 * 1024 * 1024,
            "reconstructed method exceeds output budget"
        );
        let value = Value {
            text: name,
            ty: ty.into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
        self.last_local = Some(EmittedLocal {
            value: value.clone(),
            expression: expression.into(),
            refs: refs.to_vec(),
            byte_start,
            char_start,
            link_start,
            indent: self.indent,
        });
        Ok(value)
    }

    // Collapse only two adjacent emitted statements of identical static type.
    // No expression is moved past a statement, block boundary or conversion.
    // Metadata comes from local(), never from parsing the rendered Java.
    fn return_value(&mut self, value: &Value, converted: &str, return_type: &str) {
        if let Some(local) = self.last_local.take()
            && local.value == *value
            && converted == value.text
            && value.ty == return_type
            && local.indent == self.indent
        {
            self.text.truncate(local.byte_start);
            self.chars = local.char_start;
            self.links.truncate(local.link_start);
            let refs: Vec<_> = local
                .refs
                .into_iter()
                .map(|(start, len, label)| (start + 7, len, label))
                .collect();
            self.line(&format!("return {};", local.expression), &refs);
        } else {
            self.line(&format!("return {converted};"), &[]);
        }
    }
}
fn class_label(ty: &str) -> Option<String> {
    ty.trim_start_matches('[')
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
        .map(|s| s.replace('/', "."))
}

fn reference(ty: &str) -> bool {
    ty.starts_with('L') || ty.starts_with('[')
}
fn argument(value: &Value, ty: &str) -> Result<String> {
    if let Some(bits) = value.wide_literal {
        let kind = match ty {
            "J" => Kind::Long,
            "D" => Kind::Double,
            _ => bail!("Wide literal used as narrow value"),
        };
        return Literal::Bits64(bits).render(kind);
    }
    if ty == "F"
        && let Some(bits) = value.literal
    {
        return Literal::Bits32(bits as u32).render(Kind::Float);
    }
    if ty == "F" && value.raw_bits32 {
        ensure!(
            matches!(value.ty.as_str(), "I" | "Z"),
            "invalid raw constant join type"
        );
        return Ok(format!(
            "java.lang.Float.intBitsToFloat({})",
            integral(value)?
        ));
    }
    if value.ty == "Z" && matches!(ty, "I" | "B" | "S" | "C") {
        // DEX's narrow register family also holds boolean 0/1 values. A merge
        // may choose boolean while a later byte/short/char use needs the same
        // numeric bits. Java requires explicit boolean-to-number reification;
        // both alternatives fit every narrow integral target exactly.
        let numeric = format!("({} ? 1 : 0)", value.text);
        return Ok(if ty == "I" {
            numeric
        } else {
            format!("({}) {numeric}", java_type(ty)?)
        });
    }

    if value.ty == ty || (ty == "I" && matches!(value.ty.as_str(), "B" | "S" | "C")) {
        return Ok(value.text.clone());
    }
    if let Some(n) = value.literal {
        return Ok(match ty {
            "Z" => {
                ensure!(n == 0 || n == 1, "nonboolean literal");
                if n == 0 {
                    "false".into()
                } else {
                    "true".into()
                }
            }
            "B" | "S" | "C" => format!("({}) {n}", java_type(ty)?),
            _ if reference(ty) && n == 0 => "null".into(),
            _ => bail!("unsupported literal conversion"),
        });
    }
    if reference(&value.ty) && reference(ty) {
        return Ok(format!("(({}) {})", java_type(ty)?, value.text));
    }
    bail!("unsupported register type conversion {} to {ty}", value.ty)
}

// DEX constants are untyped bits. A loop may materialize them in Java int
// locals before a boolean or float use. Reinterpret only when every physical
// definition (including copied sources and writes after backward edges) is a
// narrow DEX constant. Boolean uses additionally require the 0/1 domain.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConstantDomain {
    Boolean,
    Raw32,
}
fn constant_register_domain(
    code: &crate::native_dex::DexCode,
    graph: &Graph,
    source: usize,
    domain: ConstantDomain,
) -> Result<bool> {
    if code.instructions.len() > 1024 {
        return Ok(false);
    }
    let mut pending = vec![source];
    let mut seen = vec![false; code.registers as usize];
    while let Some(register) = pending.pop() {
        if register >= seen.len() || register >= usize::from(code.registers - code.ins) {
            return Ok(false);
        }
        if seen[register] {
            // A copy cycle does not establish a constant value by itself.
            return Ok(false);
        }
        seen[register] = true;
        let mut found_write = false;
        let mut pc = 0;
        while pc < code.instructions.len() {
            let width = graph.widths[pc];
            if width == 0 {
                pc += 1;
                continue;
            }
            let writes = graph.written_in(code, pc, pc + width);
            let Some(writes) = writes else {
                return Ok(false);
            };
            if writes[register] {
                found_write = true;
                match code.instructions[pc] as u8 {
                    0x12..=0x15 => {
                        let word = code.instructions[pc];
                        let opcode = word as u8;
                        let (dst, literal) = if let Some(instruction) = graph.instruction(pc)? {
                            ensure!(
                                instruction.opcode == opcode,
                                "shared constant opcode differs from selected source"
                            );
                            let (dst, literal) = decoded_constant(instruction, seen.len())?;
                            (dst, i32::try_from(literal)?)
                        } else {
                            let dst = if opcode == 0x12 {
                                ((word >> 8) & 15) as usize
                            } else {
                                (word >> 8) as usize
                            };
                            let literal = match opcode {
                                0x12 => ((word as i16) >> 12) as i32,
                                0x13 => i32::from(code.instructions[pc + 1] as i16),
                                0x14 => {
                                    i32::from(code.instructions[pc + 1])
                                        | (i32::from(code.instructions[pc + 2]) << 16)
                                }
                                _ => i32::from(code.instructions[pc + 1] as i16) << 16,
                            };
                            (dst, literal)
                        };
                        if dst != register
                            || (domain == ConstantDomain::Boolean && !matches!(literal, 0 | 1))
                        {
                            return Ok(false);
                        }
                    }
                    0x01..=0x03 => {
                        let (dst, copied) = if let Some(instruction) = graph.instruction(pc)? {
                            decoded_move(instruction, code.registers as usize)?
                        } else {
                            let word = code.instructions[pc];
                            match word as u8 {
                                0x01 => (((word >> 8) & 15) as usize, (word >> 12) as usize),
                                0x02 => ((word >> 8) as usize, code.instructions[pc + 1] as usize),
                                _ => (
                                    code.instructions[pc + 1] as usize,
                                    code.instructions[pc + 2] as usize,
                                ),
                            }
                        };
                        if dst != register {
                            return Ok(false);
                        }
                        pending.push(copied);
                    }
                    _ => return Ok(false),
                }
            }
            pc += width;
        }
        if !found_write {
            return Ok(false);
        }
    }
    Ok(true)
}

fn argument_from_register(
    value: &Value,
    ty: &str,
    source: usize,
    code: &crate::native_dex::DexCode,
    graph: &Graph,
) -> Result<String> {
    if ty == "Z"
        && value.ty == "I"
        && value.literal.is_none()
        && constant_register_domain(code, graph, source, ConstantDomain::Boolean)?
    {
        return Ok(format!("({} != 0)", value.text));
    }
    if ty == "F"
        && matches!(value.ty.as_str(), "I" | "Z")
        && value.literal.is_none()
        && !value.raw_bits32
        && constant_register_domain(code, graph, source, ConstantDomain::Raw32)?
    {
        return Ok(format!(
            "java.lang.Float.intBitsToFloat({})",
            integral(value)?
        ));
    }
    argument(value, ty)
}

// A DEX boolean return may merge a zero/one literal with the result of a
// boolean call, especially across a handler that observes the pre-call value.
// Prove the exact SSA value read by this return. Physical-register scans cannot
// distinguish that lifetime from unrelated integer writes elsewhere.
fn proven_boolean_return(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    pc: usize,
    source: usize,
) -> Result<bool> {
    let code = method.code.as_ref().context("missing return code")?;
    if code.instructions.len() > 512 || code.registers > 256 {
        return Ok(false);
    }
    if let Some(front) = &graph.front_end {
        ensure!(
            front.ir == crate::native_ir::DecodedMethod::decode(code)?,
            "selected return operands differ from decoded method"
        );
        let bound = crate::native_calls::BoundCalls::bind(code, &front.ir, &class.symbols)?;
        ensure!(
            bound == front.bound,
            "selected call binding differs from decoded method"
        );
        let cfg = if graph.shared_loops.is_empty() {
            crate::native_cfg::ControlFlowGraph::from_decoded(&front.ir, code.instructions.len())?
        } else {
            crate::native_cfg::ControlFlowGraph::from_decoded_loop(
                &front.ir,
                code.instructions.len(),
            )?
        };
        if let Some(selected) = &graph.shared_cfg {
            ensure!(
                *selected == cfg,
                "selected return CFG differs from decoded method"
            );
        }
        let ssa =
            crate::native_ssa::SsaMethod::build_with_work_limit(code, &front.ir, &cfg, 2_000_000)?;
        let calls = crate::native_call_values::SsaCalls::bind(&bound, &ssa)?;
        return Ok(
            boolean_return_definition_proven(&front.ir, &ssa, &calls, pc, source)
                || boolean_return_or_proven(method, &front.ir, &ssa, &calls, pc, source),
        );
    }
    let analysis = crate::native_method::MethodAnalysis::build(class, method)?;
    Ok(boolean_return_definition_proven(
        analysis.instructions(),
        analysis.ssa(),
        analysis.calls(),
        pc,
        source,
    ) || boolean_return_or_proven(
        method,
        analysis.instructions(),
        analysis.ssa(),
        analysis.calls(),
        pc,
        source,
    ))
}

fn boolean_return_definition_proven(
    ir: &crate::native_ir::DecodedMethod,
    ssa: &crate::native_ssa::SsaMethod,
    calls: &crate::native_call_values::SsaCalls,
    pc: usize,
    source: usize,
) -> bool {
    use crate::native_ssa::DefinitionKind;

    if ssa.definitions.len() > 16_384 {
        return false;
    }
    let Some(return_ir) = ir
        .instructions
        .iter()
        .find(|instruction| instruction.pc == pc)
    else {
        return false;
    };
    let Some(return_ssa) = ssa
        .instructions
        .iter()
        .find(|instruction| instruction.pc == pc)
    else {
        return false;
    };
    if return_ir.opcode != 0x0f
        || return_ssa.reads.len() != 1
        || usize::from(return_ssa.reads[0].register) != source
        || return_ssa.reads[0].words.len() != 1
    {
        return false;
    }
    let root = return_ssa.reads[0].words[0];
    let mut state = vec![0u8; ssa.definitions.len()];
    let mut pending = vec![(root, false)];
    let mut anchors = 0usize;
    while let Some((id, finished)) = pending.pop() {
        let Some(definition) = ssa.definitions.get(id) else {
            return false;
        };
        if finished {
            state[id] = 2;
            continue;
        }
        match state[id] {
            1 => return false, // cyclic phi needs a separate fixed-point proof
            2 => continue,
            _ => {}
        }
        state[id] = 1;
        let dependencies = match definition.kind {
            DefinitionKind::Parameter | DefinitionKind::Undefined => return false,
            DefinitionKind::Phi { block } => {
                let mut phis = ssa.phis.iter().filter(|phi| {
                    phi.result == id
                        && phi.block == block
                        && phi.register == definition.register
                        && ssa.reachable.get(block) == Some(&true)
                });
                let Some(phi) = phis.next() else { return false };
                if phis.next().is_some() || phi.incoming.is_empty() {
                    return false;
                }
                phi.incoming
                    .iter()
                    .map(|(_, value)| *value)
                    .collect::<Vec<_>>()
            }
            DefinitionKind::Instruction { pc, word, .. } => {
                if word != 0 {
                    return false;
                }
                let Some(instruction) = ir.instructions.iter().find(|i| i.pc == pc) else {
                    return false;
                };
                let Some(ssa_instruction) = ssa.instructions.iter().find(|i| i.pc == pc) else {
                    return false;
                };
                if ssa_instruction.writes.len() != 1
                    || ssa_instruction.writes[0].register != definition.register
                    || ssa_instruction.writes[0].words.as_slice() != [id]
                    || instruction.may_throw
                    || instruction.reference.is_some()
                    || instruction.prototype.is_some()
                    || instruction.branch_target.is_some()
                    || instruction.payload_target.is_some()
                {
                    return false;
                }
                match instruction.opcode {
                    0x12..=0x15 => {
                        let width = match instruction.opcode {
                            0x12 => 1,
                            0x14 => 3,
                            _ => 2,
                        };
                        if instruction.width != width
                            || !instruction.reads.is_empty()
                            || !matches!(instruction.literal, Some(0 | 1))
                        {
                            return false;
                        }
                        anchors += 1;
                        vec![]
                    }
                    0x01..=0x03 => {
                        let width = usize::from(instruction.opcode - 0x01) + 1;
                        if instruction.width != width
                            || instruction.literal.is_some()
                            || ssa_instruction.reads.len() != 1
                            || ssa_instruction.reads[0].words.len() != 1
                        {
                            return false;
                        }
                        vec![ssa_instruction.reads[0].words[0]]
                    }
                    0x0a => {
                        if instruction.width != 1
                            || instruction.literal.is_some()
                            || !ssa_instruction.reads.is_empty()
                            || calls
                                .calls
                                .iter()
                                .filter(|call| {
                                    call.result.as_ref().is_some_and(|result| {
                                        result.descriptor.as_ref() == "Z"
                                            && result.words.as_slice() == [id]
                                    })
                                })
                                .take(2)
                                .count()
                                != 1
                        {
                            return false;
                        }
                        anchors += 1;
                        vec![]
                    }
                    _ => return false,
                }
            }
        };
        if dependencies.is_empty() {
            state[id] = 2;
        } else {
            pending.push((id, true));
            pending.extend(
                dependencies
                    .into_iter()
                    .rev()
                    .map(|dependency| (dependency, false)),
            );
        }
    }
    anchors != 0
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BooleanDomain {
    Bottom,
    Zero,
    One,
    Both,
    Unknown,
}

impl BooleanDomain {
    fn join(self, other: Self) -> Self {
        use BooleanDomain::*;
        match (self, other) {
            (Bottom, value) | (value, Bottom) => value,
            (Unknown, _) | (_, Unknown) => Unknown,
            (left, right) if left == right => left,
            _ => Both,
        }
    }

    fn or(self, other: Self) -> Self {
        use BooleanDomain::*;
        if self == Bottom || other == Bottom {
            return Bottom;
        }
        if self == Unknown || other == Unknown {
            return Unknown;
        }
        match (self, other) {
            (Zero, Zero) => Zero,
            (One, _) | (_, One) => One,
            _ => Both,
        }
    }

    fn proven(self) -> bool {
        matches!(self, Self::Zero | Self::One | Self::Both)
    }
}

enum BooleanNode {
    Value(BooleanDomain),
    Copy(usize),
    Phi(Vec<usize>),
    Or(usize, usize),
}

impl BooleanNode {
    fn dependencies(&self) -> Vec<usize> {
        match self {
            Self::Value(_) => vec![],
            Self::Copy(value) => vec![*value],
            Self::Phi(values) => values.clone(),
            Self::Or(left, right) => vec![*left, *right],
        }
    }

    fn transfer(&self, values: &[BooleanDomain]) -> BooleanDomain {
        match self {
            Self::Value(value) => *value,
            Self::Copy(source) => values[*source],
            Self::Phi(incoming) => incoming
                .iter()
                .fold(BooleanDomain::Bottom, |value, source| {
                    value.join(values[*source])
                }),
            Self::Or(left, right) => values[*left].or(values[*right]),
        }
    }
}

// A bitwise OR of two DEX boolean words remains a boolean word. The accumulator
// can be a loop phi, so compute the least fixed point of only the return value's
// SSA dependency cone. Bottom never proves a cycle without an entry definition.
fn boolean_return_or_proven(
    method: &DexMethod,
    ir: &crate::native_ir::DecodedMethod,
    ssa: &crate::native_ssa::SsaMethod,
    calls: &crate::native_call_values::SsaCalls,
    pc: usize,
    source: usize,
) -> bool {
    use crate::native_ssa::DefinitionKind;
    use std::collections::{HashSet, VecDeque};

    let Some(code) = method.code.as_ref() else {
        return false;
    };
    if ssa.definitions.len() > 16_384 || code.ins > code.registers {
        return false;
    }
    let Some(return_ir) = ir
        .instructions
        .iter()
        .find(|instruction| instruction.pc == pc)
    else {
        return false;
    };
    let Some(return_ssa) = ssa
        .instructions
        .iter()
        .find(|instruction| instruction.pc == pc)
    else {
        return false;
    };
    if return_ir.opcode != 0x0f
        || return_ssa.reads.len() != 1
        || usize::from(return_ssa.reads[0].register) != source
        || return_ssa.reads[0].words.len() != 1
    {
        return false;
    }
    let root = return_ssa.reads[0].words[0];
    if root >= ssa.definitions.len() {
        return false;
    }
    let mut boolean_parameters = HashSet::new();
    let mut parameter = code.registers - code.ins;
    if method.access_flags & 0x8 == 0 {
        let Some(next) = parameter.checked_add(1) else {
            return false;
        };
        parameter = next;
    }
    for descriptor in &method.parameters {
        if descriptor.as_ref() == "Z" {
            boolean_parameters.insert(parameter);
        }
        let width = if matches!(descriptor.as_ref(), "J" | "D") {
            2
        } else {
            1
        };
        let Some(next) = parameter.checked_add(width) else {
            return false;
        };
        parameter = next;
    }
    if parameter != code.registers {
        return false;
    }

    let mut nodes: Vec<Option<BooleanNode>> = (0..ssa.definitions.len()).map(|_| None).collect();
    let mut pending = vec![root];
    let mut saw_or = false;
    while let Some(id) = pending.pop() {
        let Some(definition) = ssa.definitions.get(id) else {
            return false;
        };
        if nodes[id].is_some() {
            continue;
        }
        let node = match definition.kind {
            DefinitionKind::Parameter => {
                BooleanNode::Value(if boolean_parameters.contains(&definition.register) {
                    BooleanDomain::Both
                } else {
                    BooleanDomain::Unknown
                })
            }
            DefinitionKind::Undefined => BooleanNode::Value(BooleanDomain::Unknown),
            DefinitionKind::Phi { block } => {
                let phis: Vec<_> = ssa
                    .phis
                    .iter()
                    .filter(|phi| {
                        phi.result == id
                            && phi.block == block
                            && phi.register == definition.register
                    })
                    .collect();
                if phis.len() != 1 || ssa.reachable.get(block) != Some(&true) {
                    BooleanNode::Value(BooleanDomain::Unknown)
                } else {
                    let phi = phis[0];
                    let mut expected = std::collections::BTreeSet::new();
                    if block == 0 {
                        expected.insert(None);
                    }
                    for (pred, cfg_block) in ssa.graph.blocks.iter().enumerate() {
                        if ssa.reachable.get(pred) == Some(&true)
                            && cfg_block.successors.iter().any(|edge| edge.target == block)
                        {
                            expected.insert(Some(pred));
                        }
                    }
                    let actual: std::collections::BTreeSet<_> =
                        phi.incoming.iter().map(|(pred, _)| *pred).collect();
                    if expected.is_empty()
                        || actual != expected
                        || actual.len() != phi.incoming.len()
                    {
                        BooleanNode::Value(BooleanDomain::Unknown)
                    } else {
                        BooleanNode::Phi(phi.incoming.iter().map(|(_, value)| *value).collect())
                    }
                }
            }
            DefinitionKind::Instruction { pc, word, block } => {
                let decoded = ir
                    .instructions
                    .iter()
                    .find(|instruction| instruction.pc == pc);
                let renamed = ssa
                    .instructions
                    .iter()
                    .find(|instruction| instruction.pc == pc);
                if word != 0 || ssa.reachable.get(block) != Some(&true) {
                    BooleanNode::Value(BooleanDomain::Unknown)
                } else if let (Some(decoded), Some(renamed)) = (decoded, renamed) {
                    if renamed.writes.len() != 1
                        || renamed.writes[0].register != definition.register
                        || renamed.writes[0].words.as_slice() != [id]
                        || decoded.may_throw
                        || decoded.reference.is_some()
                        || decoded.prototype.is_some()
                        || decoded.branch_target.is_some()
                        || decoded.payload_target.is_some()
                    {
                        BooleanNode::Value(BooleanDomain::Unknown)
                    } else {
                        match decoded.opcode {
                            0x12..=0x15
                                if decoded.width
                                    == match decoded.opcode {
                                        0x12 => 1,
                                        0x14 => 3,
                                        _ => 2,
                                    }
                                    && decoded.reads.is_empty()
                                    && renamed.reads.is_empty() =>
                            {
                                BooleanNode::Value(match decoded.literal {
                                    Some(0) => BooleanDomain::Zero,
                                    Some(1) => BooleanDomain::One,
                                    _ => BooleanDomain::Unknown,
                                })
                            }
                            0x01..=0x03
                                if decoded.width == usize::from(decoded.opcode - 0x01) + 1
                                    && decoded.literal.is_none()
                                    && renamed.reads.len() == 1
                                    && renamed.reads[0].words.len() == 1 =>
                            {
                                BooleanNode::Copy(renamed.reads[0].words[0])
                            }
                            0x0a if decoded.width == 1
                                && decoded.literal.is_none()
                                && renamed.reads.is_empty() =>
                            {
                                let exact = calls
                                    .calls
                                    .iter()
                                    .filter(|call| {
                                        call.result.as_ref().is_some_and(|result| {
                                            result.descriptor.as_ref() == "Z"
                                                && result.words.as_slice() == [id]
                                        })
                                    })
                                    .take(2)
                                    .count()
                                    == 1;
                                BooleanNode::Value(if exact {
                                    BooleanDomain::Both
                                } else {
                                    BooleanDomain::Unknown
                                })
                            }
                            0x96 | 0xb6
                                if decoded.width == if decoded.opcode == 0x96 { 2 } else { 1 }
                                    && decoded.literal.is_none()
                                    && decoded.reads.len() == 2
                                    && renamed.reads.len() == 2
                                    && renamed.reads.iter().all(|read| read.words.len() == 1) =>
                            {
                                saw_or = true;
                                BooleanNode::Or(
                                    renamed.reads[0].words[0],
                                    renamed.reads[1].words[0],
                                )
                            }
                            _ => BooleanNode::Value(BooleanDomain::Unknown),
                        }
                    }
                } else {
                    BooleanNode::Value(BooleanDomain::Unknown)
                }
            }
        };
        let dependencies = node.dependencies();
        if dependencies
            .iter()
            .any(|dependency| *dependency >= ssa.definitions.len())
        {
            return false;
        }
        nodes[id] = Some(node);
        pending.extend(dependencies);
    }
    if !saw_or {
        return false;
    }

    let mut users = vec![Vec::new(); ssa.definitions.len()];
    let mut queue = VecDeque::new();
    let mut queued = vec![false; ssa.definitions.len()];
    for (id, node) in nodes.iter().enumerate() {
        if let Some(node) = node {
            for dependency in node.dependencies() {
                users[dependency].push(id);
            }
            queue.push_back(id);
            queued[id] = true;
        }
    }
    let mut values = vec![BooleanDomain::Bottom; ssa.definitions.len()];
    let mut work = 0usize;
    while let Some(id) = queue.pop_front() {
        queued[id] = false;
        work += 1;
        if work > 2_000_000 {
            return false;
        }
        let next = values[id].join(nodes[id].as_ref().unwrap().transfer(&values));
        if next != values[id] {
            values[id] = next;
            for &user in &users[id] {
                if !queued[user] {
                    queued[user] = true;
                    queue.push_back(user);
                }
            }
        }
    }
    values[root].proven()
        && nodes
            .iter()
            .enumerate()
            .all(|(id, node)| node.is_none() || values[id] != BooleanDomain::Bottom)
}
fn boolean_bitwise_expression(
    op: u8,
    a: usize,
    words: &[u16],
    pc: usize,
    regs: &[Option<Value>],
    decoded: Option<&(usize, [usize; 2], Option<i64>)>,
) -> Result<Option<(usize, String)>> {
    let kind = match op {
        0x90..=0x9a => op - 0x90,
        0xb0..=0xba => op - 0xb0,
        0xd0..=0xd7 => op - 0xd0,
        _ => op - 0xd8,
    };
    if !matches!(kind, 5..=7) {
        return Ok(None);
    }
    let literal = |n: i32| Value {
        text: n.to_string(),
        ty: "I".into(),
        literal: Some(n),
        wide_literal: None,
        raw_bits32: false,
    };
    let (dst, left, right) = if let Some((dst, sources, immediate)) = decoded {
        (
            *dst,
            register(regs, sources[0])?,
            if let Some(n) = immediate {
                literal(*n as i32)
            } else {
                register(regs, sources[1])?
            },
        )
    } else if op <= 0x9a {
        (
            a,
            register(regs, (words[pc + 1] & 255) as usize)?,
            register(regs, (words[pc + 1] >> 8) as usize)?,
        )
    } else if op <= 0xba {
        (a & 15, register(regs, a & 15)?, register(regs, a >> 4)?)
    } else if op <= 0xd7 {
        (
            a & 15,
            register(regs, a >> 4)?,
            literal(words[pc + 1] as i16 as i32),
        )
    } else {
        (
            a,
            register(regs, (words[pc + 1] & 255) as usize)?,
            literal((words[pc + 1] >> 8) as i8 as i32),
        )
    };
    let boolean = |v: &Value| v.ty == "Z" || matches!(v.literal, Some(0 | 1));
    if kind == 7 {
        for (source, constant) in [(&left, &right), (&right, &left)] {
            if source.ty == "Z"
                && let Some(n @ (0 | 1)) = constant.literal
            {
                return Ok(Some((
                    dst,
                    if n == 0 {
                        source.text.clone()
                    } else {
                        format!("!({})", source.text)
                    },
                )));
            }
        }
    }
    if boolean(&left) && boolean(&right) {
        return Ok(Some((
            dst,
            format!(
                "{} {} {}",
                argument(&left, "Z")?,
                opname(kind)?,
                argument(&right, "Z")?
            ),
        )));
    }
    if kind == 5 && (boolean(&left) || boolean(&right)) {
        // AND with a proven 0/1 operand is itself exactly 0/1, even when
        // the other operand is a signed integer. Keep numeric uses explicit.
        return Ok(Some((
            dst,
            format!("({} & ({})) != 0", integral(&left)?, integral(&right)?),
        )));
    }
    Ok(None)
}
fn receiver(value: &Value, ty: &str) -> Result<String> {
    ensure!(reference(ty), "nonreference receiver owner");
    if value.literal == Some(0) {
        return Ok(format!("(({}) null)", java_type(ty)?));
    }
    if ty == "Ljava/lang/Object;" && reference(&value.ty) {
        return Ok(value.text.clone());
    }
    argument(value, ty)
}

fn wide(ty: &str) -> bool {
    matches!(ty, "J" | "D")
}
// Eligible straight-line methods consume the shared decoder's operands. These
// checks are invariants, not a reason to retry raw operand decoding.
fn decoded_register(
    operand: &crate::native_ir::RegisterOperand,
    kind: crate::native_ir::ValueKind,
    registers: usize,
) -> Result<usize> {
    let register = usize::from(operand.register);
    ensure!(
        operand.kind == kind && register + kind.word_count() <= registers,
        "invalid shared register operand"
    );
    Ok(register)
}

fn decoded_arithmetic(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<(usize, [usize; 2], Option<i64>)> {
    use crate::native_ir::ValueKind::{Bits32, Wide64};
    let kind = |numeric: numeric::Kind| if numeric.width() == 2 { Wide64 } else { Bits32 };
    let op = instruction.opcode;
    let (reads, write, width, literal, may_throw) = match op {
        0x2d..=0x31 => {
            let spec = numeric::Compare::decode(op).context("shared comparison opcode")?;
            ([kind(spec.input); 2], Bits32, 2, false, false)
        }
        0x7b..=0x8f => {
            let spec = numeric::Unary::decode(op).context("shared unary opcode")?;
            (
                [kind(spec.input), Bits32],
                kind(spec.result),
                1,
                false,
                false,
            )
        }
        0x90..=0xcf => {
            let spec = numeric::Binary::decode(op).context("shared binary opcode")?;
            let basic = if op >= 0xb0 { op - 0x20 } else { op };
            (
                [kind(spec.left), kind(spec.right)],
                kind(spec.result),
                if op >= 0xb0 { 1 } else { 2 },
                false,
                matches!(basic, 0x93 | 0x94 | 0x9e | 0x9f),
            )
        }
        0xd0..=0xe2 => (
            [Bits32; 2],
            Bits32,
            2,
            true,
            matches!(op, 0xd3 | 0xd4 | 0xdb | 0xdc),
        ),
        _ => bail!("invalid shared arithmetic opcode"),
    };
    ensure!(
        instruction.width == width
            && instruction.may_throw == may_throw
            && instruction.reference.is_none()
            && instruction.prototype.is_none()
            && instruction.branch_target.is_none()
            && instruction.payload_target.is_none(),
        "invalid shared arithmetic metadata"
    );
    let read_count = if matches!(op, 0x7b..=0x8f | 0xd0..=0xe2) {
        1
    } else {
        2
    };
    ensure!(
        instruction.reads.len() == read_count && instruction.writes.len() == 1,
        "invalid shared arithmetic operand count"
    );
    ensure!(
        instruction.literal.is_some() == literal,
        "invalid shared arithmetic literal"
    );
    if let Some(value) = instruction.literal {
        ensure!(
            if op <= 0xd7 {
                i16::try_from(value).is_ok()
            } else {
                i8::try_from(value).is_ok()
            },
            "shared arithmetic literal exceeds encoding"
        );
    }
    let dst = decoded_register(&instruction.writes[0], write, registers)?;
    let mut sources = [0; 2];
    for (index, operand) in instruction.reads.iter().enumerate() {
        sources[index] = decoded_register(operand, reads[index], registers)?;
    }
    ensure!(
        !(0xb0..=0xcf).contains(&op) || sources[0] == dst,
        "shared 2addr destination differs from left operand"
    );
    Ok((dst, sources, instruction.literal))
}

fn decoded_field(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<(usize, Option<usize>, usize)> {
    use crate::native_ir::{PoolKind, ValueKind};
    let op = instruction.opcode;
    ensure!((0x52..=0x6d).contains(&op), "invalid shared field opcode");
    let is_static = op >= 0x60;
    let put = if is_static { op >= 0x67 } else { op >= 0x59 };
    let family = if op >= 0x67 {
        op - 0x67
    } else if is_static {
        op - 0x60
    } else if put {
        op - 0x59
    } else {
        op - 0x52
    };
    let kind = match family {
        1 => ValueKind::Wide64,
        2 => ValueKind::Reference,
        _ => ValueKind::Bits32,
    };
    ensure!(
        instruction.width == 2
            && instruction.may_throw
            && instruction.literal.is_none()
            && instruction.prototype.is_none()
            && instruction.branch_target.is_none()
            && instruction.payload_target.is_none(),
        "invalid shared field metadata"
    );
    let reference = instruction
        .reference
        .context("missing shared field reference")?;
    ensure!(
        reference.kind == PoolKind::Field && u16::try_from(reference.index).is_ok(),
        "invalid shared field reference"
    );
    let receiver_count = usize::from(!is_static);
    ensure!(
        instruction.reads.len() == receiver_count + usize::from(put)
            && instruction.writes.len() == usize::from(!put),
        "invalid shared field operand count"
    );
    let receiver = if is_static {
        None
    } else {
        Some(decoded_register(
            &instruction.reads[0],
            ValueKind::Reference,
            registers,
        )?)
    };
    let value = if put {
        &instruction.reads[receiver_count]
    } else {
        &instruction.writes[0]
    };
    Ok((
        decoded_register(value, kind, registers)?,
        receiver,
        reference.index as usize,
    ))
}

fn decoded_array_type(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<operations::Operands> {
    use crate::native_ir::{
        PoolKind,
        ValueKind::{Bits32, Reference, Wide64},
    };
    let op = instruction.opcode;
    let (read_kinds, read_count, write_kind, has_type) = match op {
        0x1f => ([Reference, Bits32, Bits32], 1, Some(Reference), true),
        0x20 => ([Reference, Bits32, Bits32], 1, Some(Bits32), true),
        0x21 => ([Reference, Bits32, Bits32], 1, Some(Bits32), false),
        0x23 => ([Bits32; 3], 1, Some(Reference), true),
        0x44..=0x51 => {
            let kind = match op {
                0x45 | 0x4c => Wide64,
                0x46 | 0x4d => Reference,
                _ => Bits32,
            };
            (
                [Reference, Bits32, kind],
                if op >= 0x4b { 3 } else { 2 },
                if op >= 0x4b { None } else { Some(kind) },
                false,
            )
        }
        _ => bail!("invalid shared array/type opcode"),
    };
    ensure!(
        instruction.width == if op == 0x21 { 1 } else { 2 }
            && instruction.may_throw
            && instruction.literal.is_none()
            && instruction.prototype.is_none()
            && instruction.branch_target.is_none()
            && instruction.payload_target.is_none(),
        "invalid shared array/type metadata"
    );
    ensure!(
        instruction.reads.len() == read_count
            && instruction.writes.len() == usize::from(write_kind.is_some()),
        "invalid shared array/type operand count"
    );
    let type_index = if has_type {
        let reference = instruction
            .reference
            .context("missing shared array/type reference")?;
        ensure!(
            reference.kind == PoolKind::Type && u16::try_from(reference.index).is_ok(),
            "invalid shared array/type reference"
        );
        Some(reference.index as usize)
    } else {
        ensure!(
            instruction.reference.is_none(),
            "unexpected shared array/type reference"
        );
        None
    };
    let mut reads = [0; 3];
    for (index, operand) in instruction.reads.iter().enumerate() {
        reads[index] = decoded_register(operand, read_kinds[index], registers)?;
    }
    let dst = if let Some(kind) = write_kind {
        decoded_register(&instruction.writes[0], kind, registers)?
    } else {
        0
    };
    ensure!(
        op != 0x1f || dst == reads[0],
        "shared check-cast destination differs from source"
    );
    Ok(operations::Operands {
        dst,
        reads,
        read_count,
        type_index,
    })
}

fn decoded_throw_source(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<usize> {
    use crate::native_ir::ValueKind;
    let ([read], []) = (instruction.reads.as_slice(), instruction.writes.as_slice()) else {
        bail!("invalid shared throw operand count");
    };
    ensure!(
        instruction.opcode == 0x27
            && instruction.width == 1
            && instruction.may_throw
            && instruction.literal.is_none()
            && instruction.reference.is_none()
            && instruction.prototype.is_none()
            && instruction.branch_target.is_none()
            && instruction.payload_target.is_none(),
        "invalid shared throw metadata"
    );
    decoded_register(read, ValueKind::Reference, registers)
}

fn decoded_move(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<(usize, usize)> {
    use crate::native_ir::ValueKind;
    let kind = match instruction.opcode {
        0x01..=0x03 => ValueKind::Bits32,
        0x04..=0x06 => ValueKind::Wide64,
        0x07..=0x09 => ValueKind::Reference,
        _ => bail!("invalid shared move opcode"),
    };
    let ([read], [write]) = (instruction.reads.as_slice(), instruction.writes.as_slice()) else {
        bail!("invalid shared move operand count");
    };
    ensure!(instruction.literal.is_none(), "literal on shared move");
    Ok((
        decoded_register(write, kind, registers)?,
        decoded_register(read, kind, registers)?,
    ))
}

fn decoded_result_destination(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<usize> {
    use crate::native_ir::ValueKind;
    let kind = match instruction.opcode {
        0x0a => ValueKind::Unknown32,
        0x0b => ValueKind::Wide64,
        0x0c => ValueKind::Reference,
        _ => bail!("invalid shared move-result opcode"),
    };
    let ([], [write]) = (instruction.reads.as_slice(), instruction.writes.as_slice()) else {
        bail!("invalid shared move-result operand count");
    };
    ensure!(
        instruction.width == 1
            && !instruction.may_throw
            && instruction.literal.is_none()
            && instruction.reference.is_none()
            && instruction.prototype.is_none()
            && instruction.branch_target.is_none()
            && instruction.payload_target.is_none(),
        "invalid shared move-result metadata"
    );
    decoded_register(write, kind, registers)
}

fn decoded_return_source(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<Option<usize>> {
    use crate::native_ir::ValueKind;
    let kind = match instruction.opcode {
        0x0e => None,
        0x0f => Some(ValueKind::Bits32),
        0x10 => Some(ValueKind::Wide64),
        0x11 => Some(ValueKind::Reference),
        _ => bail!("invalid shared return opcode"),
    };
    ensure!(
        instruction.width == 1
            && !instruction.may_throw
            && instruction.literal.is_none()
            && instruction.reference.is_none()
            && instruction.prototype.is_none()
            && instruction.branch_target.is_none()
            && instruction.payload_target.is_none(),
        "invalid shared return metadata"
    );
    match kind {
        None => {
            ensure!(
                instruction.reads.is_empty() && instruction.writes.is_empty(),
                "invalid shared void return operand count"
            );
            Ok(None)
        }
        Some(kind) => {
            let ([read], []) = (instruction.reads.as_slice(), instruction.writes.as_slice()) else {
                bail!("invalid shared return operand count");
            };
            Ok(Some(decoded_register(read, kind, registers)?))
        }
    }
}

fn decoded_constant(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<(usize, i64)> {
    use crate::native_ir::ValueKind;
    let kind = match instruction.opcode {
        0x12..=0x15 => ValueKind::Bits32,
        0x16..=0x19 => ValueKind::Wide64,
        _ => bail!("invalid shared constant opcode"),
    };
    let ([], [write]) = (instruction.reads.as_slice(), instruction.writes.as_slice()) else {
        bail!("invalid shared constant operand count");
    };
    let literal = instruction
        .literal
        .context("missing shared constant literal")?;
    ensure!(
        kind == ValueKind::Wide64 || i32::try_from(literal).is_ok(),
        "shared constant exceeds 32 bits"
    );
    Ok((decoded_register(write, kind, registers)?, literal))
}

fn decoded_reference_constant(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<(usize, usize)> {
    use crate::native_ir::{PoolKind, ValueKind};
    let pool_kind = match instruction.opcode {
        0x1a | 0x1b => PoolKind::String,
        0x1c => PoolKind::Type,
        _ => bail!("invalid shared reference constant opcode"),
    };
    let ([], [write]) = (instruction.reads.as_slice(), instruction.writes.as_slice()) else {
        bail!("invalid shared reference constant operand count");
    };
    ensure!(
        instruction.literal.is_none(),
        "literal on shared reference constant"
    );
    ensure!(
        instruction.may_throw,
        "shared reference constant lost class/string resolution effect"
    );
    let reference = instruction
        .reference
        .context("missing shared reference constant pool reference")?;
    ensure!(
        reference.kind == pool_kind,
        "invalid shared reference constant pool kind"
    );
    ensure!(
        instruction.opcode == 0x1b || u16::try_from(reference.index).is_ok(),
        "shared reference constant index exceeds 16 bits"
    );
    Ok((
        decoded_register(write, ValueKind::Reference, registers)?,
        usize::try_from(reference.index).context("shared reference constant index overflow")?,
    ))
}

fn register(registers: &[Option<Value>], r: usize) -> Result<Value> {
    let value = registers
        .get(r)
        .and_then(Clone::clone)
        .context("undefined register")?;
    ensure!(
        value.ty != "<wide-tail>",
        "read from upper half of wide register"
    );
    if wide(&value.ty) {
        ensure!(
            registers
                .get(r + 1)
                .and_then(Option::as_ref)
                .is_some_and(|tail| tail.ty == "<wide-tail>" && tail.text == r.to_string()),
            "invalid wide register pair"
        );
    }
    Ok(value)
}
fn assign(registers: &mut [Option<Value>], r: usize, value: Value) -> Result<()> {
    let width = if wide(&value.ty) {
        Kind::Long.width()
    } else {
        Kind::Int.width()
    };
    ensure!(
        r.checked_add(width)
            .is_some_and(|end| end <= registers.len()),
        "register pair out of bounds"
    );
    for slot in r..r + width {
        if let Some(old) = &registers[slot] {
            if old.ty == "<wide-tail>" {
                let head: usize = old.text.parse().context("invalid wide tail marker")?;
                ensure!(head + 1 == slot, "wide tail owner mismatch");
                registers[head] = None;
            } else if wide(&old.ty) && slot + 1 < registers.len() {
                registers[slot + 1] = None;
            }
        }
        registers[slot] = None;
    }
    registers[r] = Some(value);
    if width == 2 {
        registers[r + 1] = Some(Value {
            text: r.to_string(),
            ty: "<wide-tail>".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        });
    }
    Ok(())
}

// A frame stores the upper word only as ownership metadata.  Keep checking it
// at control-flow boundaries: joins construct frames directly, whereas normal
// instruction writes go through `assign`.
fn validate_wide_frame(registers: &[Option<Value>]) -> Result<()> {
    for (r, value) in registers.iter().enumerate() {
        let Some(value) = value else { continue };
        if wide(&value.ty) {
            ensure!(
                registers
                    .get(r + 1)
                    .and_then(Option::as_ref)
                    .is_some_and(|tail| tail.ty == "<wide-tail>" && tail.text == r.to_string()),
                "invalid wide register pair"
            );
        } else if value.ty == "<wide-tail>" {
            let head: usize = value.text.parse().context("invalid wide tail marker")?;
            ensure!(
                head + 1 == r
                    && registers
                        .get(head)
                        .and_then(Option::as_ref)
                        .is_some_and(|head| wide(&head.ty)),
                "wide tail owner mismatch"
            );
        }
    }
    Ok(())
}

fn loop_slots(
    regs: &[Option<Value>],
    graph: &Graph,
    start: usize,
    out: &mut Output,
    written: Option<&[bool]>,
    header_needed: Option<&[bool]>,
) -> Result<Vec<Option<Value>>> {
    validate_wide_frame(regs)?;
    let mut slots = vec![None; regs.len()];
    for (r, value) in regs.iter().enumerate() {
        let Some(value) = value else { continue };
        if value.ty == "<wide-tail>" {
            continue;
        }
        let live = graph.live_at(start, r) || (wide(&value.ty) && graph.live_at(start, r + 1));
        if live {
            if header_needed.is_some_and(|needed| !needed[r]) {
                // Still expose the entry value to a zero-iteration exit, but
                // do not allocate a Java variable for an unrelated lifetime.
                assign(&mut slots, r, value.clone())?;
                continue;
            }
            if (value.text == "this" || value.literal.is_some() || value.wide_literal.is_some())
                && written.is_some_and(|writes| !writes[r])
            {
                assign(&mut slots, r, value.clone())?;
            } else {
                assign(&mut slots, r, out.local(&value.ty, &value.text, &[])?)?;
            }
        }
    }
    Ok(slots)
}
fn integral(value: &Value) -> Result<String> {
    if value.ty == "Z" {
        return argument(value, "I");
    }
    ensure!(
        matches!(value.ty.as_str(), "I" | "B" | "S" | "C"),
        "nonintegral arithmetic"
    );
    Ok(value.text.clone())
}

// A DEX literal has no primitive/reference type until its uses constrain it.
// At a loop header, the Java slot must already have the carried type before
// rendering the body. Consume only a fully resolved, untruncated SSA phi for
// that physical register; all incoming definitions and uses must agree.
struct LoopLifetimeProof {
    header_needed: Vec<bool>,
    exit_null_types: Vec<Option<String>>,
}

fn promote_loop_entry_literals(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    region: Loop,
    regs: &mut [Option<Value>],
    written: Option<&[bool]>,
) -> Result<Option<LoopLifetimeProof>> {
    let Some(written) = written else {
        return Ok(None);
    };
    let Some(code) = method.code.as_ref() else {
        return Ok(None);
    };
    if code.instructions.len() > 512
        || graph.loops.len() > 4
        || !graph
            .loops
            .iter()
            .any(|loop_region| loop_region.start == region.start)
        || !(0..regs.len()).any(|r| {
            written.get(r) == Some(&true)
                && graph.live_at(region.start, r)
                && regs[r]
                    .as_ref()
                    .is_some_and(|value| value.ty == "I" && value.literal.is_some())
        })
    {
        return Ok(None);
    }
    if let (Some(front), Some(cfg)) = (&graph.front_end, &graph.shared_cfg) {
        let Ok(ssa) =
            crate::native_ssa::SsaMethod::build_with_work_limit(code, &front.ir, cfg, 2_000_000)
        else {
            return Ok(None);
        };
        let Ok(calls) = crate::native_call_values::SsaCalls::bind(&front.bound, &ssa) else {
            return Ok(None);
        };
        let Ok(types) = crate::native_types::InferredTypes::infer_with_work_limit(
            method,
            &front.ir,
            &ssa,
            &calls,
            &class.symbols,
            2_000_000,
        ) else {
            return Ok(None);
        };
        if types.wide_pair_issues != 0 {
            return Ok(None);
        }
        apply_loop_literal_types(regs, written, graph, region.start, &ssa, &types)?;
        return Ok(Some(loop_lifetime_proof(
            method, graph, region, regs, written, &ssa, &types,
        )));
    }
    // Legacy region selection has no selected decoded IR. Analyze its exact
    // immutable method once; malformed or unresolved SSA retains the fallback.
    let Ok(analysis) = crate::native_method::MethodAnalysis::build(class, method) else {
        return Ok(None);
    };
    let Ok(types) = analysis.infer_types() else {
        return Ok(None);
    };
    if types.wide_pair_issues != 0 {
        return Ok(None);
    }
    apply_loop_literal_types(regs, written, graph, region.start, analysis.ssa(), &types)?;
    Ok(Some(loop_lifetime_proof(
        method,
        graph,
        region,
        regs,
        written,
        analysis.ssa(),
        &types,
    )))
}

// Physical liveness includes uses after the loop. A zero literal that is used
// only at the exit must remain available on the guard path, but it is not a
// carried Java local. SSA reads prove that no iteration observes its old
// lifetime before a new definition; the exit phi proves the null/reference
// lifetime separately. Other lifetimes retain the ordinary conservative slot.
fn loop_lifetime_proof(
    method: &DexMethod,
    graph: &Graph,
    region: Loop,
    regs: &[Option<Value>],
    written: &[bool],
    ssa: &crate::native_ssa::SsaMethod,
    types: &crate::native_types::InferredTypes,
) -> LoopLifetimeProof {
    use crate::native_ssa::DefinitionKind;
    use crate::native_types::{AssignmentBound, TypeResolution};

    let mut proof = LoopLifetimeProof {
        header_needed: vec![true; regs.len()],
        exit_null_types: vec![None; regs.len()],
    };
    let Some(code) = method.code.as_ref() else {
        return proof;
    };
    if region.guard.is_none()
        || region.tail.is_some()
        || region.exit >= code.instructions.len()
        || method.code.as_ref().is_some_and(|code| {
            code.try_regions.iter().any(|protected| {
                (protected.start as usize) < region.exit && region.start < protected.end as usize
            })
        })
    {
        return proof;
    }
    let end = region.body_end(&graph.widths).min(region.exit);
    let mut reads_entry = vec![false; regs.len()];
    for instruction in &ssa.instructions {
        if !(region.start..end).contains(&instruction.pc)
            || !ssa
                .graph
                .block_at
                .get(&instruction.pc)
                .is_some_and(|block| ssa.reachable[*block])
        {
            continue;
        }
        for read in &instruction.reads {
            for (word, id) in read.words.iter().enumerate() {
                let r = usize::from(read.register) + word;
                if r >= regs.len() {
                    return proof;
                }
                if !matches!(
                    ssa.definitions[*id].kind,
                    DefinitionKind::Instruction { pc, .. } if (region.start..end).contains(&pc)
                ) {
                    reads_entry[r] = true;
                }
            }
        }
    }
    for r in 0..regs.len() {
        let Some(value) = &regs[r] else { continue };
        if value.ty != "I"
            || value.literal != Some(0)
            || written.get(r) != Some(&true)
            || reads_entry[r]
            || !graph.live_at(region.start, r)
            || !graph.live_at(region.exit, r)
            || (r > 0 && regs[r - 1].as_ref().is_some_and(|v| wide(&v.ty)))
            || regs[r].as_ref().is_some_and(|v| wide(&v.ty))
        {
            continue;
        }
        let mut phis = ssa.phis.iter().filter(|phi| {
            usize::from(phi.register) == r && ssa.graph.blocks[phi.block].start == region.exit
        });
        let Some(phi) = phis.next() else { continue };
        if phis.next().is_some() || phi.incoming.is_empty() {
            continue;
        }
        let inferred = &types.values[phi.result];
        let TypeResolution::Resolved(ty) = &inferred.resolution else {
            continue;
        };
        let descriptor = ty.as_ref();
        if !reference(descriptor)
            || inferred.bounds_truncated
            || !inferred.assignment.contains(&AssignmentBound::Literal {
                bits: 0,
                wide: false,
            })
            || !inferred
                .assignment
                .contains(&AssignmentBound::Type(ty.clone()))
            || !inferred.assignment.iter().all(|bound| match bound {
                AssignmentBound::Literal { bits, wide } => *bits == 0 && !wide,
                AssignmentBound::Type(found) => found == ty,
                _ => false,
            })
            || !inferred
                .required_types
                .iter()
                .all(|required| required == ty || required.as_ref() == "Ljava/lang/Object;")
            || !phi.incoming.iter().all(|(_, id)| {
                let incoming = &types.values[*id];
                !incoming.bounds_truncated
                    && (incoming.assignment.as_slice()
                        == [AssignmentBound::Literal {
                            bits: 0,
                            wide: false,
                        }]
                        || (incoming.resolution == inferred.resolution
                            && incoming.assignment.as_slice()
                                == [AssignmentBound::Type(ty.clone())]))
            })
        {
            continue;
        }
        proof.header_needed[r] = false;
        proof.exit_null_types[r] = Some(descriptor.into());
    }
    proof
}

fn apply_loop_literal_types(
    regs: &mut [Option<Value>],
    written: &[bool],
    graph: &Graph,
    start: usize,
    ssa: &crate::native_ssa::SsaMethod,
    types: &crate::native_types::InferredTypes,
) -> Result<()> {
    use crate::native_types::{AssignmentBound, TypeResolution};

    for (r, slot) in regs.iter_mut().enumerate() {
        let Some(value) = slot else { continue };
        let Some(bits) = value.literal else { continue };
        if value.ty != "I" || written.get(r) != Some(&true) || !graph.live_at(start, r) {
            continue;
        }
        let mut phis = ssa.phis.iter().filter(|phi| {
            usize::from(phi.register) == r && ssa.graph.blocks[phi.block].start == start
        });
        let Some(phi) = phis.next() else { continue };
        if phis.next().is_some() {
            continue;
        }
        let inferred = &types.values[phi.result];
        let TypeResolution::Resolved(ty) = &inferred.resolution else {
            continue;
        };
        let descriptor = ty.as_ref();
        if !(matches!(descriptor, "Z" | "F") || reference(descriptor) && bits == 0) {
            continue;
        }
        if descriptor == "Z" && !matches!(bits, 0 | 1) {
            continue;
        }
        if inferred.bounds_truncated
            || !inferred.assignment.contains(&AssignmentBound::Literal {
                bits: i64::from(bits),
                wide: false,
            })
            || !inferred
                .assignment
                .contains(&AssignmentBound::Type(ty.clone()))
            || !inferred.assignment.iter().all(|bound| match bound {
                AssignmentBound::Literal { bits, wide } => {
                    !wide
                        && if descriptor == "Z" {
                            matches!(bits, 0 | 1)
                        } else {
                            !reference(descriptor) || *bits == 0
                        }
                }
                AssignmentBound::Type(found) => found == ty,
                _ => false,
            })
            || !inferred.required_types.iter().all(|required| {
                required == ty
                    || (reference(descriptor) && required.as_ref() == "Ljava/lang/Object;")
            })
            || phi.incoming.is_empty()
            || !phi.incoming.iter().all(|(_, id)| {
                let incoming = &types.values[*id];
                !incoming.bounds_truncated
                    && incoming.resolution == inferred.resolution
                    && incoming.assignment.iter().all(|bound| {
                        !matches!(bound, AssignmentBound::Unknown | AssignmentBound::Undefined)
                    })
            })
        {
            continue;
        }
        let text = match descriptor {
            "Z" => (bits != 0).to_string(),
            "F" => Literal::Bits32(bits as u32).render(Kind::Float)?,
            _ => "null".into(),
        };
        *value = Value {
            text,
            ty: descriptor.into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
    }
    Ok(())
}
fn opname(op: u8) -> Result<&'static str> {
    Ok(match op {
        0 => "+",
        1 => "-",
        2 => "*",
        3 => "/",
        4 => "%",
        5 => "&",
        6 => "|",
        7 => "^",
        8 => "<<",
        9 => ">>",
        10 => ">>>",
        _ => bail!("invalid arithmetic"),
    })
}
fn string_literal(value: &str) -> Result<String> {
    super::strings::literal(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Loop {
    // Canonical enclosing header; legacy regions leave this unset.
    parent: Option<usize>,
    start: usize,
    latch: usize,
    guard: Option<usize>,
    exit: usize,
    // Conditional backedge with a forward guard and a one-time exit tail.
    tail: Option<usize>,
}
impl Loop {
    fn body_end(&self, widths: &[usize]) -> usize {
        if self.tail.is_some() {
            self.exit
        } else {
            self.latch + widths[self.latch]
        }
    }
}
#[derive(Clone, Copy)]
struct LoopContext<'a> {
    start: usize,
    slots: &'a [Option<Value>],
    exit: usize,
    exit_slots: &'a [Option<Value>],
}
#[derive(Clone)]
struct Switch {
    cases: Vec<(i32, usize)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SharedBranch {
    owner: Option<usize>,
    branch: usize,
    taken: usize,
    fallthrough: usize,
    join: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SharedLoopEdgeKind {
    Break,
    Continue,
    Terminal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SharedLoopEdge {
    owner: usize,
    branch: usize,
    taken: usize,
    fallthrough: usize,
    kind: SharedLoopEdgeKind,
}
const MAX_SHARED_LOOPS: usize = 32;
const MAX_SHARED_LOOP_DEPTH: usize = 4;
fn shared_loop_owner(regions: &[Loop], pc: usize) -> Option<Loop> {
    let mut index = regions
        .partition_point(|region| region.start <= pc)
        .checked_sub(1)?;
    for _ in 0..MAX_SHARED_LOOP_DEPTH {
        let region = regions[index];
        if pc < region.exit {
            return Some(region);
        }
        let parent = region.parent?;
        index = regions
            .binary_search_by_key(&parent, |region| region.start)
            .ok()?;
    }
    None
}
fn assign_shared_loop_parents(regions: &mut [Loop]) -> Result<()> {
    let mut ancestors: Vec<usize> = Vec::new();
    for index in 0..regions.len() {
        while ancestors
            .last()
            .is_some_and(|&parent| regions[parent].exit <= regions[index].start)
        {
            ancestors.pop();
        }
        regions[index].parent = if let Some(&parent) = ancestors.last() {
            ensure!(
                regions[index].start
                    >= regions[parent]
                        .guard
                        .context("missing shared parent guard")?
                        + 2
                    && regions[index].exit <= regions[parent].latch,
                "intersecting shared loop regions"
            );
            Some(regions[parent].start)
        } else {
            None
        };
        ensure!(
            index == 0 || regions[index - 1].start < regions[index].start,
            "duplicate shared loop header"
        );
        ancestors.push(index);
        ensure!(
            ancestors.len() <= MAX_SHARED_LOOP_DEPTH,
            "shared loop nesting budget"
        );
    }
    Ok(())
}
fn shared_loop_depth(regions: &[Loop], mut region: Loop) -> Result<usize> {
    let mut depth = 1;
    while let Some(parent) = region.parent {
        let index = regions
            .binary_search_by_key(&parent, |region| region.start)
            .map_err(|_| anyhow::anyhow!("missing shared loop parent"))?;
        ensure!(
            regions[index].start < region.start,
            "cyclic shared loop parent"
        );
        region = regions[index];
        depth += 1;
        ensure!(depth <= MAX_SHARED_LOOP_DEPTH, "shared loop nesting budget");
    }
    Ok(depth)
}
fn shared_loop_target_owned(regions: &[Loop], owner: Loop, target: usize) -> bool {
    shared_loop_owner(regions, target).is_some_and(|region| {
        region.start == owner.start
            || (target == region.start && region.parent == Some(owner.start))
    })
}
#[cfg(test)]
fn decoded_loop_edges(
    ir: &crate::native_ir::DecodedMethod,
    region: Loop,
) -> Result<Vec<SharedLoopEdge>> {
    decoded_all_loop_edges(ir, &[region])
}
fn decoded_all_loop_edges(
    ir: &crate::native_ir::DecodedMethod,
    regions: &[Loop],
) -> Result<Vec<SharedLoopEdge>> {
    ensure!(
        ir.instructions
            .iter()
            .filter(|i| matches!(i.opcode,0x28..=0x2a|0x32..=0x3d))
            .count()
            <= 1024,
        "shared loop control budget"
    );
    let mut edges = Vec::new();
    for instruction in &ir.instructions {
        let Some(region) = shared_loop_owner(regions, instruction.pc) else {
            continue;
        };
        if Some(instruction.pc) == region.guard || instruction.pc == region.latch {
            continue;
        }
        let Some(target) = instruction.branch_target else {
            continue;
        };
        let kind = if target == region.start {
            SharedLoopEdgeKind::Continue
        } else if target == region.exit {
            SharedLoopEdgeKind::Break
        } else if target == region.latch + 2
            && (region.tail == Some(target)
                || (region.guard.is_some()
                    && region.exit == region.latch + 4
                    && ir
                        .instructions
                        .iter()
                        .any(|i| i.pc == region.latch && matches!(i.opcode, 0x32..=0x3d))))
        {
            SharedLoopEdgeKind::Terminal
        } else {
            continue;
        };
        ensure!(
            matches!(instruction.opcode, 0x32..=0x3d)
                && instruction.width == 2
                && instruction.pc + instruction.width <= region.latch,
            "invalid shared loop special edge"
        );
        ensure!(
            edges
                .last()
                .is_none_or(|edge: &SharedLoopEdge| edge.branch < instruction.pc),
            "unordered shared loop edges"
        );
        edges.push(SharedLoopEdge {
            owner: region.start,
            branch: instruction.pc,
            taken: target,
            fallthrough: instruction.pc + instruction.width,
            kind,
        });
        ensure!(edges.len() <= 1024, "shared loop edge budget");
    }
    Ok(edges)
}
fn shared_loop_edge_at(edges: &[SharedLoopEdge], pc: usize) -> Option<SharedLoopEdge> {
    edges
        .binary_search_by_key(&pc, |edge| edge.branch)
        .ok()
        .map(|index| edges[index])
}
fn decoded_branch_plans(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
    len: usize,
) -> Result<Vec<SharedBranch>> {
    decoded_branch_plans_in(ir, cfg, len, 0..len, &[], &[])
}
fn decoded_branch_plans_in(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
    len: usize,
    region: std::ops::Range<usize>,
    excluded: &[SharedLoopEdge],
    atomic_loops: &[Loop],
) -> Result<Vec<SharedBranch>> {
    let postdom = cfg.forward_postdominators()?;
    let mut work = 16_000_000usize;
    decoded_branch_plans_with_postdom(
        ir,
        cfg,
        len,
        region,
        excluded,
        atomic_loops,
        &postdom,
        &mut work,
    )
}
#[allow(clippy::too_many_arguments)]
fn decoded_branch_plans_with_postdom(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
    len: usize,
    region: std::ops::Range<usize>,
    excluded: &[SharedLoopEdge],
    atomic_loops: &[Loop],
    postdom: &[usize],
    work: &mut usize,
) -> Result<Vec<SharedBranch>> {
    let first = ir.instructions.partition_point(|i| i.pc < region.start);
    let end = ir.instructions.partition_point(|i| i.pc < region.end);
    let instructions = &ir.instructions[first..end];
    let single_condition = instructions
        .iter()
        .filter(|instruction| {
            matches!(instruction.opcode, 0x32..=0x3d)
                && region.contains(&instruction.pc)
                && shared_loop_owner(atomic_loops, instruction.pc).is_none()
                && excluded
                    .binary_search_by_key(&instruction.pc, |edge| edge.branch)
                    .is_err()
        })
        .count()
        == 1;
    let mut plans = Vec::new();
    let mut seen = vec![false; cfg.blocks.len()];
    let mut escape = |start: usize, stop: usize| -> Result<Option<usize>> {
        seen.fill(false);
        if start == stop {
            return Ok(None);
        }
        ensure!(start < stop, "shared branch region starts beyond join");
        seen[start] = true;
        let mut escaped = None;
        for index in start..stop {
            ensure!(*work > 0, "shared branch region work budget");
            *work -= 1;
            if !seen[index] {
                continue;
            }
            for edge in &cfg.blocks[index].successors {
                if edge.target > stop {
                    escaped = Some(escaped.map_or(edge.target, |old: usize| old.min(edge.target)));
                } else if edge.target < stop {
                    seen[edge.target] = true;
                }
            }
        }
        Ok(escaped)
    };
    for branch in instructions.iter().filter(|instruction| {
        matches!(instruction.opcode, 0x32..=0x3d)
            && region.contains(&instruction.pc)
            && shared_loop_owner(atomic_loops, instruction.pc).is_none()
            && excluded
                .binary_search_by_key(&instruction.pc, |edge| edge.branch)
                .is_err()
    }) {
        let block_index = *cfg
            .block_at
            .get(&branch.pc)
            .context("missing shared branch block")?;
        let block = &cfg.blocks[block_index];
        ensure!(
            block.instructions.last() == Some(&branch.pc) && block.successors.len() == 2,
            "invalid shared branch successors"
        );
        let taken_index = block.successors[0].target;
        let fallthrough_index = block.successors[1].target;
        let taken = cfg.blocks[taken_index].start;
        let fallthrough = cfg.blocks[fallthrough_index].start;
        ensure!(
            taken >= fallthrough && fallthrough == branch.pc + branch.width,
            "invalid shared branch arm identity"
        );
        // A strict common postdominator is a safe ceiling. Terminal exits need
        // not traverse the join: preserve the earliest terminal-aware closed
        // region, as the existing renderer does for guard returns/throws.
        let ceiling = postdom[block_index];
        // The already-integrated single-condition route used its canonical
        // common postdominator; retain that source layout for terminal arms.
        let mut join_index = if single_condition {
            ceiling
        } else {
            taken_index
        };
        loop {
            ensure!(
                join_index <= ceiling,
                "shared branch region exceeds postdominator"
            );
            let a = escape(taken_index, join_index)?;
            let b = escape(fallthrough_index, join_index)?;
            if let Some(next) = a.into_iter().chain(b).min() {
                join_index = next;
            } else {
                break;
            }
        }
        let join = cfg.blocks.get(join_index).map_or(len, |block| block.start);
        plans.push(SharedBranch {
            owner: None,
            branch: branch.pc,
            taken,
            fallthrough,
            join,
        });
        ensure!(plans.len() <= 1024, "shared conditional budget");
    }
    ensure!(!plans.is_empty(), "missing shared forward condition");
    Ok(plans)
}

#[cfg(test)]
fn decoded_natural_loop(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
) -> Result<Loop> {
    let loops = decoded_natural_loops(ir, cfg)?;
    ensure!(loops.len() == 1, "expected one shared natural loop");
    Ok(loops[0])
}
// JADX LoopRegionMaker's condition-at-end case identifies the latch condition
// and builds the mandatory body from the loop start. This specialization keeps
// bounded forward body regions and uses the complete decoded CFG for ownership.
fn decoded_posttest_loop(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
    work: &mut usize,
) -> Result<Loop> {
    let mut controls = ir.instructions.iter().filter(|i| {
        matches!(i.opcode, 0x32..=0x3d) && i.branch_target.is_some_and(|target| target < i.pc)
    });
    let latch = controls.next().context("missing shared posttest latch")?;
    ensure!(
        controls.next().is_none() && matches!(latch.opcode, 0x32..=0x3d) && latch.width == 2,
        "shared posttest control shape"
    );
    let start = latch
        .branch_target
        .context("missing shared posttest header")?;
    let exit = latch.pc + latch.width;
    ensure!(start < latch.pc, "shared posttest latch is not backward");
    let mut breaks = 0usize;
    ensure!(
        ir.instructions.iter().all(|i| {
            if !matches!(i.opcode, 0x28..=0x2a | 0x32..=0x3d) || i.pc == latch.pc {
                return true;
            }
            if !(start..latch.pc).contains(&i.pc) {
                return false;
            }
            if i.branch_target == Some(exit) {
                breaks += 1;
                return matches!(i.opcode, 0x32..=0x3d)
                    && i.width == 2
                    && i.pc + i.width <= latch.pc;
            }
            i.branch_target
                .is_some_and(|target| target > i.pc && target <= latch.pc)
        }),
        "shared posttest additional control"
    );
    ensure!(breaks <= 8, "shared posttest break budget");
    let header = *cfg
        .block_at
        .get(&start)
        .context("shared posttest header boundary")?;
    let from = *cfg
        .block_at
        .get(&latch.pc)
        .context("shared posttest latch block")?;
    let out = *cfg
        .block_at
        .get(&exit)
        .context("shared posttest exit boundary")?;
    ensure!(
        cfg.blocks[header].start == start
            && cfg.blocks[out].start == exit
            && cfg.blocks[from].instructions.last() == Some(&latch.pc)
            && cfg.blocks[from].successors.len() == 2
            && cfg.blocks[from].successors[0].target == header
            && cfg.blocks[from].successors[1].target == out,
        "shared posttest successor identity"
    );
    let dominators = crate::native_dominators::DominatorTree::compute(cfg)?;
    ensure!(
        dominators.dominates(header, from),
        "shared posttest header does not dominate latch"
    );
    let mut members = vec![false; cfg.blocks.len()];
    members[header] = true;
    members[from] = true;
    let mut pending = vec![from];
    while let Some(block) = pending.pop() {
        if block == header {
            continue;
        }
        for &predecessor in &dominators.predecessors[block] {
            ensure!(*work > 0, "shared posttest membership work budget");
            *work -= 1;
            ensure!(
                predecessor < cfg.blocks.len(),
                "synthetic shared posttest predecessor"
            );
            if !members[predecessor] {
                members[predecessor] = true;
                pending.push(predecessor);
            }
        }
    }
    for (index, block) in cfg.blocks.iter().enumerate() {
        ensure!(*work > 0, "shared posttest membership work budget");
        *work -= 1;
        ensure!(
            members[index] == (header..=from).contains(&index),
            "shared posttest cross-region membership"
        );
        if members[index] {
            ensure!(
                dominators.dominates(header, index),
                "shared posttest non-dominated body"
            );
            for edge in &block.successors {
                ensure!(
                    members[edge.target]
                        || (edge.target == out
                            && (index == from
                                || (breaks > 0
                                    && block.instructions.last().is_some_and(|&pc| {
                                        ir.instructions
                                            .iter()
                                            .any(|i| i.pc == pc && i.branch_target == Some(exit))
                                    })))),
                    "shared posttest additional exit"
                );
            }
        } else {
            ensure!(
                block
                    .successors
                    .iter()
                    .all(|edge| !members[edge.target] || edge.target == header),
                "shared posttest non-header entry"
            );
        }
    }
    ensure!(
        ir.instructions
            .iter()
            .all(|i| !(start..latch.pc).contains(&i.pc) || !matches!(i.opcode, 0x0e..=0x11 | 0x27)),
        "terminal shared posttest body"
    );
    Ok(Loop {
        parent: None,
        start,
        latch: latch.pc,
        guard: None,
        exit,
        tail: None,
    })
}
// Two conditional backedges can share one header while earlier guards leave
// through a common exit. The last latch's fallthrough is one terminal return;
// it must stay distinct from the common exit and from the earlier backedge.
fn decoded_dual_conditional_backedges(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
    work: &mut usize,
) -> Result<Loop> {
    let backedges: Vec<_> = ir
        .instructions
        .iter()
        .filter(|i| matches!(i.opcode, 0x32..=0x3d) && i.branch_target.is_some_and(|to| to < i.pc))
        .collect();
    ensure!(backedges.len() == 2, "shared dual-backedge count");
    let (early, latch) = (backedges[0], backedges[1]);
    let start = latch.branch_target.context("shared dual-backedge header")?;
    ensure!(
        early.branch_target == Some(start)
            && start < early.pc
            && early.pc + early.width < latch.pc
            && early.width == 2
            && latch.width == 2,
        "shared dual-backedge header and order"
    );
    let tail = latch.pc + latch.width;
    let exit = tail + 1;
    let tail_instruction = ir
        .instructions
        .iter()
        .find(|i| i.pc == tail)
        .context("shared dual-backedge terminal tail")?;
    ensure!(
        matches!(tail_instruction.opcode, 0x0e..=0x11)
            && tail_instruction.width == 1
            && ir.instructions.iter().any(|i| i.pc == exit),
        "shared dual-backedge terminal tail shape"
    );
    let mut breaks = 0;
    let mut terminal_edges = 0;
    for instruction in &ir.instructions {
        if (start..=latch.pc).contains(&instruction.pc)
            && matches!(instruction.opcode, 0x0e..=0x11 | 0x27)
        {
            bail!("shared dual-backedge terminal body");
        }
        if !matches!(instruction.opcode, 0x28..=0x2a | 0x32..=0x3d)
            || instruction.pc == early.pc
            || instruction.pc == latch.pc
        {
            continue;
        }
        let target = instruction
            .branch_target
            .context("shared dual-backedge target")?;
        if instruction.pc < start {
            ensure!(
                target > instruction.pc && target <= start,
                "shared dual-backedge prefix enters body"
            );
        } else if instruction.pc < latch.pc {
            if target == exit {
                breaks += 1;
                ensure!(
                    matches!(instruction.opcode, 0x32..=0x3d) && instruction.width == 2,
                    "shared dual-backedge break shape"
                );
            } else if target == tail {
                terminal_edges += 1;
                ensure!(
                    matches!(instruction.opcode, 0x32..=0x3d) && instruction.width == 2,
                    "shared dual-backedge terminal edge shape"
                );
            } else {
                ensure!(
                    target > instruction.pc && target <= latch.pc,
                    "shared dual-backedge interior control"
                );
            }
        } else {
            ensure!(
                target > instruction.pc && target >= exit,
                "shared dual-backedge later reentry"
            );
        }
    }
    ensure!(
        breaks <= 2 && terminal_edges <= 1,
        "shared dual-backedge exit budget"
    );
    let header = *cfg
        .block_at
        .get(&start)
        .context("shared dual-backedge header block")?;
    let first = *cfg
        .block_at
        .get(&early.pc)
        .context("shared dual-backedge first block")?;
    let last = *cfg
        .block_at
        .get(&latch.pc)
        .context("shared dual-backedge latch block")?;
    let terminal = *cfg
        .block_at
        .get(&tail)
        .context("shared dual-backedge terminal block")?;
    let common = *cfg
        .block_at
        .get(&exit)
        .context("shared dual-backedge exit block")?;
    for (index, branch, fallthrough) in
        [(first, early, early.pc + early.width), (last, latch, tail)]
    {
        let block = &cfg.blocks[index];
        ensure!(
            block.instructions.last() == Some(&branch.pc)
                && block.successors.len() == 2
                && block.successors[0].target == header
                && cfg.blocks[block.successors[1].target].start == fallthrough,
            "shared dual-backedge canonical successors"
        );
    }
    ensure!(
        cfg.blocks[terminal].instructions.as_slice() == [tail]
            && cfg.blocks[terminal].successors.is_empty()
            && cfg.blocks[common].start == exit,
        "shared dual-backedge terminal ownership"
    );
    let dominators = crate::native_dominators::DominatorTree::compute(cfg)?;
    ensure!(
        dominators.dominates(header, first) && dominators.dominates(header, last),
        "shared dual-backedge header dominance"
    );
    let mut members = vec![false; cfg.blocks.len()];
    members[header] = true;
    let mut pending = vec![first, last];
    while let Some(block) = pending.pop() {
        ensure!(*work > 0, "shared dual-backedge membership budget");
        *work -= 1;
        if members[block] {
            continue;
        }
        members[block] = true;
        if block != header {
            pending.extend(dominators.predecessors[block].iter().copied());
        }
    }
    for (index, block) in cfg.blocks.iter().enumerate() {
        ensure!(*work > 0, "shared dual-backedge ownership budget");
        *work -= 1;
        ensure!(
            members[index] == (header..=last).contains(&index),
            "shared dual-backedge cross-region membership"
        );
        if members[index] {
            ensure!(
                dominators.dominates(header, index),
                "shared dual-backedge non-dominated member"
            );
            for edge in &block.successors {
                let pc = *block
                    .instructions
                    .last()
                    .context("empty dual-backedge block")?;
                let owner = ir
                    .instructions
                    .iter()
                    .find(|i| i.pc == pc)
                    .context("missing dual-backedge terminator")?;
                ensure!(
                    members[edge.target]
                        || (edge.target == common && owner.branch_target == Some(exit))
                        || (edge.target == terminal
                            && (index == last || owner.branch_target == Some(tail))),
                    "shared dual-backedge additional exit"
                );
            }
        } else {
            ensure!(
                block
                    .successors
                    .iter()
                    .all(|edge| !members[edge.target] || edge.target == header),
                "shared dual-backedge interior entry"
            );
        }
    }
    Ok(Loop {
        parent: None,
        start,
        latch: latch.pc,
        guard: None,
        exit,
        tail: Some(tail),
    })
}
// One pretest guard and a conditional latch can have two terminal return
// tails: the guard's zero-iteration/default return and the body/latch result.
// Keep both exits inside a terminal loop region, without inventing a common
// liveout frame or moving either tail's effects across the loop boundary.
fn decoded_pretest_terminal_loop(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
    work: &mut usize,
) -> Result<Loop> {
    let mut backedges = ir.instructions.iter().filter(|i| {
        matches!(i.opcode, 0x32..=0x3d) && i.branch_target.is_some_and(|target| target < i.pc)
    });
    let latch = backedges.next().context("missing shared terminal latch")?;
    ensure!(
        backedges.next().is_none() && latch.width == 2,
        "shared terminal latch shape"
    );
    let start = latch
        .branch_target
        .context("missing shared terminal header")?;
    let selected = latch.pc + latch.width;
    let default = selected + 1;
    ensure!(start < latch.pc, "shared terminal latch is not backward");
    let selected_return = ir
        .instructions
        .iter()
        .find(|i| i.pc == selected)
        .context("missing selected terminal return")?;
    let default_return = ir
        .instructions
        .iter()
        .find(|i| i.pc == default)
        .context("missing default terminal return")?;
    ensure!(
        matches!(selected_return.opcode, 0x0e..=0x11)
            && matches!(default_return.opcode, 0x0e..=0x11)
            && selected_return.width == 1
            && default_return.width == 1
            && ir.instructions.last() == Some(default_return),
        "shared terminal exits are not adjacent final returns"
    );
    let guards: Vec<_> = ir
        .instructions
        .iter()
        .filter(|i| {
            (start..latch.pc).contains(&i.pc)
                && matches!(i.opcode, 0x32..=0x3d)
                && i.branch_target == Some(default)
        })
        .collect();
    ensure!(guards.len() == 1, "shared terminal default guard identity");
    let guard = guards[0];
    let mut selected_edges = 0;
    ensure!(
        ir.instructions.iter().all(|i| {
            if !matches!(i.opcode, 0x28..=0x2a | 0x32..=0x3d) || i.pc == latch.pc {
                return true;
            }
            if i.pc < start {
                return i
                    .branch_target
                    .is_some_and(|target| target > i.pc && target <= start);
            }
            if !(start..latch.pc).contains(&i.pc) {
                return false;
            }
            if i.pc == guard.pc {
                return i.width == 2;
            }
            if i.branch_target == Some(selected) {
                selected_edges += 1;
                return matches!(i.opcode, 0x32..=0x3d) && i.width == 2;
            }
            i.branch_target
                .is_some_and(|target| target > i.pc && target <= latch.pc)
        }),
        "shared terminal additional control"
    );
    ensure!(selected_edges <= 1, "shared terminal selected-edge budget");
    ensure!(
        ir.instructions.iter().all(|i| {
            !(start..latch.pc).contains(&i.pc) || !matches!(i.opcode, 0x0e..=0x11 | 0x27)
        }),
        "terminal return inside shared loop body"
    );
    let header = *cfg
        .block_at
        .get(&start)
        .context("shared terminal header boundary")?;
    let from = *cfg
        .block_at
        .get(&latch.pc)
        .context("shared terminal latch boundary")?;
    let selected_block = *cfg
        .block_at
        .get(&selected)
        .context("shared terminal selected boundary")?;
    let default_block = *cfg
        .block_at
        .get(&default)
        .context("shared terminal default boundary")?;
    let guard_block = *cfg
        .block_at
        .get(&guard.pc)
        .context("shared terminal guard boundary")?;
    ensure!(
        cfg.blocks[from].instructions.last() == Some(&latch.pc)
            && cfg.blocks[from].successors.len() == 2
            && cfg.blocks[from].successors[0].target == header
            && cfg.blocks[from].successors[1].target == selected_block
            && cfg.blocks[guard_block].instructions.last() == Some(&guard.pc)
            && cfg.blocks[guard_block].successors.len() == 2
            && cfg.blocks[guard_block].successors[0].target == default_block
            && cfg.blocks[selected_block].successors.is_empty()
            && cfg.blocks[default_block].successors.is_empty(),
        "shared terminal successor identity"
    );
    let dominators = crate::native_dominators::DominatorTree::compute(cfg)?;
    ensure!(
        dominators.dominates(header, from),
        "shared terminal header does not dominate latch"
    );
    let mut members = vec![false; cfg.blocks.len()];
    members[header] = true;
    members[from] = true;
    let mut pending = vec![from];
    while let Some(block) = pending.pop() {
        if block == header {
            continue;
        }
        for &predecessor in &dominators.predecessors[block] {
            ensure!(*work > 0, "shared terminal membership work budget");
            *work -= 1;
            ensure!(
                predecessor < cfg.blocks.len(),
                "synthetic terminal predecessor"
            );
            if !members[predecessor] {
                members[predecessor] = true;
                pending.push(predecessor);
            }
        }
    }
    for (index, block) in cfg.blocks.iter().enumerate() {
        ensure!(*work > 0, "shared terminal membership work budget");
        *work -= 1;
        ensure!(
            members[index] == (header..=from).contains(&index),
            "shared terminal cross-region membership"
        );
        if members[index] {
            ensure!(
                dominators.dominates(header, index),
                "shared terminal non-dominated body"
            );
            for edge in &block.successors {
                ensure!(
                    members[edge.target]
                        || (index == guard_block && edge.target == default_block)
                        || (edge.target == selected_block
                            && (index == from
                                || block.instructions.last().is_some_and(|&pc| {
                                    ir.instructions
                                        .iter()
                                        .any(|i| i.pc == pc && i.branch_target == Some(selected))
                                }))),
                    "shared terminal additional exit"
                );
            }
        } else {
            ensure!(
                block
                    .successors
                    .iter()
                    .all(|edge| { !members[edge.target] || edge.target == header }),
                "shared terminal interior entry"
            );
        }
    }
    Ok(Loop {
        parent: None,
        start,
        latch: latch.pc,
        guard: Some(guard.pc),
        exit: default + 1,
        tail: None,
    })
}
fn decoded_natural_loops(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
) -> Result<Vec<Loop>> {
    if !ir.instructions.iter().any(|i| {
        matches!(i.opcode, 0x28..=0x2a) && i.branch_target.is_some_and(|target| target < i.pc)
    }) {
        if ir
            .instructions
            .iter()
            .filter(|i| {
                matches!(i.opcode, 0x32..=0x3d)
                    && i.branch_target.is_some_and(|target| target < i.pc)
            })
            .count()
            == 2
        {
            return Ok(vec![decoded_dual_conditional_backedges(
                ir,
                cfg,
                &mut 16_000_000,
            )?]);
        }
        if ir.instructions.iter().any(|i| {
            matches!(i.opcode, 0x32..=0x3d)
                && i.branch_target.is_some_and(|target| target < i.pc)
                && ir.instructions.iter().any(|guard| {
                    matches!(guard.opcode, 0x32..=0x3d)
                        && guard.branch_target == Some(i.pc + i.width + 1)
                })
        }) {
            return Ok(vec![decoded_pretest_terminal_loop(
                ir,
                cfg,
                &mut 16_000_000,
            )?]);
        }
        return Ok(vec![decoded_posttest_loop(ir, cfg, &mut 16_000_000)?]);
    }
    // JADX BlockProcessor.markLoops/registerLoops identifies and records
    // dominating-successor backedges. processNestedLoops derives containing
    // parents; this bounded specialization admits properly nested pretest
    // regions and leaves intersecting or cross-owner shapes on the legacy route.
    let dominators = crate::native_dominators::DominatorTree::compute(cfg)?;
    let mut regions = Vec::new();
    for (from, block) in cfg.blocks.iter().enumerate() {
        let pc = *block
            .instructions
            .last()
            .context("empty shared CFG block")?;
        let instruction = &ir.instructions[ir
            .instructions
            .binary_search_by_key(&pc, |i| i.pc)
            .map_err(|_| anyhow::anyhow!("missing shared block terminator"))?];
        if !matches!(instruction.opcode, 0x28..=0x2a) {
            continue;
        }
        for edge in block.successors.iter().filter(|edge| edge.target <= from) {
            ensure!(
                dominators.dominates(edge.target, from),
                "shared loop latch is not uniquely dominated"
            );
            let start = cfg.blocks[edge.target].start;
            ensure!(
                decoded_goto(instruction)? == start,
                "shared loop latch differs from header"
            );
            let guard_pc = *cfg.blocks[edge.target]
                .instructions
                .last()
                .context("missing shared pretest guard")?;
            let guard = &ir.instructions[ir
                .instructions
                .binary_search_by_key(&guard_pc, |i| i.pc)
                .map_err(|_| anyhow::anyhow!("missing shared pretest guard instruction"))?];
            let exit = pc + instruction.width;
            ensure!(
                matches!(guard.opcode, 0x32..=0x3d)
                    && guard.pc >= start
                    && guard.pc < pc
                    && guard.branch_target == Some(exit),
                "invalid shared pretest loop guard"
            );
            regions.push(Loop {
                parent: None,
                start,
                latch: pc,
                guard: Some(guard.pc),
                exit,
                tail: None,
            });
            ensure!(
                regions.len() <= MAX_SHARED_LOOPS,
                "shared loop region budget"
            );
        }
    }
    ensure!(!regions.is_empty(), "missing shared natural-loop latch");
    regions.sort_unstable_by_key(|region| region.start);
    assign_shared_loop_parents(&mut regions)?;
    let special = decoded_all_loop_edges(ir, &regions)?;
    for instruction in &ir.instructions {
        if let Some(region) = shared_loop_owner(&regions, instruction.pc) {
            let body = region.guard.unwrap() + 2..region.latch;
            if Some(instruction.pc) == region.guard || instruction.pc == region.latch {
                continue;
            }
            if matches!(instruction.opcode,0x28..=0x2a|0x32..=0x3d) {
                let target = instruction
                    .branch_target
                    .context("missing shared region branch target")?;
                ensure!(
                    body.contains(&instruction.pc)
                        && ((target > instruction.pc
                            && target <= region.latch
                            && shared_loop_target_owned(&regions, region, target))
                            || (matches!(instruction.opcode, 0x32..=0x3d)
                                && (target == region.start || target == region.exit))),
                    "shared loop branch crosses region owner"
                );
            }
            ensure!(
                !body.contains(&instruction.pc)
                    || !matches!(instruction.opcode, 0x0e..=0x11 | 0x27),
                "terminal shared loop body"
            );
        } else if matches!(instruction.opcode,0x28..=0x2a|0x32..=0x3d) {
            let target = instruction
                .branch_target
                .context("missing shared outer branch target")?;
            ensure!(
                target > instruction.pc
                    && shared_loop_owner(&regions, target)
                        .is_none_or(|region| target == region.start && region.parent.is_none()),
                "shared outer branch enters loop interior"
            );
        }
    }
    for (from, block) in cfg.blocks.iter().enumerate() {
        for edge in block.successors.iter().filter(|edge| edge.target <= from) {
            let pc = *block.instructions.last().unwrap();
            let region =
                shared_loop_owner(&regions, pc).context("backedge outside shared loop owner")?;
            ensure!(
                cfg.blocks[edge.target].start == region.start
                    && dominators.dominates(edge.target, from)
                    && (pc == region.latch
                        || shared_loop_edge_at(&special, pc)
                            .is_some_and(|edge| edge.owner == region.start
                                && edge.kind == SharedLoopEdgeKind::Continue)),
                "invalid shared loop conditional backedge"
            );
        }
    }
    let mut work = 16_000_000usize;
    for region in &regions {
        let header = cfg.block_at[&region.start];
        let latch = cfg.block_at[&region.latch];
        let exit = *cfg
            .block_at
            .get(&region.exit)
            .context("shared loop exit boundary")?;
        ensure!(
            cfg.blocks[exit].start == region.exit
                && cfg.blocks[header].successors.len() == 2
                && cfg.blocks[header].successors[0].target == exit
                && cfg.blocks[header].successors[1].target > header
                && cfg.blocks[header].successors[1].target <= latch,
            "invalid shared pretest loop successors"
        );
        let mut members = vec![false; cfg.blocks.len()];
        members[header] = true;
        members[latch] = true;
        let mut pending = vec![latch];
        while let Some(block) = pending.pop() {
            for &predecessor in &dominators.predecessors[block] {
                ensure!(work > 0, "shared loop membership work budget");
                work -= 1;
                ensure!(
                    predecessor < cfg.blocks.len(),
                    "synthetic predecessor inside shared loop"
                );
                if !members[predecessor] {
                    members[predecessor] = true;
                    pending.push(predecessor);
                }
            }
        }
        for (index, block) in cfg.blocks.iter().enumerate() {
            ensure!(work > 0, "shared loop membership work budget");
            work -= 1;
            ensure!(
                members[index] == (header..=latch).contains(&index),
                "shared loop has cross-region membership"
            );
            if members[index] {
                ensure!(
                    dominators.dominates(header, index),
                    "shared loop header does not dominate body"
                );
                for edge in &block.successors {
                    let special = block
                        .instructions
                        .last()
                        .and_then(|pc| shared_loop_edge_at(&special, *pc));
                    ensure!(
                        members[edge.target]
                            || ((index == header
                                || special.is_some_and(|edge| edge.owner == region.start
                                    && edge.kind == SharedLoopEdgeKind::Break))
                                && edge.target == exit),
                        "shared loop has an additional exit"
                    );
                }
            } else {
                ensure!(
                    block
                        .successors
                        .iter()
                        .all(|edge| !members[edge.target] || edge.target == header),
                    "shared loop has a non-header entry"
                );
            }
        }
    }
    Ok(regions)
}
fn decoded_loop_branch_plans(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
    regions: &[Loop],
    len: usize,
) -> Result<Vec<SharedBranch>> {
    if regions.len() == 1 && regions[0].guard.is_none() && regions[0].tail.is_some() {
        let region = regions[0];
        ensure!(
            decoded_dual_conditional_backedges(ir, cfg, &mut 16_000_000)? == region,
            "shared dual-backedge region differs from canonical CFG"
        );
        let special = decoded_all_loop_edges(ir, regions)?;
        let mut projection = cfg.clone();
        let early = special
            .iter()
            .find(|edge| edge.kind == SharedLoopEdgeKind::Continue && edge.owner == region.start)
            .context("missing shared dual-backedge early continue")?;
        for pc in [early.branch, region.latch] {
            let block = &mut projection.blocks[cfg.block_at[&pc]];
            ensure!(
                block.successors.len() == 2
                    && block.successors[0].target == cfg.block_at[&region.start],
                "shared dual-backedge projection differs from CFG"
            );
            block.successors.remove(0);
        }
        for edge in &special {
            if edge.branch == early.branch {
                continue;
            }
            ensure!(
                matches!(
                    edge.kind,
                    SharedLoopEdgeKind::Break | SharedLoopEdgeKind::Terminal
                ),
                "shared dual-backedge edge kind"
            );
            let block = &mut projection.blocks[cfg.block_at[&edge.branch]];
            ensure!(
                block.successors.len() == 2
                    && cfg.blocks[block.successors[0].target].start == edge.taken
                    && cfg.blocks[block.successors[1].target].start == edge.fallthrough,
                "shared dual-backedge projected edge differs from CFG"
            );
            block.successors.remove(0);
        }
        let mut plans = if ir.instructions.iter().any(|i| {
            (region.start..region.latch).contains(&i.pc)
                && matches!(i.opcode, 0x32..=0x3d)
                && shared_loop_edge_at(&special, i.pc).is_none()
        }) {
            decoded_branch_plans_in(
                ir,
                &projection,
                len,
                region.start..region.latch,
                &special,
                &[],
            )?
        } else {
            Vec::new()
        };
        for plan in &mut plans {
            ensure!(
                plan.taken <= region.latch
                    && plan.fallthrough <= region.latch
                    && plan.join <= region.latch,
                "shared dual-backedge body branch leaves owner"
            );
            plan.owner = Some(region.start);
        }
        if ir
            .instructions
            .iter()
            .any(|i| i.pc < region.start && matches!(i.opcode, 0x32..=0x3d))
        {
            let outer =
                decoded_branch_plans_in(ir, &projection, len, 0..region.start, &[], regions)?;
            ensure!(
                outer.iter().all(|plan| {
                    plan.taken <= region.start
                        && plan.fallthrough <= region.start
                        && plan.join <= region.start
                }),
                "shared dual-backedge prefix enters body"
            );
            plans.extend(outer);
        }
        if ir
            .instructions
            .iter()
            .any(|i| i.pc >= region.exit && matches!(i.opcode, 0x32..=0x3d))
        {
            let suffix =
                decoded_branch_plans_in(ir, &projection, len, region.exit..len, &[], regions)?;
            ensure!(
                suffix.iter().all(|plan| plan.branch >= region.exit
                    && plan.taken >= region.exit
                    && plan.fallthrough >= region.exit
                    && plan.join >= region.exit),
                "shared dual-backedge suffix reenters loop"
            );
            plans.extend(suffix);
        }
        plans.sort_unstable_by_key(|plan| plan.branch);
        ensure!(
            plans.windows(2).all(|pair| pair[0].branch < pair[1].branch),
            "duplicate shared dual-backedge branch plan"
        );
        return Ok(plans);
    }
    if regions.len() == 1
        && regions[0].guard.is_some()
        && regions[0].exit == len
        && ir
            .instructions
            .iter()
            .any(|i| i.pc == regions[0].latch && matches!(i.opcode, 0x32..=0x3d))
    {
        ensure!(
            decoded_pretest_terminal_loop(ir, cfg, &mut 16_000_000)? == regions[0],
            "shared terminal region differs"
        );
        let region = regions[0];
        let special = decoded_all_loop_edges(ir, regions)?;
        let mut projection = cfg.clone();
        projection.blocks[cfg.block_at[&region.latch]]
            .successors
            .remove(0);
        for edge in &special {
            ensure!(
                edge.kind == SharedLoopEdgeKind::Terminal,
                "shared terminal edge has wrong owner"
            );
            let block = cfg.block_at[&edge.branch];
            ensure!(
                projection.blocks[block].successors.len() == 2
                    && cfg.blocks[projection.blocks[block].successors[0].target].start
                        == edge.taken
                    && cfg.blocks[projection.blocks[block].successors[1].target].start
                        == edge.fallthrough,
                "shared terminal edge differs from canonical successors"
            );
            projection.blocks[block].successors.remove(0);
        }
        let mut plans = Vec::new();
        let prefix_conditions: Vec<_> = ir
            .instructions
            .iter()
            .filter(|i| i.pc < region.start && matches!(i.opcode, 0x32..=0x3d))
            .collect();
        ensure!(
            prefix_conditions.len() <= 1,
            "shared terminal prefix branch budget"
        );
        if let Some(branch) = prefix_conditions.first() {
            let fallthrough = branch.pc + branch.width;
            let taken = branch
                .branch_target
                .context("shared terminal prefix target")?;
            let returned = ir
                .instructions
                .iter()
                .find(|i| i.pc == fallthrough)
                .context("shared terminal prefix return")?;
            let block = &cfg.blocks[cfg.block_at[&branch.pc]];
            ensure!(
                branch.width == 2
                    && taken == fallthrough + 1
                    && taken <= region.start
                    && matches!(returned.opcode, 0x0e..=0x11)
                    && returned.width == 1
                    && block.instructions.last() == Some(&branch.pc)
                    && block.successors.len() == 2
                    && cfg.blocks[block.successors[0].target].start == taken
                    && cfg.blocks[block.successors[1].target].start == fallthrough,
                "shared terminal prefix is not a closed early return"
            );
            plans.push(SharedBranch {
                owner: None,
                branch: branch.pc,
                taken,
                fallthrough,
                join: taken,
            });
        }
        let body = region.guard.unwrap() + 2..region.latch;
        if ir.instructions.iter().any(|i| {
            body.contains(&i.pc)
                && matches!(i.opcode, 0x32..=0x3d)
                && shared_loop_edge_at(&special, i.pc).is_none()
        }) {
            let mut inner =
                decoded_branch_plans_in(ir, &projection, len, body.clone(), &special, &[])?;
            for plan in &mut inner {
                ensure!(
                    plan.join <= region.latch
                        && plan.taken <= region.latch
                        && plan.fallthrough <= region.latch,
                    "shared terminal body branch leaves loop"
                );
                plan.owner = Some(region.start);
            }
            plans.extend(inner);
        }
        plans.sort_unstable_by_key(|p| p.branch);
        return Ok(plans);
    }
    if regions.len() == 1 && regions[0].guard.is_none() {
        ensure!(
            decoded_posttest_loop(ir, cfg, &mut 16_000_000)? == regions[0],
            "shared posttest region differs"
        );
        let region = regions[0];
        let special = decoded_all_loop_edges(ir, regions)?;
        if !ir.instructions.iter().any(|i| {
            (region.start..region.latch).contains(&i.pc)
                && matches!(i.opcode, 0x32..=0x3d)
                && shared_loop_edge_at(&special, i.pc).is_none()
        }) {
            return Ok(Vec::new());
        }
        let mut projection = cfg.clone();
        let latch = cfg.block_at[&region.latch];
        // Only remove the conditional backedge. The fallthrough exit remains
        // canonical, while dominance and liveness use the original cyclic CFG.
        projection.blocks[latch].successors.remove(0);
        for edge in &special {
            let block = cfg.block_at[&edge.branch];
            ensure!(
                edge.kind == SharedLoopEdgeKind::Break
                    && projection.blocks[block].successors.len() == 2
                    && cfg.blocks[projection.blocks[block].successors[0].target].start
                        == edge.taken
                    && cfg.blocks[projection.blocks[block].successors[1].target].start
                        == edge.fallthrough,
                "shared posttest break differs from canonical successors"
            );
            projection.blocks[block].successors.remove(0);
        }
        let mut plans = decoded_branch_plans_in(
            ir,
            &projection,
            len,
            region.start..region.latch,
            &special,
            &[],
        )?;
        for plan in &mut plans {
            ensure!(
                plan.taken <= region.latch
                    && plan.fallthrough <= region.latch
                    && plan.join <= region.latch,
                "shared posttest branch leaves body"
            );
            plan.owner = Some(region.start);
        }
        return Ok(plans);
    }
    let special = decoded_all_loop_edges(ir, regions)?;
    let depths = regions
        .iter()
        .map(|&region| shared_loop_depth(regions, region))
        .collect::<Result<Vec<_>>>()?;
    let mut work = 16_000_000usize;
    let mut plans = Vec::new();
    // At most four body projections. Closure work is cumulative across depths;
    // each forward-postdom pass retains its existing 20m work bound.
    for depth in 1..=depths.iter().copied().max().unwrap_or(0) {
        let active: Vec<Loop> = regions
            .iter()
            .zip(&depths)
            .filter(|(_, d)| **d == depth)
            .map(|(&region, _)| region)
            .collect();
        if !ir.instructions.iter().any(|i| {
            matches!(i.opcode, 0x32..=0x3d)
                && shared_loop_owner(&active, i.pc).is_some_and(|region| Some(i.pc) != region.guard)
                && shared_loop_owner(regions, i.pc)
                    .is_some_and(|owner| active.iter().any(|region| region.start == owner.start))
                && shared_loop_edge_at(&special, i.pc).is_none()
        }) {
            continue;
        }
        let children: Vec<Loop> = regions
            .iter()
            .zip(&depths)
            .filter(|(_, d)| **d == depth + 1)
            .map(|(&region, _)| region)
            .collect();
        let mut projection = cfg.clone();
        for (region, d) in regions.iter().zip(&depths) {
            if *d <= depth {
                projection.blocks[cfg.block_at[&region.latch]]
                    .successors
                    .clear();
            }
        }
        for edge in &special {
            let owner = regions[regions
                .binary_search_by_key(&edge.owner, |region| region.start)
                .unwrap()];
            if shared_loop_depth(regions, owner)? > depth {
                continue;
            }
            let block = cfg.block_at[&edge.branch];
            ensure!(
                projection.blocks[block].successors.len() == 2
                    && cfg.blocks[projection.blocks[block].successors[0].target].start
                        == edge.taken
                    && cfg.blocks[projection.blocks[block].successors[1].target].start
                        == edge.fallthrough,
                "shared loop edge differs from canonical successors"
            );
            projection.blocks[block].successors.remove(0);
        }
        for child in &children {
            collapse_shared_loop(&mut projection, cfg, *child)?;
        }
        let postdom = projection.forward_postdominators()?;
        for region in active {
            let body = region.guard.context("missing shared loop guard")? + 2..region.latch;
            let first = ir.instructions.partition_point(|i| i.pc < body.start);
            let end = ir.instructions.partition_point(|i| i.pc < body.end);
            if !ir.instructions[first..end].iter().any(|i| {
                matches!(i.opcode, 0x32..=0x3d)
                    && shared_loop_owner(regions, i.pc)
                        .is_some_and(|owner| owner.start == region.start)
                    && shared_loop_edge_at(&special, i.pc).is_none()
            }) {
                continue;
            }
            let mut inner = decoded_branch_plans_with_postdom(
                ir,
                &projection,
                len,
                body.clone(),
                &special,
                &children,
                &postdom,
                &mut work,
            )?;
            for plan in &mut inner {
                ensure!(
                    body.contains(&plan.branch)
                        && (body.start..=region.latch).contains(&plan.fallthrough)
                        && plan.taken >= body.start
                        && plan.taken <= region.latch
                        && plan.join >= plan.taken
                        && plan.join <= region.latch
                        && [plan.taken, plan.fallthrough, plan.join]
                            .into_iter()
                            .all(|pc| shared_loop_owner(&children, pc)
                                .is_none_or(|child| pc == child.start)),
                    "shared loop branch join crosses child ownership"
                );
                plan.owner = Some(region.start);
            }
            plans.extend(inner);
        }
    }
    let roots: Vec<Loop> = regions
        .iter()
        .copied()
        .filter(|region| region.parent.is_none())
        .collect();
    if ir
        .instructions
        .iter()
        .any(|i| matches!(i.opcode, 0x32..=0x3d) && shared_loop_owner(&roots, i.pc).is_none())
    {
        let mut projection = cfg.clone();
        for root in &roots {
            collapse_shared_loop(&mut projection, cfg, *root)?;
        }
        let outer = decoded_branch_plans_in(ir, &projection, len, 0..len, &[], &roots)?;
        for plan in &outer {
            ensure!(
                [plan.taken, plan.fallthrough, plan.join]
                    .into_iter()
                    .all(|pc| shared_loop_owner(&roots, pc).is_none_or(|root| pc == root.start)),
                "shared outer plan crosses loop interior"
            );
        }
        plans.extend(outer);
    }
    plans.sort_unstable_by_key(|plan| plan.branch);
    ensure!(
        plans.windows(2).all(|pair| pair[0].branch < pair[1].branch),
        "duplicate shared loop branch ownership"
    );
    Ok(plans)
}
fn collapse_shared_loop(
    projection: &mut crate::native_cfg::ControlFlowGraph,
    cfg: &crate::native_cfg::ControlFlowGraph,
    region: Loop,
) -> Result<()> {
    let header = cfg.block_at[&region.start];
    let exit = cfg.block_at[&region.exit];
    ensure!(
        projection.blocks[header].successors.len() == 2
            && projection.blocks[header].successors[0].target == exit,
        "invalid atomic loop successor"
    );
    projection.blocks[header].successors.truncate(1);
    for block in &mut projection.blocks[header + 1..exit] {
        block.successors.clear();
    }
    Ok(())
}

struct SharedLiveness {
    bits: Vec<u64>,
    stride: usize,
    registers: usize,
}
fn decoded_cfg_liveness(
    ir: &crate::native_ir::DecodedMethod,
    cfg: &crate::native_cfg::ControlFlowGraph,
    registers: usize,
) -> Result<Option<SharedLiveness>> {
    const MAX_STORAGE: usize = 32 * 1024 * 1024;
    const MAX_WORK: usize = 16_000_000;
    let stride = registers.div_ceil(64);
    let Some(storage) = cfg
        .blocks
        .len()
        .checked_mul(stride)
        .and_then(|count| count.checked_mul(8))
    else {
        return Ok(None);
    };
    if storage > MAX_STORAGE {
        return Ok(None);
    }
    let mut bits = vec![0u64; storage / 8];
    let mut transfer = vec![0u64; stride];
    let cyclic = cfg
        .blocks
        .iter()
        .enumerate()
        .any(|(index, block)| block.successors.iter().any(|edge| edge.target <= index));
    let mut work = 0usize;
    loop {
        let mut changed = false;
        for (block_index, block) in cfg.blocks.iter().enumerate().rev() {
            transfer.fill(0);
            for edge in &block.successors {
                work = work.saturating_add(stride);
                if work > MAX_WORK {
                    return Ok(None);
                }
                for word in 0..stride {
                    transfer[word] |= bits[edge.target * stride + word];
                }
            }
            for &pc in block.instructions.iter().rev() {
                let instruction = &ir.instructions[ir
                    .instructions
                    .binary_search_by_key(&pc, |instruction| instruction.pc)
                    .map_err(|_| anyhow::anyhow!("missing shared liveness instruction"))?];
                for (operands, write) in [(&instruction.writes, true), (&instruction.reads, false)]
                {
                    for operand in operands {
                        let start = usize::from(operand.register);
                        let end = start + operand.kind.word_count();
                        ensure!(
                            end <= registers,
                            "shared liveness operand outside register file"
                        );
                        work = work.saturating_add(end - start);
                        if work > MAX_WORK {
                            return Ok(None);
                        }
                        for register in start..end {
                            let mask = 1 << (register % 64);
                            if write {
                                transfer[register / 64] &= !mask;
                            } else {
                                transfer[register / 64] |= mask;
                            }
                        }
                    }
                }
            }
            work = work.saturating_add(stride);
            if work > MAX_WORK {
                return Ok(None);
            }
            let current = &mut bits[block_index * stride..(block_index + 1) * stride];
            if current != transfer {
                current.copy_from_slice(&transfer);
                changed = true;
            }
        }
        if !cyclic || !changed {
            break;
        }
    }
    Ok(Some(SharedLiveness {
        bits,
        stride,
        registers,
    }))
}
fn decoded_condition_sources(
    instruction: &crate::native_ir::Instruction,
    registers: usize,
) -> Result<(usize, Option<usize>)> {
    use crate::native_ir::ValueKind;
    let zero = matches!(instruction.opcode, 0x38..=0x3d);
    ensure!(
        matches!(instruction.opcode, 0x32..=0x3d)
            && instruction.width == 2
            && !instruction.may_throw
            && instruction.writes.is_empty()
            && instruction.reads.len() == if zero { 1 } else { 2 }
            && instruction.literal.is_none()
            && instruction.reference.is_none()
            && instruction.prototype.is_none()
            && instruction.payload_target.is_none()
            && instruction.branch_target.is_some(),
        "invalid shared condition operands/metadata"
    );
    let left = decoded_register(&instruction.reads[0], ValueKind::Unknown32, registers)?;
    let right = if zero {
        None
    } else {
        Some(decoded_register(
            &instruction.reads[1],
            ValueKind::Unknown32,
            registers,
        )?)
    };
    Ok((left, right))
}
fn decoded_goto(instruction: &crate::native_ir::Instruction) -> Result<usize> {
    ensure!(
        matches!(instruction.opcode, 0x28..=0x2a)
            && instruction.width == usize::from(instruction.opcode - 0x27)
            && instruction.reads.is_empty()
            && instruction.writes.is_empty()
            && !instruction.may_throw
            && instruction.literal.is_none()
            && instruction.reference.is_none()
            && instruction.prototype.is_none()
            && instruction.payload_target.is_none(),
        "invalid shared goto metadata"
    );
    instruction
        .branch_target
        .context("missing shared goto target")
}

#[derive(Clone)]
struct ProtectedLoopEscape {
    start: usize,
    end: usize,
    label: String,
    forbidden_writes: Vec<bool>,
    used: std::rc::Rc<std::cell::Cell<bool>>,
}

struct Graph {
    front_end: Option<crate::native_method::MethodFrontEnd>,
    shared_cfg: Option<crate::native_cfg::ControlFlowGraph>,
    shared_loops: Vec<Loop>,
    shared_loop_edges: Vec<SharedLoopEdge>,
    shared_branches: Vec<SharedBranch>,
    shared_live: Option<SharedLiveness>,
    synchronized: Vec<synchronized::Region>,
    monitor_dispatch: std::cell::RefCell<Vec<synchronized::Dispatch>>,
    protected: Vec<std::ops::Range<usize>>,
    protected_loop_escape: std::cell::RefCell<Option<ProtectedLoopEscape>>,
    caught_values: std::cell::RefCell<std::collections::HashSet<String>>,
    catch_rethrows: std::cell::RefCell<std::collections::HashMap<String, String>>,
    constructor_bindings: std::cell::OnceCell<
        Option<std::collections::HashMap<usize, crate::native_constructors::ConstructorBinding>>,
    >,
    shrink_ssa: std::cell::OnceCell<Option<crate::native_ssa::SsaMethod>>,
    live: Option<super::liveness::LiveRegisters>,
    switches: Vec<Option<Switch>>,
    payloads: Vec<bool>,
    loops: Vec<Loop>,
    widths: Vec<usize>,
    targets: Vec<Option<usize>>,
    acyclic_backwards: std::collections::HashSet<usize>,
    work: std::cell::Cell<usize>,
}
impl Graph {
    fn single_use_call_result(
        &self,
        method: &DexMethod,
        consumer_pc: usize,
        argument_register: u16,
        result_type: &str,
    ) -> bool {
        let Some(code) = method.code.as_ref() else {
            return false;
        };
        if code.tries != 0 || !code.try_regions.is_empty() || code.instructions.len() > 512 {
            return false;
        }
        let Some(front) = self.front_end.as_ref() else {
            return false;
        };
        let index = front
            .ir
            .instructions
            .partition_point(|instruction| instruction.pc < consumer_pc);
        if index < 2
            || front
                .ir
                .instructions
                .get(index)
                .is_none_or(|instruction| instruction.pc != consumer_pc)
        {
            return false;
        }
        let move_result = &front.ir.instructions[index - 1];
        let producer = &front.ir.instructions[index - 2];
        if !matches!(move_result.opcode, 0x0a..=0x0c)
            || !matches!(producer.opcode, 0x71 | 0x77)
            || move_result.pc + move_result.width != consumer_pc
            || producer.pc + producer.width != move_result.pc
        {
            return false;
        }
        let Some(bound) = front.bound.calls.iter().find(|call| call.pc == producer.pc) else {
            return false;
        };
        if bound.return_type.as_ref() != result_type
            || bound.result.as_ref().is_none_or(|result| {
                result.move_pc != move_result.pc || result.register.register != argument_register
            })
        {
            return false;
        }
        let Some(ssa) = self.shrink_ssa.get_or_init(|| {
            let cfg = crate::native_cfg::ControlFlowGraph::from_decoded(
                &front.ir,
                code.instructions.len(),
            )
            .ok()?;
            crate::native_ssa::SsaMethod::build_with_work_limit(code, &front.ir, &cfg, 2_000_000)
                .ok()
        }) else {
            return false;
        };
        let Some(&block) = ssa.graph.block_at.get(&producer.pc) else {
            return false;
        };
        if ssa.graph.block_at.get(&move_result.pc) != Some(&block)
            || ssa.graph.block_at.get(&consumer_pc) != Some(&block)
        {
            return false;
        }
        let Some(move_insn) = ssa
            .instructions
            .iter()
            .find(|insn| insn.pc == move_result.pc)
        else {
            return false;
        };
        let Some(consumer) = ssa.instructions.iter().find(|insn| insn.pc == consumer_pc) else {
            return false;
        };
        let Some(write) = move_insn
            .writes
            .iter()
            .find(|operand| operand.register == argument_register)
        else {
            return false;
        };
        if write.words.is_empty() || write.words.len() > 2 {
            return false;
        }
        for (word, &id) in write.words.iter().enumerate() {
            if ssa
                .phis
                .iter()
                .any(|phi| phi.incoming.iter().any(|(_, incoming)| *incoming == id))
            {
                return false;
            }
            let uses = ssa
                .instructions
                .iter()
                .flat_map(|insn| insn.reads.iter().map(move |read| (insn.pc, read)))
                .flat_map(|(pc, read)| read.words.iter().map(move |&word| (pc, word)))
                .filter(|(_, word)| *word == id)
                .collect::<Vec<_>>();
            if uses.len() != 1
                || uses[0].0 != consumer_pc
                || !consumer.reads.iter().any(|read| {
                    read.register == argument_register + word as u16 && read.words.contains(&id)
                })
            {
                return false;
            }
        }
        true
    }
    /// Build the native identity pipeline only when an extended allocation
    /// region needs it. Keep the ordinary renderer path and repeated allocation
    /// lookups cheap; failed analysis is cached as a conservative decline.
    fn constructor_binding(
        &self,
        class: &DexClass,
        method: &DexMethod,
        invoke_pc: usize,
    ) -> Option<&crate::native_constructors::ConstructorBinding> {
        self.constructor_bindings.get_or_init(|| {
            let analyze = || -> Result<std::collections::HashMap<usize, crate::native_constructors::ConstructorBinding>> {
                let analysis = crate::native_method::MethodAnalysis::build(class, method)?;
                let constructors = analysis.constructors()?;
                Ok(constructors.bindings.into_iter().map(|binding| (binding.invoke_pc, binding)).collect())
            };
            analyze().ok()
        }).as_ref()?.get(&invoke_pc)
    }
    fn new(words: &[u16]) -> Result<Self> {
        Self::with_handlers(words, &[])
    }
    fn empty(len: usize) -> Self {
        Self {
            front_end: None,
            shared_cfg: None,
            shared_loops: Vec::new(),
            shared_loop_edges: Vec::new(),
            shared_branches: Vec::new(),
            shared_live: None,
            synchronized: Vec::new(),
            monitor_dispatch: Default::default(),
            protected: Vec::new(),
            protected_loop_escape: Default::default(),
            caught_values: Default::default(),
            catch_rethrows: Default::default(),
            constructor_bindings: std::cell::OnceCell::new(),
            shrink_ssa: std::cell::OnceCell::new(),
            live: None,
            loops: Vec::new(),
            switches: vec![None; len],
            payloads: vec![false; len],
            widths: vec![0; len],
            targets: vec![None; len],
            acyclic_backwards: std::collections::HashSet::new(),
            work: std::cell::Cell::new(0),
        }
    }
    fn straight_line(class: &DexClass, method: &DexMethod) -> Result<Option<Self>> {
        let code = method.code.as_ref().context("method has no code")?;
        if !code.try_regions.is_empty() || code.tries != 0 {
            return Ok(None);
        }
        // Eligibility scans instruction boundaries, never operand words. Shapes
        // with region/allocation-specific lowering remain on the existing path.
        // Once eligible, decoding/binding errors must not retry that path.
        let mut pc = 0;
        let mut condition_count = 0;
        let mut goto_count = 0;
        let mut backward_gotos = 0;
        let mut latch_candidates = Vec::new();
        let mut control = Vec::new();
        while pc < code.instructions.len() {
            let word = code.instructions[pc];
            let op = word as u8;
            if matches!(op, 0x1d..=0x1e | 0x22 | 0x24..=0x26 | 0x2b..=0x2c)
                || matches!(word, 0x0100 | 0x0200 | 0x0300)
            {
                return Ok(None);
            }
            let (width, _) = crate::native_cfg::instruction_width(&code.instructions, pc)?;
            if matches!(op, 0x32..=0x3d) {
                if method.name.as_ref() == "<init>" {
                    return Ok(None);
                }
                if code.instructions[pc + 1] as i16 == 0 {
                    return Ok(None);
                }
                let target = pc as i64 + i64::from(code.instructions[pc + 1] as i16);
                control.push((pc, target));
                condition_count += 1;
            }
            if matches!(op, 0x28..=0x2a) {
                if condition_count == 0 {
                    return Ok(None);
                }
                let delta = match op {
                    0x28 => (word >> 8) as i8 as i64,
                    0x29 => code.instructions[pc + 1] as i16 as i64,
                    _ => {
                        (u32::from(code.instructions[pc + 1])
                            | (u32::from(code.instructions[pc + 2]) << 16))
                            as i32 as i64
                    }
                };
                if delta == 0 {
                    return Ok(None);
                }
                if delta < 0 {
                    backward_gotos += 1;
                    latch_candidates.push((pc, pc as i64 + delta, width));
                }
                goto_count += 1;
                control.push((pc, pc as i64 + delta));
            }
            ensure!(
                condition_count + goto_count <= 1024,
                "shared forward branch budget"
            );
            pc += width;
        }
        let backward_conditions: Vec<_> = control
            .iter()
            .copied()
            .filter(|&(pc, target)| {
                matches!(code.instructions[pc] as u8, 0x32..=0x3d) && target < pc as i64
            })
            .collect();
        let posttest = backward_gotos == 0 && backward_conditions.len() == 1;
        let dual_backedges = backward_gotos == 0
            && backward_conditions.len() == 2
            && backward_conditions[0].1 == backward_conditions[1].1
            && backward_conditions[1].0 + 3 < code.instructions.len()
            && matches!(
                code.instructions[backward_conditions[1].0 + 2] as u8,
                0x0e..=0x11
            );
        if posttest {
            let (latch, header) = backward_conditions[0];
            let exit = latch + 2;
            let terminal_pretest = exit + 2 == code.instructions.len()
                && matches!(code.instructions[exit] as u8, 0x0e..=0x11)
                && matches!(code.instructions[exit + 1] as u8, 0x0e..=0x11)
                && control.iter().any(|&(pc, target)| {
                    pc as i64 >= header
                        && pc < latch
                        && matches!(code.instructions[pc] as u8, 0x32..=0x3d)
                        && target == (exit + 1) as i64
                });
            // The terminal specialization plans at most one prefix return.
            // A chain of closed early-return arms was already supported by
            // the ordinary renderer; leave that larger shape on its route.
            // Malformed prefix edges stay selected and fail canonical proof.
            if terminal_pretest {
                let prefix: Vec<_> = control
                    .iter()
                    .filter(|&&(pc, _)| (pc as i64) < header)
                    .collect();
                if prefix.len() > 1
                    && prefix.iter().all(|&&(pc, target)| {
                        matches!(code.instructions[pc] as u8, 0x32..=0x3d)
                            && target == (pc + 3) as i64
                            && target <= header
                            && code
                                .instructions
                                .get(pc + 2)
                                .is_some_and(|word| matches!(*word as u8, 0x0e..=0x11))
                    })
                {
                    return Ok(None);
                }
            }
            let mut breaks = 0usize;
            if control.iter().any(|&(pc, target)| {
                if pc == latch {
                    return false;
                }
                if terminal_pretest && (pc as i64) < header {
                    return !(target > pc as i64 && target <= header);
                }
                if terminal_pretest && target == (exit + 1) as i64 {
                    return !matches!(code.instructions[pc] as u8, 0x32..=0x3d)
                        || (pc as i64) < header
                        || pc + 2 > latch;
                }
                if target == exit as i64 {
                    breaks += 1;
                    return !matches!(code.instructions[pc] as u8, 0x32..=0x3d)
                        || (pc as i64) < header
                        || pc + 2 > latch;
                }
                !(pc as i64 >= header && pc < latch && target > pc as i64 && target <= latch as i64)
            }) {
                return Ok(None);
            }
            if breaks > 8 {
                return Ok(None);
            }
            // Scan real boundaries, never a branch operand interpreted as an opcode.
            // Malformed selected targets fail before excluded terminal-body routing.
            let mut body_pc = 0;
            let mut boundary = false;
            let mut terminal = false;
            while body_pc < latch {
                boundary |= body_pc as i64 == header;
                terminal |= body_pc as i64 >= header
                    && matches!(code.instructions[body_pc] as u8, 0x0e..=0x11 | 0x27);
                body_pc += crate::native_cfg::instruction_width(&code.instructions, body_pc)?.0;
            }
            ensure!(
                boundary,
                "shared posttest target is not an instruction boundary"
            );
            if terminal {
                return Ok(None);
            }
        } else if dual_backedges {
            if !code.try_regions.is_empty() {
                return Ok(None);
            }
            let start = backward_conditions[0].1;
            let early = backward_conditions[0].0;
            let latch = backward_conditions[1].0;
            let tail = latch + 2;
            let exit = tail + 1;
            // A single prefix return arm followed by this loop belongs to the
            // established renderer. The bounded outer projection below owns
            // only the already-proven chain of closed return arms.
            let prefix: Vec<_> = control
                .iter()
                .filter(|&&(pc, _)| (pc as i64) < start)
                .collect();
            if !prefix.is_empty()
                && (prefix.len() < 2
                    || !prefix.iter().all(|&&(pc, target)| {
                        matches!(code.instructions[pc] as u8, 0x32..=0x3d)
                            && target == (pc + 3) as i64
                            && target <= start
                            && code
                                .instructions
                                .get(pc + 2)
                                .is_some_and(|word| matches!(*word as u8, 0x0e..=0x11))
                    }))
            {
                return Ok(None);
            }
            let mut body_pc = 0;
            while body_pc <= latch {
                if body_pc as i64 >= start
                    && matches!(code.instructions[body_pc] as u8, 0x0e..=0x11 | 0x27)
                {
                    return Ok(None);
                }
                body_pc += crate::native_cfg::instruction_width(&code.instructions, body_pc)?.0;
            }
            let mut breaks = 0;
            let mut terminal_edges = 0;
            if control.iter().any(|&(pc, target)| {
                if pc == early || pc == latch {
                    return false;
                }
                if (pc as i64) < start {
                    return target <= pc as i64 || target > start;
                }
                if pc < latch {
                    if target == exit as i64 {
                        breaks += 1;
                        return false;
                    }
                    if target == tail as i64 {
                        terminal_edges += 1;
                        return false;
                    }
                    return target <= pc as i64 || target > latch as i64;
                }
                target <= pc as i64 || target < exit as i64
            }) || breaks > 2
                || terminal_edges > 1
            {
                return Ok(None);
            }
        } else if backward_gotos == 0 && control.iter().any(|&(pc, target)| target <= pc as i64) {
            return Ok(None);
        }
        if backward_gotos != 0 {
            if condition_count == 0 || latch_candidates.len() > MAX_SHARED_LOOPS {
                return Ok(None);
            }
            latch_candidates.sort_unstable_by_key(|candidate| candidate.1);
            let mut raw_regions: Vec<(i64, usize, usize, usize)> = Vec::new();
            let mut ancestors: Vec<usize> = Vec::new();
            let mut raw_parents = Vec::new();
            for &(latch, header, width) in &latch_candidates {
                let Some(&(guard, target)) = control.iter().find(|&&(pc, _)| {
                    pc as i64 >= header
                        && pc < latch
                        && matches!(code.instructions[pc] as u8, 0x32..=0x3d)
                }) else {
                    return Ok(None);
                };
                let exit = latch + width;
                if header > guard as i64 || guard >= latch || target != exit as i64 {
                    return Ok(None);
                }
                while ancestors
                    .last()
                    .is_some_and(|&parent| raw_regions[parent].3 as i64 <= header)
                {
                    ancestors.pop();
                }
                if let Some(&parent) = ancestors.last() {
                    let previous: (i64, usize, usize, usize) = raw_regions[parent];
                    if header < (previous.1 + 2) as i64 || exit > previous.2 {
                        return Ok(None);
                    }
                }
                if ancestors.len() >= MAX_SHARED_LOOP_DEPTH {
                    return Ok(None);
                }
                raw_parents.push(ancestors.last().map(|&index| raw_regions[index].0));
                ancestors.push(raw_regions.len());
                let mut body_pc = guard + 2;
                while body_pc < latch {
                    if matches!(code.instructions[body_pc] as u8, 0x0e..=0x11 | 0x27) {
                        return Ok(None);
                    }
                    body_pc += crate::native_cfg::instruction_width(&code.instructions, body_pc)?.0;
                }
                raw_regions.push((header, guard, latch, exit));
            }
            if control.iter().any(|&(pc, target)| {
                if let Some(&(header, guard, latch, exit)) = raw_regions
                    .iter()
                    .rev()
                    .find(|&&(header, _, _, exit)| pc as i64 >= header && pc < exit)
                {
                    if pc == guard || pc == latch {
                        return false;
                    }
                    return !(pc > guard
                        && pc < latch
                        && ((target > pc as i64
                            && target <= latch as i64
                            && raw_regions
                                .iter()
                                .rposition(|&(start, _, _, end)| {
                                    target >= start && target < end as i64
                                })
                                .is_some_and(|index| {
                                    raw_regions[index].0 == header
                                        || (target == raw_regions[index].0
                                            && raw_parents[index] == Some(header))
                                }))
                            || (matches!(code.instructions[pc] as u8, 0x32..=0x3d)
                                && (target == header || target == exit as i64))));
                }
                target <= pc as i64
                    || raw_regions
                        .iter()
                        .any(|&(header, _, _, exit)| target > header && target < exit as i64)
            }) {
                return Ok(None);
            }
        }
        let front_end = crate::native_method::MethodFrontEnd::build(class, method)
            .map_err(|error| anyhow::anyhow!("shared front end: {error:#}"))?;
        let mut graph = Self::empty(code.instructions.len());
        if condition_count != 0 {
            let cfg = if backward_gotos == 0 && !posttest && !dual_backedges {
                crate::native_cfg::ControlFlowGraph::from_decoded(
                    &front_end.ir,
                    code.instructions.len(),
                )?
            } else {
                crate::native_cfg::ControlFlowGraph::from_decoded_loop(
                    &front_end.ir,
                    code.instructions.len(),
                )?
            };
            if backward_gotos == 0 && !posttest && !dual_backedges {
                graph.shared_branches =
                    decoded_branch_plans(&front_end.ir, &cfg, code.instructions.len())?;
            } else {
                let regions = decoded_natural_loops(&front_end.ir, &cfg)?;
                graph.shared_branches = decoded_loop_branch_plans(
                    &front_end.ir,
                    &cfg,
                    &regions,
                    code.instructions.len(),
                )?;
                graph.shared_loop_edges = decoded_all_loop_edges(&front_end.ir, &regions)?;
                graph.loops = regions.clone();
                graph.shared_loops = regions;
            }
            for instruction in &front_end.ir.instructions {
                if matches!(instruction.opcode, 0x32..=0x3d) {
                    decoded_condition_sources(instruction, usize::from(code.registers))?;
                }
                if matches!(instruction.opcode, 0x28..=0x2a) {
                    decoded_goto(instruction)?;
                }
                graph.targets[instruction.pc] = instruction.branch_target;
            }
            graph.shared_live =
                decoded_cfg_liveness(&front_end.ir, &cfg, usize::from(code.registers))?;
            graph.shared_cfg = Some(cfg);
        }
        for instruction in &front_end.ir.instructions {
            ensure!(
                condition_count != 0
                    || !matches!(instruction.opcode, 0x0e..=0x11 | 0x27)
                    || instruction.pc + instruction.width == code.instructions.len(),
                "unreachable instruction region not reconstructed"
            );
            graph.widths[instruction.pc] = instruction.width;
        }
        graph.front_end = Some(front_end);
        Ok(Some(graph))
    }
    fn carry_loop_values(
        &self,
        slots: &[Option<Value>],
        values: &[Option<Value>],
        out: &mut Output,
    ) -> Result<()> {
        carry_loop_values_impl(slots, values, out, !self.shared_loops.is_empty())
    }
    fn target(&self, pc: usize) -> Result<Option<usize>> {
        if let Some(instruction) = self.instruction(pc)? {
            Ok(instruction.branch_target)
        } else {
            Ok(self.targets[pc])
        }
    }
    fn written_in(
        &self,
        code: &crate::native_dex::DexCode,
        start: usize,
        end: usize,
    ) -> Option<Vec<bool>> {
        let Some(front) = &self.front_end else {
            return super::liveness::written_in(code, start, end);
        };
        let mut written = vec![false; usize::from(code.registers)];
        for instruction in front
            .ir
            .instructions
            .iter()
            .skip_while(|instruction| instruction.pc < start)
            .take_while(|instruction| instruction.pc < end)
        {
            for operand in &instruction.writes {
                let start = usize::from(operand.register);
                written
                    .get_mut(start..start + operand.kind.word_count())?
                    .fill(true);
            }
        }
        Some(written)
    }
    fn test(
        &self,
        words: &[u16],
        pc: usize,
        inverse: bool,
        regs: &[Option<Value>],
    ) -> Result<String> {
        if let Some(instruction) = self.instruction(pc)? {
            let (left, right) = decoded_condition_sources(instruction, regs.len())?;
            condition_sources(instruction.opcode ^ u8::from(inverse), left, right, regs)
        } else {
            condition(
                (words[pc] as u8) ^ u8::from(inverse),
                (words[pc] >> 8) as usize,
                regs,
            )
        }
    }
    fn shared_loop_edge(&self, pc: usize) -> Option<SharedLoopEdge> {
        shared_loop_edge_at(&self.shared_loop_edges, pc)
    }
    #[cfg(test)]
    fn edge_kind(&self, kind: SharedLoopEdgeKind) -> Option<SharedLoopEdge> {
        self.shared_loop_edges
            .iter()
            .find(|edge| edge.kind == kind)
            .copied()
    }
    #[cfg(test)]
    fn edge_kind_mut(&mut self, kind: SharedLoopEdgeKind) -> Option<&mut SharedLoopEdge> {
        self.shared_loop_edges
            .iter_mut()
            .find(|edge| edge.kind == kind)
    }
    fn shared_branch(&self, pc: usize) -> Result<Option<SharedBranch>> {
        if self.shared_cfg.is_none() {
            return Ok(None);
        }
        let index = self
            .shared_branches
            .binary_search_by_key(&pc, |plan| plan.branch)
            .map_err(|_| anyhow::anyhow!("missing canonical shared branch plan"))?;
        Ok(Some(self.shared_branches[index]))
    }
    fn instruction(&self, pc: usize) -> Result<Option<&crate::native_ir::Instruction>> {
        self.front_end
            .as_ref()
            .map(|front| {
                front
                    .ir
                    .instructions
                    .binary_search_by_key(&pc, |insn| insn.pc)
                    .map(|index| &front.ir.instructions[index])
                    .map_err(|_| {
                        anyhow::anyhow!("render offset is not a shared instruction boundary: {pc}")
                    })
            })
            .transpose()
    }
    fn with_handlers(words: &[u16], handlers: &[usize]) -> Result<Self> {
        let mut graph = Self::empty(words.len());
        let mut pc = 0;
        let mut branches = 0;
        let mut payload_starts = Vec::new();
        while pc < words.len() {
            let op = words[pc] as u8;
            if words[pc] == 0x0300 {
                ensure!(pc.is_multiple_of(2), "unaligned array payload");
                let width = operations::array_payload(words, pc)?.2;
                graph.payloads[pc..pc + width].fill(true);
                payload_starts.push(pc);
                pc += width;
                continue;
            }
            if matches!(words[pc], 0x0100 | 0x0200) {
                ensure!(pc.is_multiple_of(2), "unaligned switch payload");
                let count = *words.get(pc + 1).context("truncated switch payload")? as usize;
                ensure!(count <= 1024, "switch exceeds case budget");
                let width = if words[pc] == 0x0100 {
                    4 + count * 2
                } else {
                    2 + count * 4
                };
                ensure!(pc + width <= words.len(), "truncated switch payload");
                graph.payloads[pc..pc + width].fill(true);
                payload_starts.push(pc);
                pc += width;
                continue;
            }
            let width = match op {
                0x18 => 5,
                0x00
                | 0x01
                | 0x04
                | 0x07
                | 0x0a..=0x12
                | 0x1d..=0x1e
                | 0x21
                | 0x27
                | 0x28
                | 0x7b..=0x8f
                | 0xb0..=0xcf => 1,
                0x02
                | 0x05
                | 0x08
                | 0x13
                | 0x15
                | 0x16
                | 0x19
                | 0x1a
                | 0x1c
                | 0x1f
                | 0x20
                | 0x22
                | 0x23
                | 0x44..=0x51
                | 0x29
                | 0x32..=0x3d
                | 0x52..=0x6d
                | 0x2d..=0x31
                | 0x90..=0xaf
                | 0xd0..=0xe2 => 2,
                0x03
                | 0x06
                | 0x09
                | 0x14
                | 0x17
                | 0x1b
                | 0x24
                | 0x25
                | 0x26
                | 0x2a
                | 0x2b
                | 0x2c
                | 0x6e..=0x72
                | 0x74..=0x78 => 3,
                _ => bail!("unsupported opcode 0x{op:02x} at {pc}"),
            };
            ensure!(pc + width <= words.len(), "truncated instruction");
            ensure!(op != 0 || words[pc] == 0, "payload in structured body");
            graph.widths[pc] = width;
            let offset = match op {
                0x28 => Some((words[pc] >> 8) as i8 as i64),
                0x29 | 0x32..=0x3d => Some(words[pc + 1] as i16 as i64),
                0x2a => Some((words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32 as i64),
                _ => None,
            };
            if let Some(offset) = offset {
                branches += 1;
                ensure!(branches <= 1024, "control flow exceeds branch budget");
                ensure!(offset != 0, "self branch not reconstructed");
                let target = pc as i64 + offset;
                ensure!(
                    target >= 0 && target < words.len() as i64,
                    "branch target outside method"
                );
                graph.targets[pc] = Some(target as usize);
            }
            pc += width;
        }
        let read_i32 = |pos: usize| -> Result<i32> {
            let lo = *words.get(pos).context("truncated switch word")? as u32;
            let hi = *words.get(pos + 1).context("truncated switch word")? as u32;
            Ok((lo | (hi << 16)) as i32)
        };
        let mut used_payloads = std::collections::HashSet::new();
        for pc in 0..words.len() {
            if graph.widths[pc] == 0 || !matches!(words[pc] as u8, 0x26 | 0x2b | 0x2c) {
                continue;
            }
            let payload = pc as i64 + i64::from(read_i32(pc + 1)?);
            ensure!(
                payload >= 0 && payload < words.len() as i64,
                "switch payload offset outside method"
            );
            let payload = payload as usize;
            ensure!(
                payload_starts.contains(&payload),
                "switch offset is not a payload boundary"
            );
            if words[pc] as u8 == 0x26 {
                ensure!(words[payload] == 0x0300, "array payload type mismatch");
                used_payloads.insert(payload);
                continue;
            }
            let packed = words[pc] as u8 == 0x2b;
            ensure!(
                words[payload] == if packed { 0x0100 } else { 0x0200 },
                "switch payload type mismatch"
            );
            used_payloads.insert(payload);
            let count = words[payload + 1] as usize;
            branches += count + 1;
            ensure!(branches <= 1024, "control flow exceeds branch budget");
            let first = if packed { read_i32(payload + 2)? } else { 0 };
            let mut cases = Vec::with_capacity(count);
            for i in 0..count {
                let key = if packed {
                    first
                        .checked_add(i as i32)
                        .context("packed switch key overflow")?
                } else {
                    read_i32(payload + 2 + i * 2)?
                };
                ensure!(
                    cases.last().is_none_or(|(previous, _)| *previous < key),
                    "switch keys are not strictly ordered"
                );
                let target_pos = if packed {
                    payload + 4 + i * 2
                } else {
                    payload + 2 + count * 2 + i * 2
                };
                let target = pc as i64 + i64::from(read_i32(target_pos)?);
                ensure!(
                    target > pc as i64 && target < words.len() as i64,
                    "nonforward or invalid switch target"
                );
                let target = target as usize;
                ensure!(
                    graph.widths[target] != 0,
                    "switch target is not executable instruction boundary"
                );
                cases.push((key, target));
            }
            graph.switches[pc] = Some(Switch { cases });
        }
        ensure!(
            used_payloads.len() == payload_starts.len(),
            "unreferenced switch payload"
        );
        for target in graph.targets.iter().flatten() {
            ensure!(
                graph.widths[*target] != 0,
                "branch target is not an instruction boundary"
            );
        }
        let mut reachable = graph.reachable(0, words.len(), words)?;
        for handler in handlers {
            ensure!(
                *handler < words.len() && graph.widths[*handler] != 0,
                "handler is not an instruction boundary"
            );
            let seen = graph.reachable(*handler, words.len(), words)?;
            for (visible, extra) in reachable.iter_mut().zip(seen) {
                *visible |= extra;
            }
        }
        ensure!(
            graph
                .widths
                .iter()
                .enumerate()
                .all(|(pc, width)| *width == 0
                    || words[pc] as u8 != 0x0d
                    || handlers.contains(&pc)),
            "move-exception outside handler entry"
        );
        ensure!(
            graph
                .widths
                .iter()
                .enumerate()
                .all(|(pc, width)| *width == 0
                    || reachable[pc]
                    || (words[pc] == 0
                        && !pc.is_multiple_of(2)
                        && payload_starts.contains(&(pc + 1)))),
            "unreachable instruction region not reconstructed"
        );
        // Address order is not execution order: optimized DEX can place a
        // shared tail before one of its predecessors. Only cyclic edges can
        // form a loop. Classify before loop summaries alter reachability.
        for (from, target) in graph.targets.iter().enumerate() {
            if let Some(to) = *target
                && to < from
                && !graph.reachable_in_suffix(to, from, words)?
            {
                graph.acyclic_backwards.insert(from);
            }
        }
        // Multiple backedges to one header are continues of the same natural
        // loop. The final address-order latch bounds its straight-line region.
        let mut latches = std::collections::BTreeMap::new();
        for (from, target) in graph.targets.iter().enumerate() {
            if let Some(start) = *target
                && start < from
                && !graph.acyclic_backwards.contains(&from)
            {
                latches.insert(start, from);
            }
        }
        let loop_bounds = latches.clone();
        for (start, latch) in latches {
            let mut exit = latch + graph.widths[latch];
            let mut tail = None;
            ensure!(
                exit < words.len() || matches!(words[latch] as u8, 0x28..=0x2a),
                "conditional loop falls beyond method"
            );
            let guard = if matches!(words[latch] as u8, 0x28..=0x2a) {
                let mut pos = start;
                while pos < latch
                    && graph.targets[pos].is_none()
                    && !matches!(words[pos] as u8, 0x0e..=0x11 | 0x27)
                {
                    pos += graph.widths[pos];
                }
                if pos < latch
                    && matches!(words[pos] as u8, 0x32..=0x3d)
                    && graph.targets[pos].is_some_and(|target| target >= exit)
                {
                    exit = graph.targets[pos].unwrap();
                    Some(pos)
                } else {
                    // A while loop can test its exit after a branch or part of
                    // its body. Preserve instruction order and emit explicit
                    // breaks rather than moving that test to the header.
                    if exit != words.len() && !graph.targets[start..latch].contains(&Some(exit)) {
                        let exits: std::collections::BTreeSet<_> = graph.targets[start..latch]
                            .iter()
                            .flatten()
                            .copied()
                            .filter(|target| *target > latch)
                            .collect();
                        ensure!(exits.len() == 1, "loop has no structured exit");
                        exit = *exits.first().unwrap();
                    }
                    None
                }
            } else {
                ensure!(
                    matches!(words[latch] as u8, 0x32..=0x3d),
                    "unsupported loop latch"
                );
                let mut pos = start;
                while pos < latch
                    && graph.targets[pos].is_none()
                    && !matches!(words[pos] as u8, 0x0e..=0x11 | 0x27)
                {
                    pos += graph.widths[pos];
                }
                if pos < latch
                    && matches!(words[pos] as u8, 0x32..=0x3d)
                    && let Some(join) = graph.targets[pos]
                    && join > exit
                {
                    // Optimized search loops often jump over a null/default
                    // assignment on the successful exit. Keep both exit paths
                    // in the loop and merge at that forward jump's destination.
                    let mut common = join;
                    if matches!(words[exit] as u8, 0x28..=0x2a)
                        && let Some(target) = graph.targets[exit]
                        && target > join
                    {
                        let mut cursor = join;
                        while cursor < target
                            && graph.targets[cursor].is_none()
                            && graph.switches[cursor].is_none()
                            && !matches!(words[cursor] as u8, 0x0e..=0x11 | 0x27)
                        {
                            cursor += graph.widths[cursor];
                        }
                        if cursor == target {
                            common = target;
                        }
                    }
                    // Shared success tails are not loop-owned: an independent
                    // predecessor may jump directly to the latch fallthrough.
                    // Keep the canonical loop interval in that case; its guard
                    // is rendered as a proven escape without absorbing effects
                    // that also execute when the loop is bypassed.
                    let shared_tail =
                        graph.targets.iter().enumerate().any(|(from, target)| {
                            !(start..common).contains(&from)
                                && target.is_some_and(|to| {
                                    (exit..common).contains(&to)
                                        && !graph.pure_exit_tail(to, common, words)
                                })
                        }) || graph.switches.iter().enumerate().any(|(from, switch)| {
                            !(start..common).contains(&from)
                                && switch.as_ref().is_some_and(|switch| {
                                    switch.cases.iter().any(|(_, to)| {
                                        (exit..common).contains(to)
                                            && !graph.pure_exit_tail(*to, common, words)
                                    })
                                })
                        });
                    if shared_tail {
                        None
                    } else {
                        tail = Some(exit);
                        exit = common;
                        Some(pos)
                    }
                } else {
                    None
                }
            };
            ensure!(
                graph.loops.iter().all(|l| (if tail.is_some() {
                    exit
                } else {
                    latch + graph.widths[latch]
                }) <= l.start
                    || start >= l.body_end(&graph.widths)
                    || (start > l.start && exit <= l.latch)),
                "overlapping loops not reconstructed"
            );
            graph.loops.push(Loop {
                parent: None,
                start,
                latch,
                guard,
                exit,
                tail,
            });
        }
        // Discover all loop regions before validating exits. A shared exit may
        // reach a later independent loop; address order does not establish CFG
        // membership. Every region still undergoes the same entry/edge checks.
        for region in graph.loops.clone() {
            let Loop {
                parent: _,
                start,
                latch,
                guard,
                exit,
                tail,
            } = region;
            for (from, target) in graph.targets.iter().enumerate() {
                graph.tick()?;
                let Some(to) = *target else { continue };
                if from == latch || Some(from) == guard {
                    continue;
                }
                if (start..latch).contains(&from) && to == start {
                    // A conditional or unconditional continue, carrying the
                    // current frame back to the header.
                    continue;
                }
                let body_end = if tail.is_some() {
                    exit
                } else {
                    latch + graph.widths[latch]
                };
                if (start..body_end).contains(&from) {
                    // A nested natural loop owns its own backedges. Its full
                    // entry/exit checks run when that region is classified.
                    if loop_bounds.iter().any(|(&inner_start, &inner_latch)| {
                        inner_start > start
                            && inner_latch < latch
                            && to == inner_start
                            && (inner_start + 1..=inner_latch).contains(&from)
                    }) {
                        continue;
                    }
                    // A shared bare return after the loop is a method exit,
                    // not a second loop continuation. It can be emitted in place.
                    if to == exit
                        || (!(start..body_end).contains(&to)
                            && graph.non_reentering_tail(to, start..body_end, words)?)
                    {
                        continue;
                    }
                    let limit = if tail.is_some_and(|tail| from >= tail) {
                        exit
                    } else {
                        latch
                    };
                    ensure!(
                        (to > from || graph.acyclic_backwards.contains(&from))
                            && (start..=limit).contains(&to),
                        "unsupported loop interior edge"
                    );
                } else {
                    ensure!(
                        to <= start
                            || to >= body_end
                            || tail.is_some_and(
                                |tail| to >= tail && graph.pure_exit_tail(to, exit, words)
                            ),
                        "loop has an interior entry"
                    );
                }
            }
        }

        ensure!(
            graph.switches.iter().enumerate().all(|(pc, switch)| {
                switch.as_ref().is_none_or(|switch| {
                    graph.loops.iter().all(|region| {
                        if (region.start..region.body_end(&graph.widths)).contains(&pc) {
                            // A switch wholly contained in the body can use its
                            // ordinary Java break. Loop-targeting switch arms need
                            // explicit labeled exits and remain unsupported.
                            switch
                                .cases
                                .iter()
                                .all(|(_, target)| (region.start..=region.latch).contains(target))
                        } else {
                            switch.cases.iter().all(|(_, target)| {
                                !(region.start + 1..region.body_end(&graph.widths)).contains(target)
                            })
                        }
                    })
                })
            }),
            "switch crosses loop boundary"
        );
        Ok(graph)
    }
    fn live_at(&self, pc: usize, register: usize) -> bool {
        if let Some(cfg) = &self.shared_cfg {
            if pc == self.widths.len() {
                return false;
            }
            let Some(live) = &self.shared_live else {
                return true;
            };
            if register >= live.registers {
                return true;
            }
            let Some(&block_index) = cfg.block_at.get(&pc) else {
                return true;
            };
            if cfg.blocks[block_index].start != pc {
                return true;
            }
            return live.bits[block_index * live.stride + register / 64] & (1 << (register % 64))
                != 0;
        }
        self.live
            .as_ref()
            .is_none_or(|live| live.contains(pc, register))
    }
    fn tick(&self) -> Result<()> {
        let n = self.work.get() + 1;
        ensure!(n <= 1_000_000, "control flow analysis exceeds work budget");
        self.work.set(n);
        Ok(())
    }
    // An address-order backedge to a shared tail inside an outer loop is not
    // itself a loop. A cycle for this candidate must close without traversing
    // instructions before its proposed header. Outer-loop reentry belongs to
    // the outer region and must not create a spurious nested loop.
    fn reachable_in_suffix(&self, start: usize, target: usize, words: &[u16]) -> Result<bool> {
        let mut seen = vec![false; words.len()];
        let mut pending = vec![start];
        while let Some(pc) = pending.pop() {
            self.tick()?;
            if pc < start || pc >= words.len() || seen[pc] {
                continue;
            }
            if pc == target {
                return Ok(true);
            }
            seen[pc] = true;
            ensure!(
                self.widths[pc] != 0 && !self.payloads[pc],
                "invalid suffix control flow"
            );
            let op = words[pc] as u8;
            if matches!(op, 0x0e..=0x11 | 0x27) {
                continue;
            }
            if let Some(switch) = &self.switches[pc] {
                pending.extend(switch.cases.iter().map(|(_, pc)| *pc));
            }
            if let Some(pc) = self.targets[pc] {
                pending.push(pc);
            }
            if !matches!(op, 0x28..=0x2a) {
                pending.push(pc + self.widths[pc]);
            }
        }
        Ok(false)
    }
    // A shared escape region may physically precede the loop and may contain
    // later independent loops. Follow every normal CFG edge to prove no reentry.
    // Paths may terminate or remain in a separately validated loop; this does
    // not prove runtime termination. Protected instructions cannot be cloned
    // here: their exception ownership needs a separate region proof.
    fn non_reentering_tail(
        &self,
        start: usize,
        excluded: std::ops::Range<usize>,
        words: &[u16],
    ) -> Result<bool> {
        let mut colors = vec![0u8; words.len()];
        let mut pending = vec![(start, false)];
        while let Some((pc, finish)) = pending.pop() {
            self.tick()?;
            if excluded.contains(&pc)
                || self.protected.iter().any(|region| region.contains(&pc))
                || pc >= words.len()
                || self.widths[pc] == 0
                || self.payloads[pc]
            {
                return Ok(false);
            }
            if finish {
                colors[pc] = 2;
                continue;
            }
            if colors[pc] == 1 {
                // A recognized independent loop can run on the escape path.
                // Traverse its actual edges (not just its ordinary exit), so a
                // secondary exit back into the excluded loop is still rejected.
                if self.loops.iter().any(|region| {
                    region.start == pc
                        && (region.body_end(&self.widths) <= excluded.start
                            || region.start >= excluded.end)
                }) {
                    continue;
                }
                return Ok(false);
            }
            if colors[pc] == 2 {
                continue;
            }
            colors[pc] = 1;
            pending.push((pc, true));
            let op = words[pc] as u8;
            if matches!(op, 0x0e..=0x11 | 0x27) {
                continue;
            }
            if let Some(switch) = &self.switches[pc] {
                pending.extend(switch.cases.iter().map(|(_, target)| (*target, false)));
            }
            if let Some(target) = self.targets[pc] {
                pending.push((target, false));
            }
            if !matches!(op, 0x28..=0x2a) {
                pending.push((pc + self.widths[pc], false));
            }
        }
        Ok(true)
    }
    /// A shared default assignment may be copied at an external entry to a
    /// loop's exit tail. Only straight-line, nonthrowing DEX moves/constants
    /// are safe to duplicate across those owners.
    fn pure_exit_tail(&self, start: usize, join: usize, words: &[u16]) -> bool {
        if start >= join {
            return false;
        }
        let mut pc = start;
        while pc < join {
            if pc >= words.len()
                || self.widths[pc] == 0
                || self.payloads[pc]
                || self.targets[pc].is_some()
                || self.switches[pc].is_some()
                || !matches!(words[pc] as u8, 0x00..=0x09 | 0x12..=0x19)
            {
                return false;
            }
            pc += self.widths[pc];
        }
        pc == join
    }
    fn terminal_loop_escape(&self, pc: usize, stop: usize, words: &[u16]) -> bool {
        self.loops.iter().any(|region| {
            region.start <= stop
                && stop <= region.latch
                && (pc == region.exit
                    || (!(region.start..region.body_end(&self.widths)).contains(&pc)
                        && self
                            .non_reentering_tail(
                                pc,
                                region.start..region.body_end(&self.widths),
                                words,
                            )
                            .unwrap_or(false)))
        })
    }

    fn protected_escape(
        &self,
        pc: usize,
        excluded: std::ops::Range<usize>,
        code: &crate::native_dex::DexCode,
    ) -> Result<Option<ProtectedLoopEscape>> {
        let Some(context) = self.protected_loop_escape.borrow().clone() else {
            return Ok(None);
        };
        if excluded.contains(&pc) || pc < context.start || pc >= context.end {
            return Ok(None);
        }
        let words = &code.instructions;
        let mut seen = std::collections::HashSet::new();
        let mut pending = vec![pc];
        while let Some(pc) = pending.pop() {
            self.tick()?;
            if pc == context.end {
                continue;
            }
            if excluded.contains(&pc)
                || pc < context.start
                || pc >= context.end
                || self.widths[pc] == 0
                || self.payloads[pc]
            {
                return Ok(None);
            }
            if !seen.insert(pc) {
                continue;
            }
            let Some(writes) = self.written_in(code, pc, pc + self.widths[pc]) else {
                return Ok(None);
            };
            if writes
                .iter()
                .zip(&context.forbidden_writes)
                .any(|(write, forbidden)| *write && *forbidden)
            {
                return Ok(None);
            }
            let op = words[pc] as u8;
            if matches!(op, 0x0d..=0x11 | 0x27) {
                return Ok(None);
            }
            let mut successors = Vec::new();
            if let Some(target) = self.targets[pc] {
                successors.push(target);
            }
            if let Some(switch) = &self.switches[pc] {
                successors.extend(switch.cases.iter().map(|(_, target)| *target));
            }
            if !matches!(op, 0x28..=0x2a) {
                successors.push(pc + self.widths[pc]);
            }
            if successors.iter().any(|target| *target <= pc) {
                return Ok(None);
            }
            pending.extend(successors);
        }
        Ok(Some(context))
    }

    fn reachable(&self, start: usize, stop: usize, words: &[u16]) -> Result<Vec<bool>> {
        let mut seen = vec![false; self.widths.len() + 1];
        let mut pending = vec![start];
        while let Some(pc) = pending.pop() {
            self.tick()?;
            if self.terminal_loop_escape(pc, stop, words) {
                continue;
            }
            ensure!(pc <= stop, "control flow crosses region boundary");
            if seen[pc] {
                continue;
            }
            seen[pc] = true;
            if pc == stop {
                continue;
            }
            ensure!(
                self.widths[pc] != 0 && !self.payloads[pc],
                "control flow enters payload or instruction interior"
            );
            if let Some(region) = self.loops.iter().find(|l| l.start == pc) {
                pending.push(region.exit);
                continue;
            }
            let op = words[pc] as u8;
            if matches!(op, 0x0e..=0x11 | 0x27) {
                continue;
            }
            if let Some(switch) = &self.switches[pc] {
                pending.extend(switch.cases.iter().map(|(_, target)| *target));
            }
            if let Some(target) = self.targets[pc] {
                pending.push(target);
            }
            if !matches!(op, 0x28..=0x2a) {
                pending.push(pc + self.widths[pc]);
            }
        }
        Ok(seen)
    }
    fn join(&self, a: usize, b: usize, stop: usize, words: &[u16]) -> Result<usize> {
        // A node reached from both entries is not necessarily their shared tail:
        // either entry may have another edge which bypasses it.  Such a node would
        // split a branch region before that edge, and rendering the edge then
        // crosses the artificial region boundary.  Pick the first instruction
        // that contains every nonterminal path from both entries instead.
        let mut pc = a.max(b);
        while pc <= stop {
            ensure!(
                pc == stop || self.widths[pc] != 0,
                "join is not an instruction boundary"
            );
            if let Some(escape) = self.region_is_closed(a, pc, words)? {
                pc = escape;
                continue;
            }
            if let Some(escape) = self.region_is_closed(b, pc, words)? {
                pc = escape;
                continue;
            }
            return Ok(pc);
        }
        bail!("control flow crosses region boundary")
    }
    /// Returns an instruction reached beyond `stop`, or `None` when every path
    /// stays within the region until it reaches `stop` or terminates. An escape
    /// also proves every candidate before that instruction invalid, allowing
    /// join discovery to jump over the whole range rather than rescan it.
    fn region_is_closed(&self, start: usize, stop: usize, words: &[u16]) -> Result<Option<usize>> {
        let mut seen = vec![false; self.widths.len() + 1];
        let mut pending = vec![start];
        while let Some(pc) = pending.pop() {
            self.tick()?;
            if self.terminal_loop_escape(pc, stop, words) {
                continue;
            }
            if pc > stop {
                return Ok(Some(pc));
            }
            if seen[pc] {
                continue;
            }
            seen[pc] = true;
            if pc == stop {
                continue;
            }
            ensure!(
                self.widths[pc] != 0 && !self.payloads[pc],
                "control flow enters payload or instruction interior"
            );
            if let Some(region) = self.loops.iter().find(|l| l.start == pc) {
                pending.push(region.exit);
                continue;
            }
            let op = words[pc] as u8;
            if matches!(op, 0x0e..=0x11 | 0x27) {
                continue;
            }
            if let Some(switch) = &self.switches[pc] {
                pending.extend(switch.cases.iter().map(|(_, target)| *target));
            }
            if let Some(target) = self.targets[pc]
                && !self.loops.iter().any(|region| {
                    region.start == target && (region.start + 1..=region.latch).contains(&pc)
                })
            {
                pending.push(target);
            }
            if !matches!(op, 0x28..=0x2a) {
                pending.push(pc + self.widths[pc]);
            }
        }
        Ok(None)
    }
}
fn merge_type(left: Option<&Value>, right: Option<&Value>) -> Result<String> {
    let Some(left) = left else {
        return Ok(right.context("empty merge")?.ty.clone());
    };
    let Some(right) = right else {
        return Ok(left.ty.clone());
    };
    if left.ty == "I"
        && right.ty == "I"
        && matches!(left.literal, Some(0 | 1))
        && matches!(right.literal, Some(0 | 1))
    {
        return Ok("Z".into());
    }
    if left.ty == right.ty {
        return Ok(left.ty.clone());
    }
    if matches!(left.ty.as_str(), "I" | "B" | "S" | "C")
        && matches!(right.ty.as_str(), "I" | "B" | "S" | "C")
    {
        return Ok("I".into());
    }
    for (typed, literal) in [(left, right), (right, left)] {
        if (reference(&typed.ty) && literal.literal == Some(0))
            || (typed.ty == "Z" && matches!(literal.literal, Some(0 | 1)))
            || (typed.ty == "F"
                && matches!(literal.ty.as_str(), "I" | "Z")
                && (literal.literal.is_some() || literal.raw_bits32))
            || (typed.ty == "D" && literal.ty == "J" && literal.wide_literal.is_some())
        {
            return Ok(typed.ty.clone());
        }
    }
    if (left.ty == "Z" && matches!(right.ty.as_str(), "I" | "B" | "S" | "C"))
        || (right.ty == "Z" && matches!(left.ty.as_str(), "I" | "B" | "S" | "C"))
    {
        return Ok("I".into());
    }
    if reference(&left.ty) && reference(&right.ty) {
        return Ok("Ljava/lang/Object;".into());
    }
    bail!("incompatible register types at control flow join")
}
fn condition(op: u8, a: usize, regs: &[Option<Value>]) -> Result<String> {
    let zero = op >= 0x38;
    condition_sources(
        op,
        if zero { a } else { a & 15 },
        if zero { None } else { Some(a >> 4) },
        regs,
    )
}
fn condition_sources(
    op: u8,
    left: usize,
    right: Option<usize>,
    regs: &[Option<Value>],
) -> Result<String> {
    let zero = op >= 0x38;
    let kind = if zero { op - 0x38 } else { op - 0x32 };
    let lhs = register(regs, left)?;
    let rhs = if zero {
        Value {
            text: "0".into(),
            ty: "I".into(),
            literal: Some(0),
            wide_literal: None,
            raw_bits32: false,
        }
    } else {
        register(regs, right.context("missing comparison right operand")?)?
    };
    let symbol = ["==", "!=", "<", ">=", ">", "<="][kind as usize];
    let (left, right) = if reference(&lhs.ty) || reference(&rhs.ty) {
        ensure!(kind <= 1, "ordered reference comparison");
        let expr = |v: &Value| -> Result<String> {
            if v.literal == Some(0) {
                Ok("null".into())
            } else {
                ensure!(reference(&v.ty), "reference comparison with nonreference");
                if lhs.literal == Some(0) || rhs.literal == Some(0) {
                    Ok(v.text.clone())
                } else {
                    Ok(format!("((java.lang.Object) {})", v.text))
                }
            }
        };
        (expr(&lhs)?, expr(&rhs)?)
    } else if (lhs.ty == "Z" || rhs.ty == "Z") && kind <= 1 {
        ensure!(kind <= 1, "ordered boolean comparison");
        let left = argument(&lhs, "Z")?;
        let right = argument(&rhs, "Z")?;
        for (expression, constant) in [(&left, &right), (&right, &left)] {
            if matches!(constant.as_str(), "true" | "false") {
                return Ok(if (constant == "true") == (kind == 0) {
                    expression.clone()
                } else {
                    format!("!({expression})")
                });
            }
        }
        (left, right)
    } else {
        (integral(&lhs)?, integral(&rhs)?)
    };
    Ok(format!("{left} {symbol} {right}"))
}

pub(super) fn reconstruct(
    _class_name: &str,
    class: &DexClass,
    method: &DexMethod,
) -> Result<MethodBody> {
    let code = method.code.as_ref().context("method has no code")?;
    ensure!(
        usize::from(code.tries) == code.try_regions.len(),
        "nested or multiple try regions not reconstructed"
    );
    ensure!(
        method.name.as_ref() != "<init>"
            || code
                .try_regions
                .iter()
                .all(|region| region.catches.iter().all(|(ty, _)| ty.is_some())),
        "constructor cleanup regions not reconstructed"
    );
    ensure!(
        code.instructions.len() <= 65_536,
        "method exceeds instruction budget"
    );
    // A supported monitor region must start immediately after its one-word monitor-enter.
    // This prefilter only selects candidates; the region analyzer proves them.
    let monitor_candidate = code.try_regions.iter().any(|region| {
        (region.start as usize)
            .checked_sub(1)
            .and_then(|pc| code.instructions.get(pc))
            .is_some_and(|word| *word as u8 == 0x1d)
    });

    ensure!(code.ins <= code.registers, "invalid incoming registers");
    let mut regs = vec![None; code.registers as usize];
    let mut r = (code.registers - code.ins) as usize;
    let constructor = method.name.as_ref() == "<init>";
    if method.access_flags & 8 == 0 {
        assign(
            &mut regs,
            r,
            Value {
                text: "this".into(),
                ty: class.descriptor.to_string(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            },
        )?;
        r += 1;
    }
    for (i, ty) in method.parameters.iter().enumerate() {
        java_type(ty)?;
        assign(
            &mut regs,
            r,
            Value {
                text: format!("p{i}"),
                ty: ty.to_string(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            },
        )?;
        r += if matches!(ty.as_ref(), "J" | "D") {
            2
        } else {
            1
        };
    }
    ensure!(r == regs.len(), "parameter register count mismatch");
    // Disjoint typed and catch-all regions can use the ordinary renderer;
    // it proves each protected range, handler and continuation independently.
    // Prefer finally only when its duplicate-cleanup proof succeeds.
    let separate_regions = code.try_regions.len() <= 16
        && code
            .try_regions
            .windows(2)
            .all(|pair| pair[0].end <= pair[1].start);
    if code.try_regions.len() > 1
        && !monitor_candidate
        && code
            .try_regions
            .iter()
            .any(|r| r.catches.iter().any(|(ty, _)| ty.is_none()))
    {
        match finally_regions::reconstruct(class, method, regs.clone()) {
            Ok(body) => return Ok(body),
            Err(error) if !separate_regions => return Err(error),
            Err(_) => {}
        }
    }
    let mut out = Output::default();
    let handlers: Vec<_> = code
        .try_regions
        .iter()
        .flat_map(|region| region.catches.iter().map(|(_, pc)| *pc as usize))
        .collect();
    let mut graph = if let Some(graph) = Graph::straight_line(class, method)? {
        graph
    } else if handlers.is_empty() {
        Graph::new(&code.instructions)?
    } else {
        Graph::with_handlers(&code.instructions, &handlers)?
    };
    graph.protected = code
        .try_regions
        .iter()
        .map(|region| region.start as usize..region.end as usize)
        .collect();
    if monitor_candidate {
        graph.synchronized = synchronized::analyze(code, &graph)?;
    }
    let monitor_try = |index: usize| {
        graph
            .synchronized
            .iter()
            .any(|region| region.tries.contains(&index))
    };
    let ordinary_tries = code
        .try_regions
        .iter()
        .enumerate()
        .filter(|(index, _)| !monitor_try(*index))
        .count();
    ensure!(
        ordinary_tries <= 1 || separate_regions,
        "nested or multiple try regions not reconstructed"
    );
    ensure!(
        graph.synchronized.is_empty()
            || code
                .try_regions
                .iter()
                .enumerate()
                .all(|(index, region)| {
                    monitor_try(index)
                        || graph.synchronized.iter().any(|r| r.inner_tries.contains(&index))
                        // A leading typed try and its terminating handler are
                        // independent of later monitor ownership. Reject any
                        // handler edge that enters monitor code or cleanup.
                        || (region.catches.iter().all(|(ty, _)| ty.is_some())
                            && graph.synchronized.iter().all(|r| region.end as usize <= r.enter)
                            && region.catches.iter().all(|(_, handler)| {
                                graph.reachable(*handler as usize, code.instructions.len(), &code.instructions)
                                    .is_ok_and(|seen| seen.iter().enumerate().all(|(pc, live)| {
                                        !live || (pc < code.instructions.len()
                                            && !matches!(code.instructions[pc] as u8, 0x1d | 0x1e)
                                            && graph.synchronized.iter().all(|r|
                                                pc < r.enter || pc > r.exit)
                                            && code.try_regions.iter().enumerate().all(|(i, protected)|
                                                !monitor_try(i)
                                                || pc < protected.start as usize
                                                || pc >= protected.end as usize))
                                    }))
                            }))
                }),
        "mixed monitor and ordinary exception regions not reconstructed"
    );
    if graph.shared_cfg.is_none()
        && (!code.try_regions.is_empty()
            || graph.switches.iter().any(Option::is_some)
            || graph.targets.iter().enumerate().any(|(pc, target)| {
                target.is_some() && matches!(code.instructions[pc] as u8, 0x32..=0x3d)
            }))
    {
        graph.live = super::liveness::analyze(code);
    }
    for region in &code.try_regions {
        ensure!(
            region.start < region.end
                && (region.end as usize) < graph.widths.len()
                && graph.widths[region.start as usize] != 0
                && graph.widths[region.end as usize] != 0,
            "try boundary is not executable instruction boundary"
        );
    }
    let (_, returned) = render(
        class,
        method,
        &graph,
        0,
        code.instructions.len(),
        regs,
        &mut out,
        0,
        !constructor,
        None,
        None,
    )?;
    ensure!(returned, "method falls off end");
    // DEX may omit Throws even though an exact static call in the rendered
    // body declares a checked exception. Add it only when the loaded override
    // family permits that declaration, after successful body reconstruction.
    if method.name.as_ref() != "<clinit>"
        && method.thrown_types.is_empty()
        && let Some(hierarchy) = class.symbols.hierarchy.get()
    {
        let mut pc = 0;
        while pc < code.instructions.len() {
            let width = graph.widths[pc];
            if width == 0 {
                pc += 1;
                continue;
            }
            if matches!(code.instructions[pc] as u8, 0x71 | 0x77)
                && let Some(&(owner_idx, proto_idx, name_idx)) = class
                    .symbols
                    .methods
                    .get(code.instructions[pc + 1] as usize)
                && let (Some(owner), Some((ret, args)), Some(name)) = (
                    class.symbols.types.get(owner_idx as usize),
                    class.symbols.protos.get(proto_idx as usize),
                    class.symbols.strings.get(name_idx as usize),
                )
                && let Some(thrown_types) = hierarchy
                    .exact_static_call_thrown_types(owner, name, args, ret)
                    .or_else(|| {
                        if owner.as_ref() != class.descriptor.as_ref() {
                            return None;
                        }
                        let mut matches = class.methods.iter().filter(|candidate| {
                            candidate.access_flags & 8 != 0
                                && candidate.name.as_ref() == name.as_str()
                                && candidate.parameters == *args
                                && candidate.return_type == *ret
                        });
                        let candidate = matches.next()?;
                        if matches.next().is_some() {
                            return None;
                        }
                        throwing::simple_static_parameter_throw(class, candidate)
                            .map(|ty| vec![std::sync::Arc::<str>::from(ty)])
                    })
            {
                for ty in thrown_types {
                    if throwing::inferable_checked_type(class, &ty)
                        && !throwing::locally_caught(
                            class,
                            method,
                            pc,
                            &Value {
                                text: String::new(),
                                ty: ty.to_string(),
                                literal: None,
                                wide_literal: None,
                                raw_bits32: false,
                            },
                        )
                    {
                        if hierarchy.permits_inferred_checked_throw(class, method, &ty) {
                            out.inferred_throws.insert(ty.to_string());
                        } else if !super::inherited_override_exception(class, method).is_some_and(
                            |declared| {
                                hierarchy.assignable(&ty, declared)
                                    == crate::native_hierarchy::Relation::Proven
                            },
                        ) {
                            bail!("Checked static call exceeds inherited throws contract");
                        }
                    }
                }
            }
            pc += width;
        }
    }
    let mut body = MethodBody {
        text: out.text,
        links: out.links,
        inferred_throws: out.inferred_throws.into_iter().collect(),
    };
    cleanup::inline_receivers(&mut body, &out.receiver_locals);
    Ok(body)
}

// Snapshot every carried value before assigning any loop slot: DEX moves can
// create aliases and swaps, so sequential assignments would change semantics.
fn carry_loop_values(
    slots: &[Option<Value>],
    values: &[Option<Value>],
    out: &mut Output,
) -> Result<()> {
    carry_loop_values_impl(slots, values, out, false)
}
fn carry_loop_values_impl(
    slots: &[Option<Value>],
    values: &[Option<Value>],
    out: &mut Output,
    retained_literals: bool,
) -> Result<()> {
    validate_wide_frame(slots)?;
    validate_wide_frame(values)?;
    let mut copies = Vec::new();
    for (slot, value) in slots.iter().zip(values) {
        let Some(slot) = slot else { continue };
        if slot.ty == "<wide-tail>" {
            continue;
        }
        let value = value.as_ref().context("loop loses initialized register")?;
        if retained_literals && slot.text == "this" {
            ensure!(value == slot, "loop changes retained receiver");
            continue;
        }
        if retained_literals && (slot.literal.is_some() || slot.wide_literal.is_some()) {
            ensure!(
                value.literal == slot.literal && value.wide_literal == slot.wide_literal,
                "loop changes retained literal"
            );
            continue;
        }
        if value == slot {
            continue;
        }
        // No reference downcasts are introduced merely to join a loop.
        ensure!(
            value.ty == slot.ty
                || (slot.ty == "Ljava/lang/Object;" && reference(&value.ty))
                || value.literal.is_some()
                || (slot.ty == "I" && matches!(value.ty.as_str(), "B" | "C" | "S")),
            "loop changes register type"
        );
        let expression = argument(value, &slot.ty)?;
        let copy = out.local(&slot.ty, &expression, &[])?;
        copies.push((slot.text.clone(), copy.text));
    }
    for (slot, value) in copies {
        out.line(&format!("{slot} = {value};"), &[]);
    }
    Ok(())
}
// Track entry identities through moves across the loop CFG. This proves that
// temporary register reuse restores a literal before every backedge/exit; a
// may-write set alone loses that fact and turns untyped float/boolean constants
// into permanently typed Java ints. Unknown instructions decline this proof.
fn loop_invariant_registers(
    code: &crate::native_dex::DexCode,
    graph: &Graph,
    region: Loop,
) -> Option<Vec<bool>> {
    use std::collections::VecDeque;
    let count = code.registers as usize;
    let words = &code.instructions;
    let end = region.body_end(&graph.widths);
    if count.checked_mul(end - region.start)? > 1_000_000 {
        return None;
    }
    let entry: Vec<_> = (0..count).map(Some).collect();
    let mut frames: std::collections::HashMap<usize, Vec<Option<usize>>> = Default::default();
    frames.insert(region.start, entry.clone());
    let mut pending = VecDeque::from([region.start]);
    let mut preserved = vec![true; count];
    let mut found = false;
    while let Some(pc) = pending.pop_front() {
        graph.tick().ok()?;
        let mut frame = frames.get(&pc)?.clone();
        let instruction = graph.instruction(pc).ok()?;
        let op = instruction.map_or(words[pc] as u8, |instruction| instruction.opcode);
        let width = instruction.map_or(graph.widths[pc], |instruction| instruction.width);
        let moved = if matches!(op, 0x01..=0x09) {
            let a = (words[pc] >> 8) as usize;
            let (dst, src) = if let Some(instruction) = instruction {
                decoded_move(instruction, count).ok()?
            } else {
                match (op - 1) % 3 {
                    0 => (a & 15, a >> 4),
                    1 => (a, *words.get(pc + 1)? as usize),
                    _ => (*words.get(pc + 1)? as usize, *words.get(pc + 2)? as usize),
                }
            };
            let n = if matches!(op, 0x04..=0x06) { 2 } else { 1 };
            Some((dst, frame.get(src..src + n)?.to_vec()))
        } else {
            None
        };
        let writes = graph.written_in(code, pc, pc + width)?;
        for (r, write) in writes.into_iter().enumerate() {
            if write {
                frame[r] = None;
            }
        }
        if let Some((dst, values)) = moved {
            frame
                .get_mut(dst..dst + values.len())?
                .copy_from_slice(&values);
        }
        if matches!(op, 0x0e..=0x11 | 0x27) {
            continue;
        }
        let mut successors = Vec::new();
        if let Some(cfg) = &graph.shared_cfg {
            let block = &cfg.blocks[*cfg.block_at.get(&pc)?];
            if block.instructions.last() == Some(&pc) {
                successors.extend(
                    block
                        .successors
                        .iter()
                        .map(|edge| cfg.blocks[edge.target].start),
                );
            } else {
                successors.push(pc + width);
            }
        } else {
            if let Some(switch) = &graph.switches[pc] {
                successors.extend(switch.cases.iter().map(|(_, target)| *target));
            }
            if let Some(target) = graph.targets[pc] {
                successors.push(target);
            }
            if !matches!(op, 0x28..=0x2a) {
                successors.push(pc + width);
            }
        }
        for next in successors {
            if next == region.start || !(region.start..end).contains(&next) {
                found = true;
                for (r, same) in preserved.iter_mut().enumerate() {
                    *same &= frame[r] == entry[r];
                }
                continue;
            }
            if let Some(previous) = frames.get_mut(&next) {
                let mut changed = false;
                for (old, new) in previous.iter_mut().zip(&frame) {
                    if old.is_some() && old != new {
                        *old = None;
                        changed = true;
                    }
                }
                if changed {
                    pending.push_back(next);
                }
            } else {
                frames.insert(next, frame.clone());
                pending.push_back(next);
            }
        }
    }
    found.then_some(preserved)
}
#[allow(clippy::too_many_arguments)]
fn render_loop(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    region: Loop,
    mut regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
    in_try: bool,
) -> Result<Vec<Option<Value>>> {
    ensure!(depth <= 32, "loop nesting exceeds budget");
    let words = &method
        .code
        .as_ref()
        .context("missing loop code")?
        .instructions;
    // Carry only values read before their next definition.  `loop_slots`
    // preserves wide heads and tails together while materializing Java locals
    // only for heads.
    ensure!(
        method
            .code
            .as_ref()
            .unwrap()
            .try_regions
            .iter()
            .enumerate()
            .all(|(index, protected)| protected.end as usize <= region.start
                || protected.start as usize >= region.exit
                || (in_try
                    && protected.start as usize <= region.start
                    && region.exit <= protected.end as usize)
                || (!in_try
                    && region.start <= protected.start as usize
                    && protected.end as usize <= region.exit)
                || graph
                    .synchronized
                    .iter()
                    .any(|monitor| monitor.tries.contains(&index)
                        && region.start > monitor.enter
                        && region.exit <= monitor.exit)),
        "loop overlaps protected region"
    );
    let no_snapshots = vec![None; regs.len()];
    let exception_context = in_try.then_some(no_snapshots.as_slice());
    // A direct common-exit cast proves incoming DEX zero is null. Normalize
    // before allocating header and exit locals so a zero-iteration guard does
    // not copy an int local into a reference join. Primitive uses still reject.
    let exit_cast_register = if region.exit == words.len() {
        None
    } else if let Some(instruction) = graph.instruction(region.exit)? {
        (instruction.opcode == 0x1f)
            .then(|| decoded_array_type(instruction, regs.len()).map(|cast| cast.dst))
            .transpose()?
    } else {
        words
            .get(region.exit)
            .and_then(|word| (*word as u8 == 0x1f).then_some((*word >> 8) as usize))
    };
    if let Some(r) = exit_cast_register
        && let Some(Some(seed)) = regs.get_mut(r)
        && seed.literal == Some(0)
    {
        seed.ty = "Ljava/lang/Object;".into();
        seed.text = "null".into();
    }
    let written = graph.written_in(method.code.as_ref().unwrap(), region.start, region.exit);
    let mut header_written = written.clone();
    if regs.iter().any(|value| {
        value
            .as_ref()
            .is_some_and(|value| value.literal.is_some() || value.wide_literal.is_some())
    }) && let Some(invariant) =
        loop_invariant_registers(method.code.as_ref().unwrap(), graph, region)
        && let Some(writes) = &mut header_written
    {
        for (r, value) in regs.iter().enumerate() {
            if let Some(value) = value
                && (value.literal.is_some() || value.wide_literal.is_some())
                && invariant[r]
                && (!wide(&value.ty) || invariant.get(r + 1) == Some(&true))
            {
                writes[r] = false;
            }
        }
    }
    let lifetime = promote_loop_entry_literals(
        class,
        method,
        graph,
        region,
        &mut regs,
        header_written.as_deref(),
    )?;
    let header_needed = lifetime
        .as_ref()
        .map(|proof| proof.header_needed.as_slice());
    let slots = loop_slots(
        &regs,
        graph,
        region.start,
        out,
        header_written.as_deref(),
        header_needed,
    )?;
    let mut carry_slots = slots.clone();
    if let Some(needed) = header_needed {
        for (r, slot) in carry_slots.iter_mut().enumerate() {
            if !needed[r] {
                *slot = None;
            }
        }
    }
    let has_break = if !graph.shared_loops.is_empty() {
        graph
            .shared_loop_edges
            .iter()
            .any(|edge| edge.owner == region.start && edge.kind == SharedLoopEdgeKind::Break)
    } else {
        graph.targets.iter().enumerate().any(|(pc, target)| {
            (region.start..region.latch).contains(&pc)
                && Some(pc) != region.guard
                && *target == Some(region.exit)
        })
    };
    // Header liveness excludes registers defined before the guard, even when
    // their final value is live after the loop. Preserve that distinct exit
    // frame for ordinary guarded loops too (including zero body iterations).
    let posttest = !graph.shared_loops.is_empty() && region.guard.is_none();
    let header_exit = (region.guard.is_some() || posttest)
        && (0..regs.len()).any(|r| {
            graph.live_at(region.exit, r)
                && (slots[r].is_none() || header_needed.is_some_and(|needed| !needed[r]))
        });
    let exit_slots =
        if region.tail.is_some() || has_break || header_exit || posttest {
            // Separate exit values from values required only by the next iteration.
            // In iterator loops the same DEX register may hold a String on the exit
            // path after holding an Iterator on the backedge.
            let mut exit_entry = regs.clone();
            let mut exit_written = if !graph.shared_loops.is_empty() {
                // The decoded invariant walk proves retained literals at both
                // the latch and every canonical exit, including conditional break.
                header_written.clone()
            } else {
                written.clone()
            };
            if (0..regs.len()).any(|r| {
                graph.live_at(region.exit, r)
                    && (regs[r].is_none() || (header_exit && slots[r].is_none()))
            }) {
                let prefix_end = if let Some(guard) = region.guard {
                    guard
                } else if posttest {
                    // An early break can reach the exit before later body
                    // definitions. Seed exit types only from the prefix that
                    // is guaranteed to execute before the first break.
                    graph
                        .shared_loop_edges
                        .iter()
                        .filter(|edge| {
                            edge.owner == region.start && edge.kind == SharedLoopEdgeKind::Break
                        })
                        .map(|edge| edge.branch)
                        .min()
                        .unwrap_or(region.latch)
                } else {
                    let mut cursor = region.start;
                    while cursor < region.latch
                        && graph.targets[cursor].is_none()
                        && graph.switches[cursor].is_none()
                        && !graph
                            .loops
                            .iter()
                            .any(|inner| inner.start == cursor && cursor != region.start)
                        && !matches!(words[cursor] as u8, 0x0e..=0x11 | 0x27)
                    {
                        cursor += graph.widths[cursor];
                    }
                    cursor
                };
                // The straight-line header dominates every loop escape. Infer newly
                // established exit types without hoisting any of its effects. Only
                // literals unchanged by the body may remain literal expressions.
                let mut preview = Output::default();
                let (mut header, returned) = render(
                    class,
                    method,
                    graph,
                    region.start,
                    prefix_end,
                    slots.clone(),
                    &mut preview,
                    depth,
                    true,
                    Some(LoopContext {
                        start: region.start,
                        slots: &carry_slots,
                        exit: region.exit,
                        exit_slots: &carry_slots,
                    }),
                    exception_context,
                )?;
                ensure!(!returned, "loop header returns");
                let body_header = header.clone();
                if let Some(guard) = region.guard
                    && let Some(guard_target) = graph.target(guard)?
                    && guard_target != region.exit
                {
                    let (values, returned) = render(
                        class,
                        method,
                        graph,
                        guard_target,
                        region.exit,
                        header,
                        &mut preview,
                        depth,
                        true,
                        None,
                        exception_context,
                    )?;
                    ensure!(!returned, "loop guard tail returns before merge");
                    header = values;
                }
                // If the guard's pure default tail assigns DEX zero while the
                // straight-line match body establishes a reference in the same
                // register, both loop exits have a reference value (null/item).
                // Inspect the body without emitting it; any branch, switch or
                // terminal instruction declines this narrow type proof.
                let reference_body_exit =
                    region
                        .guard
                        .and_then(|guard| {
                            let target = graph.targets[guard]?;
                            (region.tail.is_some()
                                && graph.pure_exit_tail(target, region.exit, words)
                                && method.code.as_ref().unwrap().try_regions.iter().all(
                                    |protected| {
                                        protected.end as usize <= guard + graph.widths[guard]
                                            || protected.start as usize >= region.latch
                                    },
                                )
                                && (guard + graph.widths[guard]..region.latch).all(|pc| {
                                    graph.widths[pc] == 0
                                        || (graph.targets[pc].is_none()
                                            && graph.switches[pc].is_none()
                                            && !matches!(words[pc] as u8, 0x0e..=0x11 | 0x27))
                                }))
                            .then_some(guard)
                        })
                        .and_then(|guard| {
                            let mut dry = Output::default();
                            render(
                                class,
                                method,
                                graph,
                                guard + graph.widths[guard],
                                region.latch,
                                body_header.clone(),
                                &mut dry,
                                depth,
                                true,
                                None,
                                exception_context,
                            )
                            .ok()
                            .and_then(|(values, returned)| (!returned).then_some(values))
                        });
                let body_writes = graph.written_in(
                    method.code.as_ref().unwrap(),
                    region
                        .guard
                        .map_or(prefix_end, |guard| guard + graph.widths[guard]),
                    region.exit,
                );
                for (r, value) in header.iter().enumerate() {
                    if !graph.live_at(region.exit, r)
                        || (exit_entry[r].is_some() && !(header_exit && slots[r].is_none()))
                    {
                        continue;
                    }
                    let Some(value) = value else { continue };
                    if value.ty == "<wide-tail>" {
                        continue;
                    }
                    let mut seed = value.clone();
                    if seed.literal == Some(0)
                        && reference_body_exit.as_ref().is_some_and(|body| {
                            body.get(r)
                                .and_then(Option::as_ref)
                                .is_some_and(|value| reference(&value.ty))
                        })
                    {
                        seed.ty = "Ljava/lang/Object;".into();
                        seed.text = "null".into();
                    }
                    if seed.literal == Some(0)
                        && if let Some(instruction) = graph.instruction(region.exit)? {
                            instruction.opcode == 0x1f
                                && decoded_array_type(instruction, regs.len())?.dst == r
                        } else {
                            words.get(region.exit).is_some_and(|word| {
                                *word as u8 == 0x1f && (*word >> 8) as usize == r
                            })
                        }
                    {
                        // A check-cast at the common exit proves this register is a
                        // reference on every incoming edge, including DEX null.
                        seed.ty = "Ljava/lang/Object;".into();
                        seed.text = "null".into();
                    }
                    if (seed.literal.is_some() || seed.wide_literal.is_some())
                        && body_writes.as_ref().is_some_and(|writes| !writes[r])
                    {
                        if let Some(writes) = &mut exit_written {
                            writes[r] = false;
                        }
                    } else {
                        seed.text = if reference(&seed.ty) {
                            "null"
                        } else {
                            match seed.ty.as_str() {
                                "Z" => "false",
                                "J" => "0L",
                                "F" => "0.0f",
                                "D" => "0.0d",
                                _ => "0",
                            }
                        }
                        .into();
                        seed.literal = None;
                        seed.wide_literal = None;
                    }
                    assign(&mut exit_entry, r, seed)?;
                }
            }
            if let Some(proof) = &lifetime {
                for (r, ty) in proof.exit_null_types.iter().enumerate() {
                    let Some(ty) = ty else { continue };
                    let seed = exit_entry[r]
                        .as_mut()
                        .context("loop exit null loses entry definition")?;
                    ensure!(
                        seed.ty == "I" && seed.literal == Some(0),
                        "loop exit null loses literal proof"
                    );
                    seed.ty = ty.clone();
                    seed.text = "null".into();
                    seed.literal = None;
                }
            }
            ensure!(
                (0..regs.len()).all(|r| !graph.live_at(region.exit, r) || exit_entry[r].is_some()),
                "loop exit value has no established entry type"
            );
            Some(loop_slots(
                &exit_entry,
                graph,
                region.exit,
                out,
                exit_written.as_deref(),
                None,
            )?)
        } else {
            None
        };
    let context = LoopContext {
        start: region.start,
        slots: &carry_slots,
        exit: region.exit,
        exit_slots: exit_slots.as_ref().unwrap_or(&slots),
    };
    let mut body = Output {
        sequence: out.sequence,
        indent: out.indent + 1,
        ..Default::default()
    };
    let (values, returned) = if let Some(guard) = region.guard {
        let (header_regs, returned) = render(
            class,
            method,
            graph,
            region.start,
            guard,
            slots.clone(),
            &mut body,
            depth,
            true,
            Some(context),
            exception_context,
        )?;
        ensure!(!returned, "loop header returns");
        let cond = graph.test(words, guard, false, &header_regs)?;
        body.line(&format!("if ({cond}) {{"), &[]);
        body.indent += 1;
        let guard_target = graph.target(guard)?.context("missing loop guard target")?;
        let (guard_values, guard_returned) = if guard_target != region.exit {
            render(
                class,
                method,
                graph,
                guard_target,
                region.exit,
                header_regs.clone(),
                &mut body,
                depth,
                true,
                Some(context),
                exception_context,
            )?
        } else {
            (header_regs.clone(), false)
        };
        if !guard_returned {
            graph.carry_loop_values(
                exit_slots.as_ref().unwrap_or(&slots),
                &guard_values,
                &mut body,
            )?;
            body.line("break;", &[]);
        }
        body.indent -= 1;
        body.line("}", &[]);
        render(
            class,
            method,
            graph,
            guard + graph.widths[guard],
            region.latch,
            header_regs,
            &mut body,
            depth,
            true,
            Some(context),
            exception_context,
        )?
    } else {
        render(
            class,
            method,
            graph,
            region.start,
            region.latch,
            slots.clone(),
            &mut body,
            depth,
            true,
            Some(context),
            exception_context,
        )?
    };
    if returned {
        // Every body path ends in a return/throw or an explicit continue. The
        // forward header guard still provides the loop's ordinary exit path.
        ensure!(
            (region.guard.is_some() || has_break || region.exit == words.len())
                && region.tail.is_none(),
            "loop body has no continuation"
        );
    } else if let Some(tail) = region.tail {
        let cond = if graph.shared_loops.is_empty() {
            condition(
                words[region.latch] as u8,
                (words[region.latch] >> 8) as usize,
                &values,
            )?
        } else {
            graph.test(words, region.latch, false, &values)?
        };
        body.line(&format!("if ({cond}) {{"), &[]);
        body.indent += 1;
        graph.carry_loop_values(&carry_slots, &values, &mut body)?;
        body.line("continue;", &[]);
        body.indent -= 1;
        body.line("}", &[]);
        let (tail_values, returned) = render(
            class,
            method,
            graph,
            tail,
            region.exit,
            values,
            &mut body,
            depth,
            true,
            Some(context),
            exception_context,
        )?;
        if !returned {
            graph.carry_loop_values(
                exit_slots.as_ref().expect("tail exit slots"),
                &tail_values,
                &mut body,
            )?;
            body.line("break;", &[]);
        }
    } else if region.guard.is_some()
        && !graph.shared_loops.is_empty()
        && graph
            .instruction(region.latch)?
            .is_some_and(|i| matches!(i.opcode, 0x32..=0x3d))
    {
        // The conditional latch either reexecutes the header or falls through
        // to the selected terminal return. The guard's default return was
        // emitted above; neither exit needs a fabricated post-loop value.
        let cond = graph.test(words, region.latch, false, &values)?;
        body.line(&format!("if ({cond}) {{"), &[]);
        body.indent += 1;
        graph.carry_loop_values(&carry_slots, &values, &mut body)?;
        body.line("continue;", &[]);
        body.indent -= 1;
        body.line("}", &[]);
        let (_, terminal) = render(
            class,
            method,
            graph,
            region.latch + graph.widths[region.latch],
            region.exit,
            values,
            &mut body,
            depth,
            true,
            Some(context),
            exception_context,
        )?;
        ensure!(terminal, "shared terminal latch exit does not return");
    } else if region.guard.is_none()
        && !matches!(
            graph
                .instruction(region.latch)?
                .map_or(words[region.latch] as u8, |i| i.opcode),
            0x28..=0x2a
        )
    {
        let cond = graph.test(words, region.latch, true, &values)?;
        // Evaluate the condition before rewriting the loop slots.
        let test = body.local("Z", &cond, &[])?;
        if exit_slots.is_none() {
            graph.carry_loop_values(&carry_slots, &values, &mut body)?;
        }
        body.line(&format!("if ({}) {{", test.text), &[]);
        body.indent += 1;
        if let Some(exit_slots) = &exit_slots {
            // Snapshot exit values before rewriting possibly aliased header
            // slots on the continuation path.
            graph.carry_loop_values(exit_slots, &values, &mut body)?;
        }
        body.line("break;", &[]);
        body.indent -= 1;
        body.line("}", &[]);
        if exit_slots.is_some() {
            graph.carry_loop_values(&carry_slots, &values, &mut body)?;
        }
    } else {
        graph.carry_loop_values(&carry_slots, &values, &mut body)?;
    }
    out.sequence = body.sequence;
    out.line("while (true) {", &[]);
    out.append(body);
    out.line("}", &[]);
    ensure!(
        out.text.len() <= 4 * 1024 * 1024,
        "reconstructed method exceeds output budget"
    );
    Ok(exit_slots.unwrap_or(slots))
}

fn sync_exception_registers(
    slots: &[Option<Value>],
    regs: &[Option<Value>],
    previous: &mut [Option<Value>],
    out: &mut Output,
    allocating: bool,
) -> Result<()> {
    let mut visible = slots.to_vec();
    for (r, slot) in visible.iter_mut().enumerate() {
        if regs[r] == previous[r] {
            *slot = None;
            continue;
        }
        if let Some(value) = &regs[r] {
            if value.text.starts_with("<class:") {
                *slot = None;
                continue;
            }
            ensure!(
                !allocating || slot.as_ref().is_none_or(|old| old == value),
                "allocation reorders exception-visible register write"
            );
        }
    }
    carry_loop_values(&visible, regs, out)?;
    // Semantic register values remain immutable expressions. Only handlers read
    // mutable snapshots: rewriting registers to snapshot names corrupts aliases.
    previous.clone_from_slice(regs);
    Ok(())
}

// No handler can observe register writes followed only by a bare return.
// In particular, move-result may reuse an argument register with a different
// type after the last protected invocation. Do not force that value into the
// handler's old argument snapshot. Follow only proven nonthrowing goto tails.
fn bare_return_tail(graph: &Graph, words: &[u16], mut pc: usize) -> bool {
    for _ in 0..32 {
        let Some(word) = words.get(pc) else {
            return false;
        };
        if graph.widths.get(pc).copied().unwrap_or(0) == 0 || graph.payloads[pc] {
            return false;
        }
        match *word as u8 {
            0x0e..=0x11 => return true,
            0x28..=0x2a => {
                let Some(target) = graph.targets[pc] else {
                    return false;
                };
                pc = target;
            }
            _ => return false,
        }
    }
    false
}

fn catch_parent(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "Ljava/lang/Exception;" | "Ljava/lang/Error;" => "Ljava/lang/Throwable;",
        "Ljava/lang/RuntimeException;"
        | "Ljava/io/IOException;"
        | "Lorg/json/JSONException;"
        | "Landroid/util/AndroidException;" => "Ljava/lang/Exception;",
        "Landroid/os/RemoteException;" => "Landroid/util/AndroidException;",
        "Ljava/io/FileNotFoundException;" => "Ljava/io/IOException;",
        "Landroid/content/ActivityNotFoundException;"
        | "Ljava/lang/ArithmeticException;"
        | "Ljava/lang/ArrayStoreException;"
        | "Ljava/lang/ClassCastException;"
        | "Ljava/lang/IllegalArgumentException;"
        | "Ljava/lang/IllegalStateException;"
        | "Ljava/lang/IndexOutOfBoundsException;"
        | "Ljava/lang/NullPointerException;"
        | "Ljava/lang/SecurityException;"
        | "Ljava/lang/UnsupportedOperationException;"
        | "Ljava/lang/NegativeArraySizeException;" => "Ljava/lang/RuntimeException;",
        "Ljava/lang/NumberFormatException;" => "Ljava/lang/IllegalArgumentException;",
        "Ljava/lang/ArrayIndexOutOfBoundsException;"
        | "Ljava/lang/StringIndexOutOfBoundsException;" => "Ljava/lang/IndexOutOfBoundsException;",
        _ => return None,
    })
}

fn validate_catch_order(
    class: &DexClass,
    catches: &[(Option<std::sync::Arc<str>>, u32)],
) -> Result<()> {
    ensure!(
        catches.len() <= 128,
        "exception handler count exceeds reconstruction budget"
    );
    for (index, (later, _)) in catches.iter().enumerate() {
        let later = later.as_deref().unwrap_or("Ljava/lang/Throwable;");
        for (earlier, _) in &catches[..index] {
            let earlier = earlier.as_deref().unwrap_or("Ljava/lang/Throwable;");
            ensure!(
                earlier != later && earlier != "Ljava/lang/Throwable;",
                "catch ordering shadows a later handler"
            );
            if later == "Ljava/lang/Throwable;" {
                continue;
            }
            if let Some(hierarchy) = class.symbols.hierarchy.get() {
                match hierarchy.assignable(later, earlier) {
                    crate::native_hierarchy::Relation::Proven => {
                        bail!("catch ordering shadows a later handler")
                    }
                    crate::native_hierarchy::Relation::Disproven => continue,
                    crate::native_hierarchy::Relation::Unknown => {}
                }
            }
            ensure!(
                catch_parent(earlier).is_some() && catch_parent(later).is_some(),
                "catch ordering requires unavailable type hierarchy"
            );
            let mut ancestor = Some(later);
            while let Some(ty) = ancestor {
                ensure!(ty != earlier, "catch ordering shadows a later handler");
                ancestor = catch_parent(ty);
            }
        }
    }
    Ok(())
}
// A protected region can exit through several goto trampolines. Only move
// those non-effectful instructions into the Java try; handler blocks embedded
// in its address interval must remain unreachable through normal edges.
fn protected_normal_exit(
    graph: &Graph,
    words: &[u16],
    start: usize,
    end: usize,
    loop_start: Option<usize>,
) -> Result<usize> {
    let mut pending = vec![start];
    let mut seen = vec![false; words.len()];
    let mut exits = std::collections::BTreeSet::new();
    while let Some(pc) = pending.pop() {
        graph.tick()?;
        if pc < start || pc >= end {
            let mut exit = pc;
            let mut trampolines = std::collections::HashSet::new();
            while exit < words.len() && matches!(words[exit] as u8, 0x28..=0x2a) {
                if graph.targets[exit] == loop_start {
                    break;
                }
                ensure!(trampolines.insert(exit), "cyclic protected exit");
                exit = graph.targets[exit].context("missing protected exit target")?;
            }
            ensure!(exit >= end, "protected flow escapes backward");
            exits.insert(exit);
            continue;
        }
        if seen[pc] {
            continue;
        }
        seen[pc] = true;
        let op = words[pc] as u8;
        ensure!(op != 0x0d, "normal flow enters move-exception");
        if matches!(op, 0x0e..=0x11 | 0x27) {
            continue;
        }
        if let Some(target) = graph.targets[pc] {
            pending.push(target);
        }
        if let Some(switch) = &graph.switches[pc] {
            pending.extend(switch.cases.iter().map(|(_, target)| *target));
        }
        if !matches!(op, 0x28..=0x2a) {
            pending.push(pc + graph.widths[pc]);
        }
    }
    if exits.len() > 1 {
        // Pure forward branches/moves ending in returns can be included in the
        // Java try: none introduces a newly caught exception. Effectful tails
        // must retain their original exception boundary.
        let mut finish = end;
        let mut effectful_frontiers = std::collections::BTreeSet::new();
        let mut pending: Vec<_> = exits.into_iter().collect();
        let mut seen = std::collections::HashSet::new();
        while let Some(pc) = pending.pop() {
            graph.tick()?;
            ensure!(
                pc >= end && pc < words.len() && graph.widths[pc] != 0,
                "protected return tail is cyclic or invalid"
            );
            if !seen.insert(pc) {
                continue;
            }
            let op = words[pc] as u8;
            let pure = matches!(op, 0x00..=0x09 | 0x0e..=0x19 | 0x28..=0x2a | 0x32..=0x3d);
            if pure {
                finish = finish.max(pc + graph.widths[pc]);
            }
            match op {
                0x0e..=0x11 => {}
                0x00..=0x09 | 0x12..=0x19 => pending.push(pc + graph.widths[pc]),
                0x28..=0x2a | 0x32..=0x3d => {
                    let target = graph.targets[pc].context("missing return tail target")?;
                    ensure!(target > pc, "cyclic protected return tail");
                    pending.push(target);
                    if op >= 0x32 {
                        pending.push(pc + graph.widths[pc]);
                    }
                }
                _ => {
                    effectful_frontiers.insert(pc);
                }
            }
        }
        ensure!(
            effectful_frontiers.len() <= 1,
            "protected region has distinct effectful exits"
        );
        if let Some(frontier) = effectful_frontiers.into_iter().next() {
            // Pure early-return paths can stay in try while the one surviving
            // effectful continuation stays outside. No effect crosses its DEX
            // exception boundary, including boxing calls after coroutine exits.
            ensure!(
                finish <= frontier,
                "protected return tail crosses effectful continuation"
            );
            return Ok(frontier);
        }
        return Ok(finish);
    }
    Ok(exits.into_iter().next().unwrap_or(end))
}

// Split a protected interval at a pure branch when one arm leaves the try
// before an effectful terminal tail. Each reached suffix retains the original
// handler set; the branch and its external arm stay outside that catch.
fn protected_branch_split(
    graph: &Graph,
    words: &[u16],
    entry: usize,
    start: usize,
    end: usize,
    loop_start: Option<usize>,
) -> Result<Option<usize>> {
    let Some(escape) = (entry..end).find(|&pc| {
        graph.widths[pc] != 0
            && matches!(words[pc] as u8, 0x32..=0x3d)
            && pc + graph.widths[pc] < end
            && graph.targets[pc].is_some_and(|target| target >= end)
    }) else {
        return Ok(None);
    };
    match protected_normal_exit(graph, words, start, end, loop_start) {
        Ok(_) => return Ok(None),
        Err(error) if error.to_string() == "protected region has distinct effectful exits" => {}
        Err(error) => return Err(error),
    }
    Ok((entry..escape)
        .find(|&pc| {
            graph.widths[pc] != 0
                && matches!(words[pc] as u8, 0x32..=0x3d)
                && graph.targets[pc].is_some_and(|target| target > escape && target < end)
        })
        .or(Some(escape)))
}

fn stable_retry_argument(value: &Value) -> bool {
    if value.literal.is_some() || value.wide_literal.is_some() || value.text == "this" {
        return true;
    }
    if value
        .text
        .strip_prefix(['v', 'p'])
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    {
        return true;
    }
    let Some(inner) = value
        .text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
    else {
        return false;
    };
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if chars.next().is_none() {
                return false;
            }
        } else if matches!(ch, '"' | '\n' | '\r') {
            return false;
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn render_try(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    region: &crate::native_dex::DexTryRegion,
    stop: usize,
    regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
    enclosing_loop: Option<LoopContext<'_>>,
) -> Result<(Vec<Option<Value>>, usize, bool)> {
    ensure!(depth <= 32, "exception nesting exceeds budget");
    let words = &method
        .code
        .as_ref()
        .context("missing try code")?
        .instructions;
    let start = region.start as usize;
    let end = region.end as usize;
    if let Some(context) = enclosing_loop {
        ensure!(
            context.start <= start
                && end <= context.exit
                && region
                    .catches
                    .iter()
                    .all(|(_, handler)| (*handler as usize) < context.exit),
            "inner try handler escapes enclosing loop"
        );
    }
    validate_wide_frame(&regs)?;
    ensure!(
        end <= stop && !region.catches.is_empty(),
        "try crosses enclosing region"
    );
    let normal_end = protected_normal_exit(
        graph,
        words,
        start,
        end,
        enclosing_loop.map(|context| context.start),
    )?;
    ensure!(
        normal_end <= stop,
        "try continuation crosses enclosing region"
    );
    let normal_reachable = graph.reachable(start, normal_end, words)?;
    ensure!(
        method
            .code
            .as_ref()
            .unwrap()
            .try_regions
            .iter()
            .enumerate()
            .all(|(index, other)| {
                graph
                    .synchronized
                    .iter()
                    .any(|monitor| monitor.tries.contains(&index))
                    || other.start <= region.start
                    || normal_end <= other.start as usize
                    // Handler-local try blocks can lie between the protected
                    // body and its forward continuation without being executed
                    // by that body's normal path.
                    || !(other.start as usize..other.end as usize)
                        .any(|pc| pc < normal_end && normal_reachable.get(pc).copied().unwrap_or(false))
            }),
        "protected continuation crosses another exception region"
    );
    ensure!(
        region
            .catches
            .iter()
            .all(|(_, handler)| { *handler >= region.end || !normal_reachable[*handler as usize] }),
        "normal flow enters interleaved handler"
    );
    // A single void call at the end of its own protected range can retry at
    // the handler entry if it throws the caught type. The catch body below
    // gets a small loop so that this DEX edge is not lost in Java.
    let reentering_handler = region.catches.first().and_then(|(ty, handler)| {
        let handler = *handler as usize;
        (region.catches.len() == 1
            && throwing::validate_catch_type(
                class,
                ty.as_deref().unwrap_or("Ljava/lang/Throwable;"),
                false,
            )
            .is_ok()
            && handler >= start
            && handler < end
            && !normal_reachable[handler]
            && matches!(words[handler] as u8, 0x71 | 0x77)
            && handler + graph.widths[handler] == end
            && class
                .symbols
                .methods
                .get(words[handler + 1] as usize)
                .and_then(|(_, proto, name)| {
                    Some((
                        class.symbols.protos.get(*proto as usize)?,
                        class.symbols.strings.get(*name as usize)?,
                    ))
                })
                .is_some_and(|((ret, _), name)| {
                    ret.as_ref() == "V" && !matches!(name.as_str(), "<init>" | "<clinit>")
                }))
        .then_some(handler)
    });
    for (_, handler) in region
        .catches
        .iter()
        .filter(|(_, handler)| *handler < region.end)
    {
        let handler_reachable = graph.reachable(*handler as usize, stop, words)?;
        for pc in start..end {
            // A throwing handler instruction inside its own protected range
            // can re-enter that handler. Moving it to Java catch would lose
            // that edge. Pure move/constant/branch prefixes are safe.
            ensure!(
                !handler_reachable[pc]
                    || matches!(words[pc] as u8, 0x00..=0x19 | 0x28..=0x2a | 0x32..=0x3d)
                    || reentering_handler == Some(pc),
                "throwing handler instruction inside protected region"
            );
        }
    }
    validate_catch_order(class, &region.catches)?;
    let mut guarded_dispatch = false;
    for (ty, _) in region.catches.iter() {
        if let Some(ty) = ty {
            let mut declared_throw = false;
            for pc in start..end {
                if !normal_reachable[pc] || !matches!(words[pc] as u8, 0x6e..=0x72 | 0x74..=0x78) {
                    continue;
                }
                if let Some(&(owner, proto, name)) =
                    class.symbols.methods.get(words[pc + 1] as usize)
                    && let Some(owner) = class.symbols.types.get(owner as usize)
                    && let Some((ret, args)) = class.symbols.protos.get(proto as usize)
                    && let Some(name) = class.symbols.strings.get(name as usize)
                {
                    declared_throw |= throwing::known_call_throws(owner, name, args, ret, ty);
                    declared_throw |= class.symbols.hierarchy.get().is_some_and(|hierarchy| {
                        hierarchy.call_declares(
                            owner,
                            name,
                            args,
                            ret,
                            ty,
                            matches!(words[pc] as u8, 0x71 | 0x77),
                        )
                    });
                }
            }
            throwing::validate_type(class, ty)?;
            guarded_dispatch |= throwing::validate_catch_type(class, ty, declared_throw).is_err();
        }
    }
    // Identify entry values actually observed by handlers (including their
    // continuation). Unused entry registers may freely change type in the try.
    // Wide operations are safe without snapshots; an immutable wide entry must
    // have neither word overwritten. Tail text remains ownership metadata.
    let written = super::liveness::written_in(
        method.code.as_ref().context("missing try code")?,
        start,
        end,
    );
    let mut needed = vec![false; regs.len()];
    for (ty, handler) in region.catches.iter() {
        let handler = *handler as usize;
        let mut probe: Vec<_> = regs
            .iter()
            .enumerate()
            .map(|(r, value)| {
                value.as_ref().map(|v| {
                    if v.ty == "<wide-tail>"
                        || written
                            .as_ref()
                            .is_some_and(|writes| !writes[r] && (!wide(&v.ty) || !writes[r + 1]))
                    {
                        // Immutable entry values need no exception snapshot.
                        // Keep literal typing (notably boolean/null constants).
                        v.clone()
                    } else {
                        Value {
                            text: format!("__rdx_exception_input_{r}__"),
                            ty: v.ty.clone(),
                            literal: None,
                            wide_literal: None,
                            raw_bits32: false,
                        }
                    }
                })
            })
            .collect();
        let mut entry = handler;
        if words[handler] as u8 == 0x0d {
            assign(
                &mut probe,
                (words[handler] >> 8) as usize,
                Value {
                    text: "__rdx_caught__".into(),
                    ty: ty.as_deref().unwrap_or("Ljava/lang/Throwable;").into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )?;
            entry += 1;
        }
        let saved_caught_values = graph.caught_values.borrow().clone();
        let saved_catch_rethrows = graph.catch_rethrows.borrow().clone();
        graph
            .caught_values
            .borrow_mut()
            .insert("__rdx_caught__".into());
        let mut dry = Output::default();
        let probe_result = render(
            class,
            method,
            graph,
            entry,
            words.len(),
            probe,
            &mut dry,
            depth,
            true,
            enclosing_loop,
            None,
        );
        // Probing must not leak generated handler identities into real output:
        // its independent local sequence may reuse a later real catch name.
        *graph.caught_values.borrow_mut() = saved_caught_values;
        *graph.catch_rethrows.borrow_mut() = saved_catch_rethrows;
        let (_, returned) = probe_result?;
        ensure!(returned, "handler falls off method");
        for (r, observed) in needed.iter_mut().enumerate() {
            *observed |= dry.text.contains(&format!("__rdx_exception_input_{r}__"));
        }
    }
    let mut slots = vec![None; regs.len()];
    let mut initial = regs.clone();
    for (r, value) in regs.iter().enumerate() {
        if needed[r] {
            let value = value
                .as_ref()
                .context("handler observes undefined entry register")?;
            ensure!(
                !wide(&value.ty) && value.ty != "<wide-tail>",
                "wide exception snapshots not reconstructed"
            );
            let slot = out.local(&value.ty, &value.text, &[])?;
            initial[r] = Some(slot.clone());
            slots[r] = Some(slot);
        }
    }
    let mut body = Output {
        sequence: out.sequence,
        indent: out.indent + 1,
        ..Default::default()
    };
    let escape_used = std::rc::Rc::new(std::cell::Cell::new(false));
    let escape_label = format!("tryExit{}", body.sequence);
    body.sequence += 1;
    let invariant_continuation = normal_end == end
        && graph
            .written_in(method.code.as_ref().unwrap(), start, normal_end)
            .is_some_and(|writes| {
                writes
                    .iter()
                    .enumerate()
                    .all(|(r, write)| !*write || !graph.live_at(normal_end, r))
            });
    let previous_escape = graph
        .protected_loop_escape
        .replace(invariant_continuation.then(|| {
            ProtectedLoopEscape {
                start,
                end: normal_end,
                label: escape_label.clone(),
                forbidden_writes: (0..initial.len())
                    .map(|r| graph.live_at(normal_end, r) || slots[r].is_some())
                    .collect(),
                used: escape_used.clone(),
            }
        }));
    let normal_result = render(
        class,
        method,
        graph,
        start,
        normal_end,
        regs,
        &mut body,
        depth,
        true,
        enclosing_loop,
        Some(&slots),
    );
    graph.protected_loop_escape.replace(previous_escape);
    let (mut normal, normal_return) = normal_result?;
    if escape_used.get() {
        // A labeled escape skips assignments at the end of the normal body.
        // Whole-region write analysis proved these physical registers invariant;
        // reuse their initialized outer values rather than loop-local aliases.
        for r in 0..normal.len() {
            if graph.live_at(normal_end, r) {
                normal[r] = initial[r].clone();
            }
        }
    }
    let mut visits = vec![0u16; words.len() + 1];
    let mut roots = Vec::new();
    if !normal_return {
        roots.push(normal_end);
    }
    roots.extend(region.catches.iter().map(|(_, addr)| *addr as usize));
    // Duplicate handlers count as a single path when finding a common tail.
    roots.sort_unstable();
    roots.dedup();
    // A handler may be physically after an enclosing branch's join. It is
    // owned by this try, not by that branch's normal instruction interval.
    // Permit only detached forward tails: no handler edge may re-enter the
    // enclosing interval, and rendering below must prove termination.
    let mut outside_handlers = std::collections::HashSet::new();
    let mut loop_handlers = std::collections::HashSet::new();
    let mut detached_forward_handlers = std::collections::HashSet::new();
    let split_subregion = !method
        .code
        .as_ref()
        .unwrap()
        .try_regions
        .iter()
        .any(|original| {
            original.start == region.start
                && original.end == region.end
                && original.catches == region.catches
        });
    for (_, handler) in region.catches.iter() {
        let handler = *handler as usize;
        if handler > stop {
            if enclosing_loop.is_some_and(|context| handler < context.exit) {
                // The catch is lexically inside the enclosing Java loop even
                // when it lies beyond this branch's stop. Its renderer must
                // prove a terminal continue/break/return below; it is not a
                // normal join root and may serve another disjoint try arm.
                outside_handlers.insert(handler);
                loop_handlers.insert(handler);
                continue;
            }
            // Shared cleanup/dispatch handlers need ownership across all of
            // their protected ranges, not this enclosing branch alone.
            ensure!(
                method
                    .code
                    .as_ref()
                    .unwrap()
                    .try_regions
                    .iter()
                    .filter(|other| other
                        .catches
                        .iter()
                        .any(|(_, target)| *target as usize == handler))
                    .count()
                    == 1
                    || split_subregion,
                "outside handler shared by multiple protected regions"
            );
            let seen = graph.reachable(handler, words.len(), words)?;
            ensure!(
                !seen[..=stop].iter().any(|seen| *seen)
                    && graph.non_reentering_tail(handler, 0..stop + 1, words)?,
                "outside handler re-enters enclosing region"
            );
            outside_handlers.insert(handler);
        }
        if handler < stop {
            // A catch can jump past the normal continuation to its own
            // cleanup+throw tail. Render that tail in the catch; it cannot
            // re-enter instructions preceding its move-exception entry.
            let seen = graph.reachable(handler, words.len(), words)?;
            if seen.iter().enumerate().any(|(pc, live)| pc > stop && *live)
                && !seen[..handler].iter().any(|live| *live)
                && graph.non_reentering_tail(handler, 0..handler, words)?
            {
                detached_forward_handlers.insert(handler);
            }
        }
    }
    for root in &roots {
        if loop_handlers.contains(root) || detached_forward_handlers.contains(root) {
            continue;
        }
        let limit = if outside_handlers.contains(root) {
            words.len()
        } else {
            stop
        };
        let seen = graph.reachable(*root, limit, words)?;
        for (count, seen) in visits.iter_mut().zip(seen) {
            *count += u16::from(seen);
        }
    }
    // Terminating handlers with no path back to normal flow can stay in
    // their catch arms. Keep the effectful normal continuation outside try.
    let detached_handlers = !normal_return
        && region.catches.iter().all(|(_, handler)| {
            *handler as usize != normal_end
                && ((*handler as usize) < stop || outside_handlers.contains(&(*handler as usize)))
        })
        && (!graph
            .reachable(normal_end, stop, words)?
            .iter()
            .zip(&visits)
            .any(|(seen, count)| *seen && *count > 1)
            // A shared bare void return has no state or effects to merge.
            // Emit it directly in catch, leaving normal effects outside try.
            || region.catches.iter().all(|(_, handler)| words[*handler as usize] as u8 == 0x0e));
    // A catch may enter the same downstream cleanup as the normal path. If
    // the normal prefix up to that handler is entirely pure, stop this try at
    // the handler entry. The caller renders the effectful cleanup in its own
    // exception region, so it executes once with the original catch ownership.
    let shared_handler_join = if !normal_return {
        region
            .catches
            .iter()
            .map(|(_, handler)| *handler as usize)
            .max()
            .filter(|&candidate| candidate > normal_end && candidate < stop)
            .filter(|&candidate| words[candidate] as u8 != 0x0d)
            .filter(|&candidate| {
                graph
                    .reachable(normal_end, candidate, words)
                    .is_ok_and(|seen| {
                        seen[candidate]
                            && seen.iter().enumerate().all(|(pc, reachable)| {
                                !*reachable
                                    || pc == candidate
                                    || matches!(words[pc] as u8,
                            0x00..=0x09 | 0x0e..=0x19 | 0x28..=0x2a | 0x32..=0x3d)
                            })
                    })
            })
            .filter(|&candidate| {
                region.catches.iter().all(|(_, handler)| {
                    graph.reachable(*handler as usize, candidate, words).is_ok()
                })
            })
    } else {
        None
    };
    let join = if normal_return {
        // No ordinary value reaches a join; handlers own their complete
        // terminal tails even when a handler begins at the old normal end.
        stop
    } else if detached_handlers {
        normal_end
    } else if let Some(candidate) = shared_handler_join {
        candidate
    } else if roots.len() == 1 && !normal_return && roots[0] == normal_end {
        roots[0]
    } else {
        let latest_entry = roots.iter().copied().max().unwrap_or(normal_end);
        visits
            .iter()
            .enumerate()
            .find_map(|(pc, count)| {
                (pc >= latest_entry && usize::from(*count) == roots.len()).then_some(pc)
            })
            .unwrap_or(stop)
    };
    let mut paths = Vec::new();
    let mut normal_regs = normal;
    let mut normal_terminal = normal_return;
    if !normal_return {
        let normal_reachable = graph.reachable(normal_end, join, words)?;
        ensure!(
            region
                .catches
                .iter()
                .all(|(_, handler)| words[*handler as usize] as u8 != 0x0d
                    || !normal_reachable[*handler as usize]),
            "normal flow enters move-exception"
        );
        let mut continuation = Output {
            sequence: body.sequence,
            indent: body.indent,
            ..Default::default()
        };
        (normal_regs, normal_terminal) = render(
            class,
            method,
            graph,
            normal_end,
            join,
            normal_regs,
            &mut continuation,
            depth,
            true,
            enclosing_loop,
            None,
        )?;
        // Moving effects from outside the DEX try would change which throws
        // are caught. An empty goto path or a bare void return has no throwing
        // expression and can be folded into the try arm. Pure move/constant/
        // branch/return tails are safe too: they introduce no caught effects.
        // Preserve the return
        // so terminal normal paths never fall through into a shared tail.
        ensure!(
            continuation.text.is_empty()
                || (normal_terminal && continuation.text.trim() == "return;")
                || (normal_terminal
                    && enclosing_loop.is_some_and(|context| {
                        graph
                            .targets
                            .get(normal_end)
                            .copied()
                            .flatten()
                            .is_some_and(|target| target == context.start || target == context.exit)
                            && matches!(words[normal_end] as u8, 0x28..=0x2a)
                    })
                    && matches!(continuation.text.trim(), "continue;" | "break;"))
                || normal_reachable.iter().enumerate().all(|(pc, reachable)| {
                    !reachable
                        || matches!(words[pc] as u8,
                        0x00..=0x09 | 0x0e..=0x19 | 0x28..=0x2a | 0x32..=0x3d)
                }),
            "effectful normal continuation before exception join"
        );
        body.sequence = continuation.sequence;
        body.append(continuation);
    }
    let mut sequence = body.sequence;
    let dispatch_name = format!("caught{sequence}");
    if guarded_dispatch {
        sequence += 1;
    }
    paths.push((usize::MAX, body, normal_regs, normal_terminal));
    let mut headers = Vec::new();
    let mut types = std::collections::HashSet::new();
    for (index, (ty, handler)) in region.catches.iter().enumerate() {
        let ty = ty.as_deref().unwrap_or("Ljava/lang/Throwable;");
        ensure!(types.insert(ty), "duplicate catch type");
        // A detached catch can reach the enclosing branch's shared bare
        // return. Include only that terminal instruction: moving calls or
        // other effects across this boundary would change catch ownership.
        let handler_stop = if loop_handlers.contains(&(*handler as usize))
            || detached_forward_handlers.contains(&(*handler as usize))
        {
            words.len()
        } else if detached_handlers {
            if outside_handlers.contains(&(*handler as usize)) {
                words.len()
            } else if stop < words.len()
                && graph.widths[stop] != 0
                && matches!(words[stop] as u8, 0x0e..=0x11)
            {
                stop + graph.widths[stop]
            } else {
                stop
            }
        } else {
            join
        };
        let mut entry = *handler as usize;
        ensure!(
            entry <= handler_stop,
            "partially overlapping exception handlers"
        );
        let name = format!("e{sequence}");
        graph.caught_values.borrow_mut().insert(name.clone());
        if guarded_dispatch {
            graph
                .catch_rethrows
                .borrow_mut()
                .insert(name.clone(), dispatch_name.clone());
        } else {
            graph.catch_rethrows.borrow_mut().remove(&name);
        }
        sequence += 1;
        let mut values = initial.clone();
        if words[entry] as u8 == 0x0d {
            assign(
                &mut values,
                (words[entry] >> 8) as usize,
                Value {
                    text: name.clone(),
                    ty: ty.into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )?;
            entry += 1;
        }
        ensure!(
            entry <= handler_stop,
            "move-exception intersects shared continuation"
        );
        let retry = reentering_handler == Some(*handler as usize);
        if retry {
            let count = (words[entry] >> 8) as usize;
            let inputs: Vec<usize> = if words[entry] as u8 == 0x77 {
                (words[entry + 2] as usize..words[entry + 2] as usize + count).collect()
            } else {
                let count = count >> 4;
                ensure!(count <= 5, "self-covered handler invoke register count");
                let packed = words[entry + 2];
                let all = [
                    (packed & 15) as usize,
                    ((packed >> 4) & 15) as usize,
                    ((packed >> 8) & 15) as usize,
                    ((packed >> 12) & 15) as usize,
                    (words[entry] >> 8) as usize & 15,
                ];
                all[..count].to_vec()
            };
            ensure!(
                inputs.iter().all(|&r| {
                    values
                        .get(r)
                        .and_then(Option::as_ref)
                        .is_some_and(stable_retry_argument)
                }),
                "self-covered handler has deferred invoke argument"
            );
        }
        let catch_indent = out.indent + if guarded_dispatch { 2 } else { 1 };
        let mut handler_body = Output {
            sequence,
            indent: catch_indent + if retry { 2 } else { 0 },
            ..Default::default()
        };
        let (values, terminal) = render(
            class,
            method,
            graph,
            entry,
            handler_stop,
            values,
            &mut handler_body,
            depth,
            true,
            enclosing_loop,
            None,
        )?;
        ensure!(
            !retry || (handler_stop == end && !terminal),
            "self-covered handler has effectful continuation"
        );
        ensure!(
            !retry || handler_body.text.lines().count() == 1,
            "self-covered handler emitted more than one statement"
        );
        ensure!(
            !detached_forward_handlers.contains(&(*handler as usize)) || terminal,
            "detached forward handler does not terminate"
        );
        ensure!(
            !detached_handlers || terminal,
            "detached handler does not terminate"
        );
        if retry {
            let mut wrapped = Output {
                sequence: handler_body.sequence,
                indent: catch_indent,
                ..Default::default()
            };
            wrapped.line("while (true) {", &[]);
            wrapped.indent += 1;
            wrapped.line("try {", &[]);
            wrapped.indent += 1;
            wrapped.append(handler_body);
            wrapped.line("break;", &[]);
            wrapped.indent -= 1;
            let retry_name = format!("retry{}", wrapped.sequence);
            wrapped.sequence += 1;
            let display = java_type(ty)?;
            wrapped.line(
                &format!("}} catch ({display} {retry_name}) {{"),
                &[(
                    9,
                    display.chars().count(),
                    class_label(ty).context("invalid catch type")?,
                )],
            );
            wrapped.line("}", &[]);
            wrapped.indent -= 1;
            wrapped.line("}", &[]);
            handler_body = wrapped;
        }
        sequence = handler_body.sequence;
        paths.push((index, handler_body, values, terminal));
        headers.push((
            java_type(ty)?,
            name,
            class_label(ty).context("invalid catch type")?,
        ));
    }
    if escape_used.get() {
        ensure!(
            paths.iter().skip(1).all(|path| path.3),
            "protected loop escape has nonterminal catch merge"
        );
    }
    out.sequence = sequence;
    let mut merged = initial;
    merge_path_registers(&mut merged, &mut paths, out, graph, join)?;
    let all_returned = paths.iter().all(|path| path.3);
    if escape_used.get() {
        out.line(&format!("{escape_label}:"), &[]);
    }
    out.line("try {", &[]);
    let mut paths = paths.into_iter();
    out.append(paths.next().context("missing try body")?.1);
    if guarded_dispatch {
        // Throws annotations may be erased from DEX. A broad Java catch with
        // ordered type guards retains the original dispatch without inventing
        // callee declarations or swallowing unmatched exceptions.
        out.line(
            &format!("}} catch (java.lang.Throwable {dispatch_name}) {{"),
            &[],
        );
        out.indent += 1;
        let mut catches_all = false;
        for (index, ((_, body, _, _), (ty, name, label))) in paths.zip(headers).enumerate() {
            let prefix = if index == 0 { "if" } else { "} else if" };
            if ty == "java.lang.Throwable" {
                ensure!(index != 0, "guarded dispatch requires a typed first arm");
                out.line("} else {", &[]);
                catches_all = true;
            } else {
                let head = format!("{prefix} ({dispatch_name} instanceof ");
                out.line(
                    &format!("{head}{ty}) {{"),
                    &[(head.chars().count(), ty.chars().count(), label.clone())],
                );
            }
            out.indent += 1;
            out.line(
                &format!("{ty} {name} = ({ty}) {dispatch_name};"),
                &[(0, ty.chars().count(), label)],
            );
            out.append(body);
            out.indent -= 1;
        }
        if !catches_all {
            out.line("} else {", &[]);
            out.indent += 1;
            out.line(&format!("throw {dispatch_name};"), &[]);
            out.indent -= 1;
        }
        out.line("}", &[]);
        out.indent -= 1;
    } else {
        for ((_, body, _, _), (ty, name, label)) in paths.zip(headers) {
            out.line(
                &format!("}} catch ({ty} {name}) {{"),
                &[(9, ty.chars().count(), label)],
            );
            out.append(body);
        }
    }
    out.line("}", &[]);
    ensure!(
        out.text.len() <= 4 * 1024 * 1024,
        "reconstructed method exceeds output budget"
    );
    Ok((merged, join, all_returned))
}

type RenderedPath = (usize, Output, Vec<Option<Value>>, bool);
fn merge_path_registers(
    regs: &mut [Option<Value>],
    arms: &mut [RenderedPath],
    out: &mut Output,
    graph: &Graph,
    join: usize,
) -> Result<()> {
    validate_wide_frame(regs)?;
    for arm in arms.iter() {
        validate_wide_frame(&arm.2)?;
    }
    let mut r = 0;
    while r < regs.len() {
        if regs[r]
            .as_ref()
            .is_some_and(|value| value.ty == "<wide-tail>")
        {
            r += 1;
            continue;
        }
        let entry_wide = regs[r].as_ref().is_some_and(|value| wide(&value.ty));
        let live: Vec<_> = arms.iter().filter(|arm| !arm.3).collect();
        let potential_wide = entry_wide
            || live
                .iter()
                .any(|arm| arm.2[r].as_ref().is_some_and(|value| wide(&value.ty)));
        let live_here = graph.live_at(join, r) || (potential_wide && graph.live_at(join, r + 1));
        if !live_here {
            regs[r] = None;
            if entry_wide {
                regs[r + 1] = None;
                r += 2;
            } else {
                r += 1;
            }
            continue;
        }
        if live.is_empty() || live.iter().any(|arm| arm.2[r].is_none()) {
            regs[r] = None;
            if entry_wide {
                regs[r + 1] = None;
                r += 2;
            } else {
                r += 1;
            }
            continue;
        }
        let first = live[0].2[r].as_ref().context("missing switch value")?;
        ensure!(first.ty != "<wide-tail>", "wide tail merged without head");
        let merged_wide = wide(&first.ty);
        ensure!(
            live.iter().all(|arm| {
                arm.2[r]
                    .as_ref()
                    .is_some_and(|value| wide(&value.ty) == merged_wide)
            }),
            "wide register width mismatch at control flow join"
        );
        if live.iter().all(|arm| arm.2[r].as_ref() == Some(first))
            && (regs[r].as_ref() == Some(first)
                || first.literal.is_some()
                || first.text.starts_with('"'))
        {
            regs[r] = Some(first.clone());
            if merged_wide {
                regs[r + 1] = Some(Value {
                    text: r.to_string(),
                    ty: "<wide-tail>".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                });
                r += 2;
            } else {
                r += 1;
            }
            continue;
        }
        let mut representative = first.clone();
        for arm in &live[1..] {
            let ty = merge_type(Some(&representative), arm.2[r].as_ref())?;
            if ty != representative.ty {
                representative.ty = ty;
                representative.literal = None;
            }
        }
        let ty = representative.ty;
        let raw_bits32 = matches!(ty.as_str(), "I" | "Z")
            && live.iter().all(|arm| {
                arm.2[r]
                    .as_ref()
                    .is_some_and(|value| value.literal.is_some() || value.raw_bits32)
            });
        let name = format!("v{}", out.sequence);
        out.sequence += 1;
        let display = java_type(&ty)?;
        let refs = class_label(&ty)
            .map(|label| vec![(0, display.chars().count(), label)])
            .unwrap_or_default();
        out.line(&format!("{display} {name};"), &refs);
        for (_, body, values, returned) in arms.iter_mut() {
            if !*returned {
                body.line(
                    &format!(
                        "{name} = {};",
                        argument(
                            values[r].as_ref().context("missing switch merge value")?,
                            &ty
                        )?
                    ),
                    &[],
                );
            }
        }
        assign(
            regs,
            r,
            Value {
                text: name,
                ty,
                literal: None,
                wide_literal: None,
                raw_bits32,
            },
        )?;
        r += if merged_wide { 2 } else { 1 };
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn render_switch(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    pc: usize,
    stop: usize,
    switch: &Switch,
    mut regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
) -> Result<(Vec<Option<Value>>, usize, bool)> {
    ensure!(depth <= 32, "switch nesting exceeds budget");
    let words = &method
        .code
        .as_ref()
        .context("missing switch code")?
        .instructions;
    let selector = integral(&register(&regs, (words[pc] >> 8) as usize)?)?;
    let default = pc + graph.widths[pc];
    let mut starts = vec![default];
    for (_, target) in &switch.cases {
        if !starts.contains(target) {
            starts.push(*target);
        }
    }
    let mut visits = vec![0u16; words.len() + 1];
    for start in &starts {
        let seen = graph.reachable(*start, stop, words)?;
        for (count, seen) in visits.iter_mut().zip(seen) {
            *count += u16::from(seen);
        }
    }
    // Stop at the first overlapping instruction, not merely a join common to
    // every arm. A later arm entry requires a larger closed region below.
    let mut join = visits
        .iter()
        .enumerate()
        .find_map(|(pos, count)| (*count > 1).then_some(pos))
        .unwrap_or(stop);
    {
        // String-switch lowering can send several failed equality tests back
        // to the default selector assignment. It is an acyclic shared prefix,
        // not the switch join. Find a closed region. Each generated case owns
        // its path through that region, so duplicating a shared tail in the
        // source does not duplicate its execution: exactly one case runs.
        join = join.max(*starts.iter().max().unwrap());
        loop {
            let mut next = join;
            for start in &starts {
                if let Some(escape) = graph.region_is_closed(*start, join, words)? {
                    next = next.max(escape);
                }
            }
            ensure!(next <= stop, "switch join crosses enclosing region");
            if next == join {
                break;
            }
            join = next;
        }
        let duplicates_effects = visits.iter().enumerate().any(|(pc, count)| {
            pc < join
                && *count > 1
                && !matches!(words[pc] as u8,
                0x00..=0x09 | 0x12..=0x19 | 0x28..=0x2a | 0x32..=0x3d)
        });
        if duplicates_effects {
            // Normal-flow closure alone cannot establish exception ownership.
            // Keep duplicated protected effects and monitor operations out;
            // their specialized renderers must establish their own boundaries.
            let code = method.code.as_ref().unwrap();
            ensure!(
                code.try_regions.iter().all(|region| {
                    !(region.start as usize..region.end as usize)
                        .any(|pc| pc < join && visits.get(pc).is_some_and(|n| *n > 1))
                }),
                "shared switch tail crosses protected region"
            );
            ensure!(
                visits.iter().enumerate().all(|(pc, count)| {
                    pc >= join || *count <= 1 || !matches!(words[pc] as u8, 0x1d | 0x1e)
                }),
                "shared switch tail crosses monitor boundary"
            );
            ensure!(
                join == words.len() || !matches!(words[join] as u8, 0x0a..=0x0c),
                "shared switch join splits invocation result"
            );
        }
    }
    let mut arms = Vec::new();
    let mut sequence = out.sequence;
    for start in starts {
        ensure!(start <= join, "switch has partially overlapping arms");
        let mut body = Output {
            sequence,
            indent: out.indent + 2,
            ..Default::default()
        };
        let (values, returned) = render(
            class,
            method,
            graph,
            start,
            join,
            regs.clone(),
            &mut body,
            depth,
            true,
            None,
            None,
        )?;
        sequence = body.sequence;
        arms.push((start, body, values, returned));
    }
    out.sequence = sequence;
    merge_path_registers(&mut regs, &mut arms, out, graph, join)?;
    out.line(&format!("switch ({selector}) {{"), &[]);
    let all_returned = arms.iter().all(|arm| arm.3);
    for (start, mut body, _, returned) in arms {
        out.indent += 1;
        for (key, target) in &switch.cases {
            if *target == start {
                out.line(&format!("case {key}:"), &[]);
            }
        }
        if start == default {
            out.line("default:", &[]);
        }
        out.line("{", &[]);
        if !returned {
            body.line("break;", &[]);
        }
        out.append(body);
        out.line("}", &[]);
        out.indent -= 1;
    }
    out.line("}", &[]);
    ensure!(
        out.text.len() <= 4 * 1024 * 1024,
        "reconstructed method exceeds output budget"
    );
    Ok((regs, join, all_returned))
}

#[allow(clippy::too_many_arguments)]
fn render(
    class: &DexClass,
    method: &DexMethod,
    graph: &Graph,
    mut pc: usize,
    stop: usize,
    mut regs: Vec<Option<Value>>,
    out: &mut Output,
    depth: usize,
    mut initialized: bool,
    suppressed_loop: Option<LoopContext<'_>>,
    exception_slots: Option<&[Option<Value>]>,
) -> Result<(Vec<Option<Value>>, bool)> {
    ensure!(depth <= 32, "control flow nesting exceeds budget");
    let words = &method
        .code
        .as_ref()
        .context("method has no code")?
        .instructions;
    if depth == 0
        && let Some(cfg) = &graph.shared_cfg
    {
        let front = graph
            .front_end
            .as_ref()
            .context("shared CFG without front end")?;
        if !graph.shared_loops.is_empty() {
            cfg.validate_decoded_loop(&front.ir, words.len())?;
            ensure!(
                graph.shared_loops == decoded_natural_loops(&front.ir, cfg)?
                    && graph.loops == graph.shared_loops
                    && graph.shared_loop_edges
                        == decoded_all_loop_edges(&front.ir, &graph.shared_loops)?
                    && graph.shared_branches
                        == decoded_loop_branch_plans(
                            &front.ir,
                            cfg,
                            &graph.shared_loops,
                            words.len()
                        )?,
                "shared natural-loop metadata differs from canonical CFG"
            );
        } else {
            cfg.validate_decoded(&front.ir, words.len())?;
            ensure!(
                graph.shared_branches == decoded_branch_plans(&front.ir, cfg, words.len())?,
                "shared diamond plan differs from canonical CFG"
            );
        }
    }
    let constructor = method.name.as_ref() == "<init>";
    let mut returned = false;
    let mut pending: Option<Value> = None;
    let mut allocation: Option<(usize, String)> = None;
    let mut class_captures: Vec<(String, String, Option<String>, Option<String>)> = Vec::new();
    let mut previous_exception_values = regs.clone();
    let mut early_field_writes = false;
    while pc < stop {
        graph.tick()?;
        ensure!(!returned, "instructions after return");
        if let Some(region) = graph.synchronized.iter().find(|region| region.enter == pc) {
            ensure!(
                initialized
                    && pending.is_none()
                    && allocation.is_none()
                    && exception_slots.is_none(),
                "monitor interrupts instruction state"
            );
            ensure!(region.exit < stop, "monitor crosses enclosing region");
            let (values, terminal) =
                synchronized::emit(class, method, graph, region, regs, out, depth)?;
            regs = values;
            if terminal {
                return Ok((regs, true));
            }
            pc = region.exit + 1;
            previous_exception_values.clone_from_slice(&regs);
            continue;
        }
        let candidate_try = method
            .code
            .as_ref()
            .unwrap()
            .try_regions
            .iter()
            .enumerate()
            .find(|(index, region)| {
                (region.start as usize..region.end as usize).contains(&pc)
                    && !graph
                        .synchronized
                        .iter()
                        .any(|monitor| monitor.tries.contains(index))
            });
        // An interleaved catch can occupy addresses inside its own protected
        // interval. Its probe resumes after move-exception, so a plain range
        // check would recursively treat the handler body as a new try suffix.
        let handler_only_entry = if let Some((_, region)) = candidate_try {
            let start = region.start as usize;
            let end = region.end as usize;
            if pc > start
                && region
                    .catches
                    .iter()
                    .any(|(_, handler)| (*handler as usize) < end)
            {
                let normal = graph.reachable(start, words.len(), words)?;
                let mut handler_reaches_pc = false;
                if !normal[pc] {
                    for (_, handler) in region.catches.iter() {
                        if (*handler as usize) < end
                            && graph.reachable(*handler as usize, words.len(), words)?[pc]
                        {
                            handler_reaches_pc = true;
                            break;
                        }
                    }
                }
                !normal[pc] && handler_reaches_pc
            } else {
                false
            }
        } else {
            false
        };
        if exception_slots.is_none()
            && !handler_only_entry
            && let Some((_, region)) = candidate_try
        {
            let split = protected_branch_split(
                graph,
                words,
                pc,
                region.start as usize,
                region.end as usize,
                suppressed_loop.map(|context| context.start),
            )?;
            if split != Some(pc) {
                ensure!(
                    initialized && allocation.is_none() && pending.is_none(),
                    "try interrupts instruction state"
                );
                // A branch may enter a protected suffix after the region's first
                // instruction. Render that suffix with the same DEX handler set.
                // Earlier instructions belong to the other branch and are never
                // replayed on this path.
                let mut suffix = region.clone();
                suffix.start = pc as u32;
                if let Some(split) = split {
                    ensure!(split > pc, "protected branch split at entry");
                    suffix.end = split as u32;
                }
                let try_stop = if suffix.end as usize > stop {
                    if let Some(context) = suppressed_loop {
                        ensure!(
                            suffix.end as usize <= context.exit,
                            "inner try crosses loop exit"
                        );
                        context.exit
                    } else {
                        words.len()
                    }
                } else {
                    stop
                };
                let (values, next, terminal) = render_try(
                    class,
                    method,
                    graph,
                    &suffix,
                    try_stop,
                    regs,
                    out,
                    depth + 1,
                    suppressed_loop,
                )?;
                regs = values;
                if terminal {
                    return Ok((regs, true));
                }
                if next > stop {
                    let (values, terminal) = render(
                        class,
                        method,
                        graph,
                        next,
                        try_stop,
                        regs,
                        out,
                        depth + 1,
                        initialized,
                        suppressed_loop,
                        exception_slots,
                    )?;
                    ensure!(
                        terminal,
                        "inner try suffix does not terminate enclosing branch"
                    );
                    return Ok((values, true));
                }
                pc = next;
                continue;
            }
        }
        if Some(pc) != suppressed_loop.map(|context| context.start)
            && let Some(region) = graph.loops.iter().find(|l| l.start == pc)
        {
            ensure!(
                initialized && pending.is_none() && allocation.is_none(),
                "loop interrupts instruction state"
            );
            ensure!(region.exit <= stop, "loop crosses region boundary");
            // A wholly protected loop may use ordinary loop locals when no
            // handler-visible register is written anywhere in its body. This
            // also covers nested loops. Changing snapshots need a separate
            // exceptional liveness proof and remain unsupported here.
            if let Some(slots) = exception_slots {
                let writes = graph
                    .written_in(method.code.as_ref().unwrap(), region.start, region.exit)
                    .context("unknown protected loop writes")?;
                ensure!(
                    slots
                        .iter()
                        .enumerate()
                        .all(|(r, slot)| slot.is_none() || !writes[r]),
                    "loop writes handler-visible register"
                );
            }
            let before = regs.clone();
            regs = render_loop(
                class,
                method,
                graph,
                *region,
                regs,
                out,
                depth + 1,
                exception_slots.is_some(),
            )?;
            if let Some(slots) = exception_slots {
                for (r, slot) in slots.iter().enumerate() {
                    if slot.is_some() {
                        regs[r] = before[r].clone();
                    }
                }
            }
            if region.exit == words.len() {
                // The unconditional final backedge has no normal successor;
                // a Java while(true) is terminal even if some paths return.
                return Ok((regs, true));
            }
            pc = region.exit;
            continue;
        }
        let w = words[pc];
        let instruction = graph.instruction(pc)?;
        let op = instruction.map_or(w as u8, |insn| insn.opcode);
        // Keep the last local only for a direct return or a candidate static
        // call. The latter must pass the separate adjacent SSA proof below.
        if !matches!(op, 0x0a..=0x0c | 0x0f..=0x11 | 0x71 | 0x77) {
            out.last_local = None;
        }
        let a = (w >> 8) as usize;
        ensure!(
            allocation.is_none() || matches!(op, 0x00..=0x09 | 0x12..=0x19 | 0x1c | 0x70 | 0x76),
            "effectful instruction between allocation and constructor"
        );
        let width = instruction.map_or(graph.widths[pc], |insn| insn.width);
        if op == 0x22
            && initialized
            && pending.is_none()
            && allocation.is_none()
            && exception_slots.is_none()
            && graph.front_end.is_none()
            && graph.shared_cfg.is_none()
            && graph.loops.is_empty()
            && graph.protected.is_empty()
            && graph.synchronized.is_empty()
            && let Some(plan) = concat::plan_at(class, method, pc)
            && plan.end <= stop
            && let Some(value) = regs.get(plan.argument).and_then(Option::as_ref)
            && reference(&value.ty)
            && value.literal.is_none()
            && value.text.strip_prefix('p').is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            })
        {
            let expression = format!("{} + {}", string_literal(&plan.prefix)?, value.text);
            let text = out.local("Ljava/lang/String;", &expression, &[])?;
            assign(&mut regs, plan.result, text)?;
            pc = plan.end;
            previous_exception_values.clone_from_slice(&regs);
            continue;
        }
        if op == 0x1e && synchronized::dispatch_release(graph, pc, &regs, out)? {
            ensure!(
                allocation.is_none() && pending.is_none(),
                "monitor dispatch interrupts instruction state"
            );
            return Ok((regs, true));
        }
        if op == 0x1e
            && graph
                .synchronized
                .iter()
                .any(|region| region.releases.contains(&pc))
        {
            ensure!(
                allocation.is_none() && pending.is_none(),
                "monitor release interrupts instruction state"
            );
            pc += width;
            continue;
        }
        if matches!(op, 0x28..=0x2a) {
            ensure!(
                initialized || constructor,
                "branch before constructor initialization"
            );
            let target = if let Some(instruction) = instruction {
                decoded_goto(instruction)?
            } else {
                graph.targets[pc].context("missing goto target")?
            };
            if let Some(context) = suppressed_loop
                && target == context.start
            {
                ensure!(
                    allocation.is_none(),
                    "continue interrupts instruction state"
                );
                carry_loop_values(context.slots, &regs, out)?;
                out.line("continue;", &[]);
                return Ok((regs, true));
            }
            if let Some(context) = suppressed_loop
                && target == context.exit
            {
                ensure!(allocation.is_none(), "break interrupts instruction state");
                graph.carry_loop_values(context.exit_slots, &regs, out)?;
                out.line("break;", &[]);
                return Ok((regs, true));
            }
            if let Some(context) = suppressed_loop
                && let Some(region) = graph
                    .loops
                    .iter()
                    .find(|region| region.start == context.start)
                && let Some(escape) = graph.protected_escape(
                    target,
                    region.start..region.body_end(&graph.widths),
                    method.code.as_ref().unwrap(),
                )?
            {
                ensure!(
                    allocation.is_none(),
                    "protected escape interrupts instruction state"
                );
                let (_, terminal) = render(
                    class,
                    method,
                    graph,
                    target,
                    escape.end,
                    regs.clone(),
                    out,
                    depth + 1,
                    initialized,
                    None,
                    exception_slots,
                )?;
                ensure!(!terminal, "protected escape terminates before continuation");
                escape.used.set(true);
                out.line(&format!("break {};", escape.label), &[]);
                return Ok((regs, true));
            }
            if let Some(context) = suppressed_loop
                && graph
                    .loops
                    .iter()
                    .find(|region| region.start == context.start)
                    .is_some_and(|region| {
                        !(region.start..region.body_end(&graph.widths)).contains(&target)
                            && graph
                                .non_reentering_tail(
                                    target,
                                    region.start..region.body_end(&graph.widths),
                                    words,
                                )
                                .unwrap_or(false)
                    })
            {
                ensure!(allocation.is_none(), "invalid loop return");
                let result = render(
                    class,
                    method,
                    graph,
                    target,
                    words.len(),
                    regs,
                    out,
                    depth + 1,
                    initialized,
                    None,
                    exception_slots,
                )?;
                ensure!(result.1, "loop escape does not terminate its region");
                return Ok(result);
            }
            ensure!(
                suppressed_loop.is_none_or(|context| target >= context.start),
                "unproven backward loop escape"
            );
            ensure!(
                target > pc || graph.acyclic_backwards.contains(&pc),
                "unstructured backward jump"
            );
            ensure!(target <= stop, "goto crosses region boundary");
            pc = target;
            pending = None;
            continue;
        }
        if let Some(switch) = &graph.switches[pc] {
            ensure!(
                exception_slots.is_none(),
                "switch inside try not reconstructed"
            );
            ensure!(initialized, "switch before constructor initialization");
            let (next_regs, next_pc, all_returned) =
                render_switch(class, method, graph, pc, stop, switch, regs, out, depth + 1)?;
            regs = next_regs;
            if all_returned {
                return Ok((regs, true));
            }
            pc = next_pc;
            pending = None;
            continue;
        }
        if matches!(op, 0x32..=0x3d) {
            ensure!(
                initialized || constructor,
                "branch before constructor initialization"
            );
            let target = if let Some(edge) = graph.shared_loop_edge(pc) {
                edge.taken
            } else if let Some(diamond) = graph.shared_branch(pc)? {
                ensure!(diamond.branch == pc, "condition outside shared diamond");
                diamond.taken
            } else {
                graph.targets[pc].context("missing branch target")?
            };
            if let Some(context) = suppressed_loop
                && target == context.start
            {
                ensure!(
                    allocation.is_none(),
                    "continue interrupts instruction state"
                );
                let test = graph.test(words, pc, false, &regs)?;
                out.line(&format!("if ({test}) {{"), &[]);
                out.indent += 1;
                graph.carry_loop_values(context.slots, &regs, out)?;
                out.line("continue;", &[]);
                out.indent -= 1;
                out.line("}", &[]);
                pending = None;
                pc = graph
                    .shared_loop_edge(pc)
                    .map_or(pc + width, |edge| edge.fallthrough);
                continue;
            }
            if let Some(context) = suppressed_loop
                && let Some(edge) = graph.shared_loop_edge(pc)
                && edge.owner == context.start
                && edge.kind == SharedLoopEdgeKind::Terminal
            {
                ensure!(
                    allocation.is_none() && target == edge.taken,
                    "invalid shared terminal escape"
                );
                let test = graph.test(words, pc, false, &regs)?;
                out.line(&format!("if ({test}) {{"), &[]);
                out.indent += 1;
                let (_, terminal) = render(
                    class,
                    method,
                    graph,
                    target,
                    target + 1,
                    regs.clone(),
                    out,
                    depth + 1,
                    initialized,
                    None,
                    exception_slots,
                )?;
                ensure!(terminal, "shared terminal escape does not return");
                out.indent -= 1;
                out.line("}", &[]);
                pending = None;
                pc = edge.fallthrough;
                continue;
            }
            if let Some(context) = suppressed_loop
                && target == context.exit
            {
                ensure!(allocation.is_none(), "break interrupts instruction state");
                let test = graph.test(words, pc, false, &regs)?;
                out.line(&format!("if ({test}) {{"), &[]);
                out.indent += 1;
                graph.carry_loop_values(context.exit_slots, &regs, out)?;
                out.line("break;", &[]);
                out.indent -= 1;
                out.line("}", &[]);
                pending = None;
                pc = graph
                    .shared_loop_edge(pc)
                    .map_or(pc + width, |edge| edge.fallthrough);
                continue;
            }
            if let Some(context) = suppressed_loop
                && let Some(region) = graph
                    .loops
                    .iter()
                    .find(|region| region.start == context.start)
                && let Some(escape) = graph.protected_escape(
                    target,
                    region.start..region.body_end(&graph.widths),
                    method.code.as_ref().unwrap(),
                )?
            {
                ensure!(
                    allocation.is_none(),
                    "protected escape interrupts instruction state"
                );
                let test = condition(op, a, &regs)?;
                out.line(&format!("if ({test}) {{"), &[]);
                out.indent += 1;
                let (_, terminal) = render(
                    class,
                    method,
                    graph,
                    target,
                    escape.end,
                    regs.clone(),
                    out,
                    depth + 1,
                    initialized,
                    None,
                    exception_slots,
                )?;
                ensure!(!terminal, "protected escape terminates before continuation");
                escape.used.set(true);
                out.line(&format!("break {};", escape.label), &[]);
                out.indent -= 1;
                out.line("}", &[]);
                pending = None;
                pc += width;
                continue;
            }
            if let Some(context) = suppressed_loop
                && graph
                    .loops
                    .iter()
                    .find(|region| region.start == context.start)
                    .is_some_and(|region| {
                        !(region.start..region.body_end(&graph.widths)).contains(&target)
                            && graph
                                .non_reentering_tail(
                                    target,
                                    region.start..region.body_end(&graph.widths),
                                    words,
                                )
                                .unwrap_or(false)
                    })
            {
                ensure!(allocation.is_none(), "invalid loop return");
                let test = condition(op, a, &regs)?;
                out.line(&format!("if ({test}) {{"), &[]);
                out.indent += 1;
                let (_, terminal) = render(
                    class,
                    method,
                    graph,
                    target,
                    words.len(),
                    regs.clone(),
                    out,
                    depth + 1,
                    initialized,
                    None,
                    exception_slots,
                )?;
                ensure!(terminal, "loop escape does not terminate its region");
                out.indent -= 1;
                out.line("}", &[]);
                pending = None;
                pc += width;
                continue;
            }
            ensure!(
                suppressed_loop.is_none_or(|context| target >= context.start),
                "unproven backward loop escape"
            );
            ensure!(
                (target > pc || graph.acyclic_backwards.contains(&pc)) && target <= stop,
                "branch crosses region boundary"
            );
            let mut branch_regs = regs.clone();
            if !initialized {
                for value in &mut branch_regs {
                    if value.as_ref().is_some_and(|value| value.text == "this") {
                        *value = None;
                    }
                }
            }
            let (condition, inverse, fallthrough, join) = if let Some(diamond) =
                graph.shared_branch(pc)?
            {
                let instruction = instruction.context("shared diamond lacks decoded condition")?;
                let (left, right) = decoded_condition_sources(instruction, regs.len())?;
                ensure!(diamond.join <= stop, "shared diamond join crosses region");
                (
                    condition_sources(op, left, right, &branch_regs)?,
                    condition_sources(op ^ 1, left, right, &branch_regs)?,
                    diamond.fallthrough,
                    diamond.join,
                )
            } else {
                (
                    condition(op, a, &branch_regs)?,
                    self::condition(op ^ 1, a, &branch_regs)?,
                    pc + width,
                    graph.join(pc + width, target, stop, words)?,
                )
            };
            if !initialized {
                // Keep initialization outside both arms. Masking rejects any
                // reads/escapes/delegation through this, and this write proof
                // prevents resurrecting a overwritten or invalidated receiver.
                let written = super::liveness::written_in(
                    method.code.as_ref().unwrap(),
                    (pc + width).min(target),
                    join,
                )
                .context("constructor branch writes unknown")?;
                ensure!(
                    regs.iter().enumerate().all(|(index, value)| value
                        .as_ref()
                        .is_none_or(|value| value.text != "this")
                        || !written[index]),
                    "constructor branch overwrites receiver"
                );
            }
            let mut yes = Output {
                sequence: out.sequence,
                indent: out.indent + 1,
                ..Default::default()
            };
            let (mut yes_regs, yes_return) = render(
                class,
                method,
                graph,
                target,
                join,
                branch_regs.clone(),
                &mut yes,
                depth + 1,
                initialized,
                suppressed_loop,
                exception_slots,
            )?;
            let mut no = Output {
                sequence: yes.sequence,
                indent: out.indent + 1,
                ..Default::default()
            };
            let (mut no_regs, no_return) = render(
                class,
                method,
                graph,
                fallthrough,
                join,
                branch_regs,
                &mut no,
                depth + 1,
                initialized,
                suppressed_loop,
                exception_slots,
            )?;
            if !initialized {
                for (index, value) in regs.iter().enumerate() {
                    if value.as_ref().is_some_and(|value| value.text == "this") {
                        ensure!(
                            yes_regs[index].is_none() && no_regs[index].is_none(),
                            "masked branch receiver replaced"
                        );
                        yes_regs[index] = value.clone();
                        no_regs[index] = value.clone();
                    }
                }
            }
            out.sequence = no.sequence;
            validate_wide_frame(&regs)?;
            validate_wide_frame(&yes_regs)?;
            validate_wide_frame(&no_regs)?;
            let mut r = 0;
            while r < regs.len() {
                if regs[r]
                    .as_ref()
                    .is_some_and(|value| value.ty == "<wide-tail>")
                {
                    r += 1;
                    continue;
                }
                let entry_wide = regs[r].as_ref().is_some_and(|value| wide(&value.ty));
                let left = if yes_return {
                    None
                } else {
                    yes_regs[r].as_ref()
                };
                let right = if no_return { None } else { no_regs[r].as_ref() };
                let potential_wide = entry_wide
                    || left.is_some_and(|value| wide(&value.ty))
                    || right.is_some_and(|value| wide(&value.ty));
                if !(graph.live_at(join, r) || (potential_wide && graph.live_at(join, r + 1))) {
                    regs[r] = None;
                    if entry_wide {
                        regs[r + 1] = None;
                        r += 2;
                    } else {
                        r += 1;
                    }
                    continue;
                }
                let next = match (left, right) {
                    (None, None) => None,
                    (Some(v), None) if no_return => Some(v),
                    (None, Some(v)) if yes_return => Some(v),
                    (Some(v), Some(w)) if v == w => Some(v),
                    (Some(_), Some(_)) => None,
                    _ => {
                        regs[r] = None;
                        if entry_wide {
                            regs[r + 1] = None;
                            r += 2;
                        } else {
                            r += 1;
                        }
                        continue;
                    }
                };
                if let Some(value) = next {
                    ensure!(value.ty != "<wide-tail>", "wide tail merged without head");
                }
                let merged_wide = left.or(right).is_some_and(|value| wide(&value.ty));
                ensure!(
                    left.is_none_or(|value| wide(&value.ty) == merged_wide)
                        && right.is_none_or(|value| wide(&value.ty) == merged_wide),
                    "wide register width mismatch at control flow join"
                );
                if let Some(v) = next
                    && (regs[r].as_ref() == Some(v)
                        || v.literal.is_some()
                        || v.text.starts_with('"'))
                {
                    regs[r] = Some(v.clone());
                    if merged_wide {
                        regs[r + 1] = Some(Value {
                            text: r.to_string(),
                            ty: "<wide-tail>".into(),
                            literal: None,
                            wide_literal: None,
                            raw_bits32: false,
                        });
                        r += 2;
                    } else {
                        r += 1;
                    }
                    continue;
                }
                if left.is_none() && right.is_none() {
                    regs[r] = None;
                    if entry_wide {
                        regs[r + 1] = None;
                        r += 2;
                    } else {
                        r += 1;
                    }
                    continue;
                }
                let ty = merge_type(left, right)?;
                let raw_bits32 = matches!(ty.as_str(), "I" | "Z")
                    && left
                        .into_iter()
                        .chain(right)
                        .all(|value| value.literal.is_some() || value.raw_bits32);
                let name = format!("v{}", out.sequence);
                out.sequence += 1;
                let display = java_type(&ty)?;
                let refs = class_label(&ty)
                    .map(|label| vec![(0, display.chars().count(), label)])
                    .unwrap_or_default();
                out.line(&format!("{display} {name};"), &refs);
                if let Some(v) = left {
                    yes.line(&format!("{name} = {};", argument(v, &ty)?), &[]);
                }
                if let Some(v) = right {
                    no.line(&format!("{name} = {};", argument(v, &ty)?), &[]);
                }
                assign(
                    &mut regs,
                    r,
                    Value {
                        text: name,
                        ty,
                        literal: None,
                        wide_literal: None,
                        raw_bits32,
                    },
                )?;
                r += if merged_wide { 2 } else { 1 };
            }
            if yes.text.is_empty() {
                out.line(&format!("if ({inverse}) {{"), &[]);
                out.append(no);
                out.line("}", &[]);
            } else if no.text.is_empty() {
                out.line(&format!("if ({condition}) {{"), &[]);
                out.append(yes);
                out.line("}", &[]);
            } else {
                out.line(&format!("if ({condition}) {{"), &[]);
                out.append(yes);
                out.line("} else {", &[]);
                out.append(no);
                out.line("}", &[]);
            }
            ensure!(
                out.text.len() <= 4 * 1024 * 1024,
                "reconstructed method exceeds output budget"
            );
            if yes_return && no_return {
                return Ok((regs, true));
            }
            pc = join;
            previous_exception_values.clone_from_slice(&regs);
            pending = None;
            continue;
        }
        if !matches!(op, 0x0a..=0x0c) {
            pending = None;
        }
        match op {
            0x00 => ensure!(w == 0, "payload in straight-line body"),
            0x01..=0x09 => {
                let (dst, src) = if let Some(instruction) = instruction {
                    decoded_move(instruction, regs.len())?
                } else {
                    match (op - 1) % 3 {
                        0 => (a & 15, a >> 4),
                        1 => (a, words[pc + 1] as usize),
                        _ => (words[pc + 1] as usize, words[pc + 2] as usize),
                    }
                };
                let value = register(&regs, src)?;
                let kind = (op - 1) / 3;
                ensure!(
                    match kind {
                        0 => !reference(&value.ty) && !matches!(value.ty.as_str(), "J" | "D"),
                        1 => matches!(value.ty.as_str(), "J" | "D"),
                        _ => reference(&value.ty) || value.literal == Some(0),
                    },
                    "move type mismatch at {pc:04x}: register v{src} has type {}",
                    value.ty
                );
                ensure!(
                    dst == src
                        || !value.text.starts_with("<class:")
                        || exception_slots
                            .is_none_or(|slots| slots.get(dst).is_none_or(Option::is_none)),
                    "deferred class alias writes exception-visible register"
                );
                assign(&mut regs, dst, value)?;
            }
            0x0a..=0x0c => {
                let dst = if let Some(instruction) = instruction {
                    decoded_result_destination(instruction, regs.len())?
                } else {
                    a
                };
                if let Some(front) = &graph.front_end {
                    let index = front.ir.instructions.partition_point(|insn| insn.pc < pc);
                    let producer = index
                        .checked_sub(1)
                        .and_then(|index| front.ir.instructions.get(index))
                        .context("missing shared move-result producer")?;
                    let call_index = front
                        .bound
                        .calls
                        .binary_search_by_key(&producer.pc, |call| call.pc)
                        .map_err(|_| anyhow::anyhow!("missing shared move-result binding"))?;
                    let result = front.bound.calls[call_index]
                        .result
                        .as_ref()
                        .context("missing shared bound result")?;
                    ensure!(
                        result.move_pc == pc && usize::from(result.register.register) == dst,
                        "shared move-result destination differs from bound result"
                    );
                }
                let value = pending.take().context("move-result without invoke")?;
                ensure!(
                    match op {
                        0x0a => !reference(&value.ty) && !matches!(value.ty.as_str(), "J" | "D"),
                        0x0b => matches!(value.ty.as_str(), "J" | "D"),
                        _ => reference(&value.ty),
                    },
                    "result opcode type mismatch"
                );
                assign(&mut regs, dst, value)?;
            }
            0x0d => bail!("move-exception outside handled entry"),
            0x27 => {
                ensure!(allocation.is_none(), "throw interrupts allocation");
                let throwing_prologue = !initialized
                    && constructor
                    && (class.superclass.as_deref() == Some("Ljava/lang/Object;")
                        || class
                            .symbols
                            .hierarchy
                            .get()
                            .is_some_and(|h| h.has_accessible_noarg_super(&class.descriptor)))
                    && method.code.as_ref().unwrap().try_regions.is_empty()
                    && graph.targets.iter().all(Option::is_none)
                    && graph.switches.iter().all(Option::is_none);
                ensure!(
                    initialized || throwing_prologue,
                    "throw before initialization: implicit super() would change construction/finalization effects"
                );
                let src = if let Some(instruction) = instruction {
                    decoded_throw_source(instruction, regs.len())?
                } else {
                    a
                };
                let value = register(&regs, src)?;
                // Java precise rethrow preserves the exception from this catch.
                // Only unchanged catch identities qualify, not arbitrary Throwable values.
                let expression = if graph.caught_values.borrow().contains(&value.text) {
                    graph
                        .catch_rethrows
                        .borrow()
                        .get(&value.text)
                        .cloned()
                        .unwrap_or_else(|| value.text.clone())
                } else {
                    match throwing::expression(class, method, &value) {
                        Ok(expression) => expression,
                        Err(error) => {
                            if throwing::locally_caught(class, method, pc, &value) {
                                value.text.clone()
                            } else if throwing::may_infer_declaration(class, method, &value) {
                                out.inferred_throws.insert(value.ty.clone());
                                value.text.clone()
                            } else {
                                return Err(error);
                            }
                        }
                    }
                };
                if throwing_prologue {
                    // Java 25 keeps the delegation source-reachable but emits
                    // no superclass invocation for this always-throw prologue.
                    out.line("if (true) {", &[]);
                    out.indent += 1;
                    out.line(&format!("throw {expression};"), &[]);
                    out.indent -= 1;
                    out.line("}", &[]);
                    out.line("super();", &[]);
                } else {
                    out.line(&format!("throw {expression};"), &[]);
                }
                returned = true;
            }
            0x0e => {
                if let Some(instruction) = instruction {
                    ensure!(
                        decoded_return_source(instruction, regs.len())?.is_none(),
                        "invalid shared void return"
                    );
                }
                ensure!(
                    method.return_type.as_ref() == "V" && initialized,
                    "invalid void return"
                );
                out.line("return;", &[]);
                returned = true;
            }
            0x0f..=0x11 => {
                let src = if let Some(instruction) = instruction {
                    decoded_return_source(instruction, regs.len())?
                        .context("missing shared return source")?
                } else {
                    a
                };
                ensure!(
                    initialized && method.return_type.as_ref() != "V",
                    "invalid return"
                );
                ensure!(
                    (op == 0x11) == reference(&method.return_type)
                        && (op == 0x10) == wide(&method.return_type),
                    "return opcode type mismatch"
                );
                let value = register(&regs, src)?;
                let converted = argument_from_register(
                    &value,
                    &method.return_type,
                    src,
                    method.code.as_ref().unwrap(),
                    graph,
                );
                let expr = match converted {
                    Ok(expr) => expr,
                    Err(original)
                        if method.return_type.as_ref() == "Z"
                            && value.ty == "I"
                            && value.literal.is_none()
                            && !value.raw_bits32 =>
                    {
                        if proven_boolean_return(class, method, graph, pc, src)? {
                            format!("({} != 0)", value.text)
                        } else {
                            return Err(original);
                        }
                    }
                    Err(original) => return Err(original),
                };
                if exception_slots.is_some()
                    && reference(&method.return_type)
                    && !method
                        .code
                        .as_ref()
                        .unwrap()
                        .try_regions
                        .iter()
                        .any(|region| region.start as usize <= pc && pc < region.end as usize)
                {
                    // An absorbed DEX return cannot throw. Do not introduce a
                    // potentially failing Java downcast under a new catch.
                    ensure!(
                        expr == value.text
                            || expr == "null"
                            || method.return_type.as_ref() == "Ljava/lang/Object;"
                            || class.symbols.hierarchy.get().is_some_and(|hierarchy| {
                                hierarchy.assignable(&value.ty, &method.return_type)
                                    == crate::native_hierarchy::Relation::Proven
                            }),
                        "return tail requires a potentially throwing conversion"
                    );
                }
                out.return_value(&value, &expr, &method.return_type);
                returned = true;
            }
            0x12..=0x15 => {
                let (dst, n) = if let Some(instruction) = instruction {
                    let (dst, literal) = decoded_constant(instruction, regs.len())?;
                    (
                        dst,
                        i32::try_from(literal).context("shared constant exceeds 32 bits")?,
                    )
                } else {
                    match op {
                        0x12 => (a & 15, ((w as i16) >> 12) as i32),
                        0x13 => (a, words[pc + 1] as i16 as i32),
                        0x14 => (
                            a,
                            (words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32,
                        ),
                        _ => (a, (words[pc + 1] as i32) << 16),
                    }
                };
                assign(
                    &mut regs,
                    dst,
                    Value {
                        text: n.to_string(),
                        ty: "I".into(),
                        literal: Some(n),
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )?;
            }
            0x16..=0x19 => {
                let (dst, bits) = if let Some(instruction) = instruction {
                    let (dst, literal) = decoded_constant(instruction, regs.len())?;
                    (dst, literal as u64)
                } else {
                    (
                        a,
                        match op {
                            0x16 => words[pc + 1] as i16 as i64 as u64,
                            0x17 => {
                                (words[pc + 1] as u32 | ((words[pc + 2] as u32) << 16)) as i32
                                    as i64 as u64
                            }
                            0x18 => (0..4).fold(0u64, |bits, i| {
                                bits | ((words[pc + 1 + i] as u64) << (16 * i))
                            }),
                            _ => (words[pc + 1] as u64) << 48,
                        },
                    )
                };
                assign(
                    &mut regs,
                    dst,
                    Value {
                        text: format!("{}L", bits as i64),
                        ty: "J".into(),
                        literal: None,
                        wide_literal: Some(bits),
                        raw_bits32: false,
                    },
                )?;
            }
            0x2d..=0x31 => {
                let spec = numeric::Compare::decode(op).context("comparison opcode")?;
                let (dst, left, right) = if let Some(instruction) = instruction {
                    let (dst, sources, _) = decoded_arithmetic(instruction, regs.len())?;
                    (dst, sources[0], sources[1])
                } else {
                    let packed = words[pc + 1];
                    (a, (packed & 255) as usize, (packed >> 8) as usize)
                };
                let lhs = argument(&register(&regs, left)?, spec.input.descriptor())?;
                let rhs = argument(&register(&regs, right)?, spec.input.descriptor())?;
                let value = out.local("I", &spec.expression(&lhs, &rhs), &[])?;
                assign(&mut regs, dst, value)?;
            }
            0x1a | 0x1b => {
                let (dst, index) = if let Some(instruction) = instruction {
                    decoded_reference_constant(instruction, regs.len())?
                } else {
                    let mut index = words[pc + 1] as usize;
                    if op == 0x1b {
                        index |= (words[pc + 2] as usize) << 16;
                    }
                    (a, index)
                };
                let s = class.symbols.strings.get(index).context("string index")?;
                assign(
                    &mut regs,
                    dst,
                    Value {
                        text: string_literal(s)?,
                        ty: "Ljava/lang/String;".into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )?;
            }
            0x1c if allocation.is_some() => {
                let ty = class
                    .symbols
                    .types
                    .get(words[pc + 1] as usize)
                    .context("class literal type")?;
                let display = java_type(ty)?;
                let snapshot =
                    if let Some(Some(slot)) = exception_slots.and_then(|slots| slots.get(a)) {
                        ensure!(
                            slot.ty == "Ljava/lang/Class;",
                            "class capture changes exception register type"
                        );
                        Some(slot.text.clone())
                    } else {
                        None
                    };
                let name = format!("v{}", out.sequence);
                out.sequence += 1;
                out.line(&format!("java.lang.Class {name};"), &[]);
                let index = class_captures.len();
                class_captures.push((name, display, class_label(ty), snapshot));
                assign(
                    &mut regs,
                    a,
                    Value {
                        text: format!("<class:{index}>"),
                        ty: "Ljava/lang/Class;".into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )?;
            }
            0x8d..=0x8f if instruction.is_some() => {
                let (dst, sources, _) = decoded_arithmetic(instruction.unwrap(), regs.len())?;
                let input = register(&regs, sources[0])?;
                ensure!(
                    initialized || input.text != "this",
                    "uninitialized this used by array/type operation"
                );
                let ty = ["B", "C", "S"][(op - 0x8d) as usize];
                let value = out.local(
                    ty,
                    &format!("({}) ({})", java_type(ty)?, integral(&input)?),
                    &[],
                )?;
                assign(&mut regs, dst, value)?;
            }
            0x1c | 0x1f..=0x21 | 0x23 | 0x44..=0x51 | 0x8d..=0x8f => {
                let operands = if let Some(instruction) = instruction {
                    if op == 0x1c {
                        let (dst, index) = decoded_reference_constant(instruction, regs.len())?;
                        operations::Operands {
                            dst,
                            reads: [0; 3],
                            read_count: 0,
                            type_index: Some(index),
                        }
                    } else {
                        decoded_array_type(instruction, regs.len())?
                    }
                } else {
                    operations::Operands::legacy(op, a, if width > 1 { words[pc + 1] } else { 0 })
                };
                if !initialized {
                    let inputs = operands.guarded_inputs(op);
                    for &input in &inputs[..operands.read_count] {
                        ensure!(
                            register(&regs, input)?.text != "this",
                            "uninitialized this used by array/type operation"
                        );
                    }
                }
                operations::emit(class, op, operands, &mut regs, out)?;
            }
            0x24 | 0x25 => {
                if !initialized {
                    let packed = words[pc + 2];
                    let inputs: Vec<usize> = if op == 0x25 {
                        (packed as usize..packed as usize + a).collect()
                    } else {
                        let count = a >> 4;
                        ensure!(count <= 5, "filled-array register count");
                        let registers = [
                            (packed & 15) as usize,
                            ((packed >> 4) & 15) as usize,
                            ((packed >> 8) & 15) as usize,
                            (packed >> 12) as usize,
                            a & 15,
                        ];
                        registers[..count].to_vec()
                    };
                    for input in inputs {
                        ensure!(
                            register(&regs, input)?.text != "this",
                            "uninitialized this escapes through filled array"
                        );
                    }
                }
                pending = Some(operations::filled(
                    class,
                    op,
                    a,
                    words[pc + 1],
                    words[pc + 2],
                    &regs,
                    out,
                )?);
            }
            0x26 => {
                let offset = i32::from_le_bytes([
                    words[pc + 1] as u8,
                    (words[pc + 1] >> 8) as u8,
                    words[pc + 2] as u8,
                    (words[pc + 2] >> 8) as u8,
                ]);
                let payload = usize::try_from(pc as i64 + i64::from(offset))
                    .context("array payload outside method")?;
                operations::fill_array(words, payload, &register(&regs, a)?, out)?;
            }
            0x22 => {
                // The allocation decoder must not read or capture uninitialized
                // this, including any aliases. Mask those inputs transactionally.
                let prologue_regs = (!initialized).then(|| {
                    regs.iter()
                        .map(|value| value.clone().filter(|value| value.text != "this"))
                        .collect::<Vec<_>>()
                });
                if (initialized || constructor)
                    // Staging is safe for handler state only when no entry
                    // register needs a mutable exception snapshot.
                    && exception_slots.is_none_or(|slots| slots.iter().all(Option::is_none))
                    && let Some(lowered) = allocation_lowering::try_lower(
                        class, method, graph, words, pc, stop,
                        prologue_regs.as_deref().unwrap_or(&regs), out,
                    )?
                    && method.code.as_ref().is_some_and(|code| code.try_regions.iter().all(|region| {
                        let start = region.start as usize;
                        let end = region.end as usize;
                        end <= pc || start >= lowered.next_pc || (start <= pc && lowered.next_pc <= end)
                    }))
                {
                    let mut values = lowered.regs;
                    if !initialized {
                        let written = super::liveness::written_in(
                            method.code.as_ref().unwrap(),
                            pc,
                            lowered.next_pc,
                        )
                        .context("constructor allocation register writes unknown")?;
                        for (index, original) in regs.iter().enumerate() {
                            if original.as_ref().is_some_and(|value| value.text == "this")
                                && !written[index]
                            {
                                ensure!(
                                    values[index].is_none(),
                                    "masked constructor receiver replaced"
                                );
                                values[index] = original.clone();
                            }
                        }
                    }
                    regs = values;
                    out.sequence = lowered.out.sequence;
                    out.append(lowered.out);
                    pc = lowered.next_pc;
                    continue;
                }
                ensure!(
                    exception_slots.is_none_or(|slots| slots.get(a).is_none_or(Option::is_none)),
                    "allocation overwrites exception-visible register"
                );
                // Independent allocations are valid Java 25 constructor prologue
                // statements. Their arguments must not expose the receiver.
                ensure!(
                    initialized || constructor,
                    "allocation before constructor initialization"
                );
                let ty = class
                    .symbols
                    .types
                    .get(words[pc + 1] as usize)
                    .context("allocation type")?;
                ensure!(ty.starts_with('L'), "new-instance requires class");
                java_type(ty)?;
                assign(
                    &mut regs,
                    a,
                    Value {
                        text: "<uninitialized>".into(),
                        ty: ty.to_string(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )?;
                allocation = Some((a, ty.to_string()));
            }
            0x52..=0x6d => {
                let (reg, receiver_reg, index) = if let Some(instruction) = instruction {
                    decoded_field(instruction, regs.len())?
                } else {
                    (
                        if op >= 0x60 { a } else { a & 15 },
                        if op >= 0x60 { None } else { Some(a >> 4) },
                        words[pc + 1] as usize,
                    )
                };
                let &(owner, ty, name) = class.symbols.fields.get(index).context("field index")?;
                let owner = class
                    .symbols
                    .types
                    .get(owner as usize)
                    .context("field owner")?;
                let ty = class.symbols.types.get(ty as usize).context("field type")?;
                let name = class
                    .symbols
                    .strings
                    .get(name as usize)
                    .context("field name")?;
                let display_name = names::member(name)?;
                let family = if op >= 0x67 {
                    op - 0x67
                } else if op >= 0x60 {
                    op - 0x60
                } else if op >= 0x59 {
                    op - 0x59
                } else {
                    op - 0x52
                };
                ensure!(
                    match family {
                        0 => matches!(ty.as_ref(), "I" | "F"),
                        1 => wide(ty),
                        2 => reference(ty),
                        3 => ty.as_ref() == "Z",
                        4 => ty.as_ref() == "B",
                        5 => ty.as_ref() == "C",
                        6 => ty.as_ref() == "S",
                        _ => false,
                    },
                    "field opcode type mismatch"
                );
                let is_static = op >= 0x60;
                let put = if is_static { op >= 0x67 } else { op >= 0x59 };
                let independent_early_field = constructor
                    && if is_static {
                        !put
                    } else {
                        register(&regs, receiver_reg.context("missing field receiver")?)?.text
                            != "this"
                            && (!put || register(&regs, reg)?.text != "this")
                    };
                if !(initialized || independent_early_field) {
                    // Independent static reads do not touch the uninitialized receiver.
                    // Emit them in DEX order; single-use adjacent delegation arguments
                    // can be inlined later without repeating or moving a field read.
                    // Java 25 early construction permits assignments to fields
                    // declared in this class, but forbids reading/escaping this.
                    // Keep DEX order: superclass callbacks may observe the write.
                    ensure!(
                        constructor
                            && !is_static
                            && put
                            && owner.as_ref() == class.descriptor.as_ref()
                            && register(&regs, receiver_reg.context("missing field receiver")?)?
                                .text
                                == "this"
                            && class.fields.iter().any(|field| !field.is_static
                                && field.declaring_type == class.descriptor
                                && field.name.as_ref() == name
                                && field.field_type.as_ref() == ty.as_ref()),
                        "unsupported field access before constructor initialization"
                    );
                    ensure!(
                        register(&regs, reg)?.text != "this",
                        "uninitialized this escapes through field value"
                    );
                    early_field_writes = true;
                }
                let target = if is_static {
                    java_type(owner)?
                } else {
                    receiver(
                        &register(&regs, receiver_reg.context("missing field receiver")?)?,
                        owner,
                    )?
                };
                // Own blank-final fields require a simple assignment name in
                // Java. A field can also hide the class/package qualifier.
                // Do not use a bare name that could bind a generated local.
                let declared_field = class.fields.iter().find(|field| {
                    field.is_static
                        && field.declaring_type == class.descriptor
                        && field.name.as_ref() == name.as_str()
                        && field.field_type.as_ref() == ty.as_ref()
                });
                let initializer_needs_bare = is_static
                    && put
                    && method.name.as_ref() == "<clinit>"
                    && owner == &class.descriptor
                    && declared_field.is_some()
                    && (declared_field.is_some_and(|field| field.access_flags & 0x10 != 0)
                        || class.fields.iter().any(|field| {
                            names::member(&field.name).is_ok_and(|field_name| {
                                Some(field_name.as_str()) == target.split('.').next()
                                    || Some(field_name.as_str()) == target.rsplit('.').next()
                            })
                        }));
                let generated_collision =
                    ["v", "e", "caught", "monitorExit"].iter().any(|prefix| {
                        if *prefix != "v"
                            && method
                                .code
                                .as_ref()
                                .is_some_and(|code| code.try_regions.is_empty())
                        {
                            return false;
                        }
                        display_name.strip_prefix(prefix).is_some_and(|suffix| {
                            !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                        })
                    });
                ensure!(
                    !initializer_needs_bare || !generated_collision,
                    "initializer field collides with generated local namespace"
                );
                let bare_initializer_field = initializer_needs_bare;
                let expr = if bare_initializer_field {
                    display_name.clone()
                } else {
                    format!("{target}.{display_name}")
                };
                let label = format!(
                    "{}.{}:{}",
                    owner
                        .trim_start_matches('L')
                        .trim_end_matches(';')
                        .replace('/', "."),
                    name,
                    ty
                );
                let mut refs = vec![(
                    if bare_initializer_field {
                        0
                    } else {
                        target.chars().count() + 1
                    },
                    display_name.chars().count(),
                    label,
                )];
                if is_static && !bare_initializer_field {
                    refs.push((
                        0,
                        target.chars().count(),
                        class_label(owner).context("invalid owner")?,
                    ));
                }
                if put {
                    let value = argument_from_register(
                        &register(&regs, reg)?,
                        ty,
                        reg,
                        method.code.as_ref().unwrap(),
                        graph,
                    )?;
                    out.line(&format!("{expr} = {value};"), &refs);
                } else {
                    let value = out.local(ty, &expr, &refs)?;
                    assign(&mut regs, reg, value)?;
                }
            }
            0x6e..=0x72 | 0x74..=0x78 => {
                let kind = if op >= 0x74 { op - 6 } else { op };
                let static_call = kind == 0x71;
                let inputs: Vec<usize> = if let Some(insn) = instruction {
                    insn.reads
                        .iter()
                        .map(|input| usize::from(input.register))
                        .collect()
                } else if op >= 0x74 {
                    (words[pc + 2] as usize..words[pc + 2] as usize + a).collect()
                } else {
                    let n = a >> 4;
                    ensure!(n <= 5, "invoke register count");
                    let packed = words[pc + 2];
                    let all = [
                        (packed & 15) as usize,
                        ((packed >> 4) & 15) as usize,
                        ((packed >> 8) & 15) as usize,
                        ((packed >> 12) & 15) as usize,
                        a & 15,
                    ];
                    all[..n].to_vec()
                };
                let local_binding;
                let call = if let Some(front) = &graph.front_end {
                    let index = front
                        .bound
                        .calls
                        .binary_search_by_key(&pc, |call| call.pc)
                        .map_err(|_| {
                            anyhow::anyhow!("missing shared invocation binding at {pc}")
                        })?;
                    &front.bound.calls[index]
                } else {
                    local_binding = crate::native_calls::bind_invocation(
                        op,
                        pc,
                        u32::from(words[pc + 1]),
                        &inputs,
                        &class.symbols,
                    )?;
                    &local_binding
                };
                let crate::native_calls::CallTarget::Method {
                    declaring_type: owner,
                    name,
                    prototype_index,
                    ..
                } = &call.target
                else {
                    bail!("ordinary invoke has non-method target");
                };
                let name = name.as_ref();
                let args = &class.symbols.protos[usize::from(*prototype_index)].1;
                let ret = &call.return_type;
                let target = if static_call {
                    java_type(owner)?
                } else {
                    let value = register(
                        &regs,
                        usize::from(
                            call.receiver
                                .as_ref()
                                .context("missing invoke receiver")?
                                .register,
                        ),
                    )?;
                    if receiver_cleanup::sdk_number_conversion_receiver(
                        &value,
                        owner,
                        name,
                        args,
                        ret,
                        op,
                        class.symbols.hierarchy.get().map(AsRef::as_ref),
                    ) || receiver_cleanup::proven_void_receiver(
                        &value,
                        owner,
                        name,
                        args,
                        ret,
                        op,
                        class.symbols.hierarchy.get().map(AsRef::as_ref),
                    ) {
                        value.text.clone()
                    } else {
                        receiver(&value, owner)?
                    }
                };
                let label = format!(
                    "{}.{}({}){}",
                    owner
                        .trim_start_matches('L')
                        .trim_end_matches(';')
                        .replace('/', "."),
                    name,
                    args.join(""),
                    ret
                );
                let unambiguous_object_call = static_call
                    && args.iter().any(|ty| ty.as_ref() == "Ljava/lang/Object;")
                    && class
                        .symbols
                        .hierarchy
                        .get()
                        .is_some_and(|hierarchy| hierarchy.is_unambiguous_object_call(&label));
                let mut actual = Vec::new();
                let mut capture_count = 0;
                let mut capture_refs = Vec::new();
                for (argument_index, input) in call.arguments.iter().enumerate() {
                    let ty = &input.descriptor;
                    let reg = usize::from(input.register);
                    let mut value = register(&regs, reg)?;
                    if let Some(index) = value
                        .text
                        .strip_prefix("<class:")
                        .and_then(|v| v.strip_suffix('>'))
                        .and_then(|v| v.parse::<usize>().ok())
                    {
                        ensure!(
                            allocation.is_some() && name == "<init>",
                            "deferred class literal outside allocation"
                        );
                        let (local, display, label, snapshot) = &class_captures[index];
                        ensure!(
                            index <= capture_count,
                            "constructor arguments reorder class resolution"
                        );
                        if index == capture_count {
                            let prefix = if let Some(snapshot) = snapshot {
                                format!("({snapshot} = ({local} = ")
                            } else {
                                format!("({local} = ")
                            };
                            value.text = format!(
                                "{prefix}{display}.class{}",
                                if snapshot.is_some() { "))" } else { ")" }
                            );
                            if let Some(label) = label {
                                capture_refs.push((
                                    actual.len(),
                                    prefix.chars().count(),
                                    display.chars().count(),
                                    label.clone(),
                                ));
                            }
                            capture_count += 1;
                        } else {
                            value.text = local.clone();
                        }
                    }
                    ensure!(
                        value.text != "<uninitialized>" && (initialized || value.text != "this"),
                        "uninitialized invocation argument"
                    );
                    // Typed null preserves the DEX descriptor when Java has
                    // overloads (including constructors and varargs arrays).
                    let omit_cast = receiver_cleanup::proven_call_argument(
                        &value,
                        owner,
                        name,
                        args,
                        ret,
                        op,
                        argument_index,
                        class.symbols.hierarchy.get().map(AsRef::as_ref),
                    );
                    actual.push(if value.literal == Some(0) && reference(ty) {
                        if omit_cast {
                            "null".into()
                        } else {
                            format!("(({}) null)", java_type(ty)?)
                        }
                    } else if omit_cast
                        || (unambiguous_object_call
                            && ty.as_ref() == "Ljava/lang/Object;"
                            && reference(&value.ty))
                    {
                        value.text.clone()
                    } else if ty.as_ref() == "I" && matches!(value.ty.as_str(), "B" | "S" | "C") {
                        format!("((int) {})", value.text)
                    } else {
                        argument_from_register(
                            &value,
                            ty,
                            reg,
                            method.code.as_ref().unwrap(),
                            graph,
                        )?
                    });
                }
                ensure!(name != "<clinit>", "static initializer invocation");
                if name == "<init>" {
                    if let Some((dst, ty)) = allocation.take() {
                        ensure!(
                            kind == 0x70
                                && inputs.first() == Some(&dst)
                                && owner.as_ref() == ty
                                && ret.as_ref() == "V",
                            "allocation constructor mismatch"
                        );
                        ensure!(
                            register(&regs, dst)?.text == "<uninitialized>",
                            "allocation register overwritten"
                        );
                        ensure!(
                            capture_count == class_captures.len(),
                            "unused class literal during allocation"
                        );
                        let display = java_type(owner)?;
                        let expr = format!("new {display}({})", actual.join(", "));
                        let mut refs = vec![(4, display.chars().count(), label)];
                        for (arg, start, len, label) in capture_refs {
                            let offset = 5
                                + display.chars().count()
                                + actual[..arg]
                                    .iter()
                                    .map(|s| s.chars().count() + 2)
                                    .sum::<usize>();
                            // The Class-valued capture itself has no conversion
                            // unless the declared parameter type differs.
                            let cast_prefix = if args[arg].as_ref() == "Ljava/lang/Class;" {
                                0
                            } else {
                                format!("(({}) ", java_type(&args[arg])?).chars().count()
                            };
                            refs.push((offset + cast_prefix + start, len, label));
                        }
                        let value = out.local(owner, &expr, &refs)?;
                        for register in regs.iter_mut().flatten() {
                            if register.text == "<uninitialized>" {
                                *register = value.clone();
                            }
                            if let Some(index) = register
                                .text
                                .strip_prefix("<class:")
                                .and_then(|v| v.strip_suffix('>'))
                                .and_then(|v| v.parse::<usize>().ok())
                            {
                                register.text = class_captures[index].0.clone();
                            }
                        }
                        class_captures.clear();
                        if let Some(slots) = exception_slots {
                            sync_exception_registers(
                                slots,
                                &regs,
                                &mut previous_exception_values,
                                out,
                                false,
                            )?;
                        }
                        pc += width;
                        continue;
                    }
                    ensure!(
                        constructor
                            && !initialized
                            && kind == 0x70
                            && (owner.as_ref() == class.descriptor.as_ref()
                                || class.superclass.as_deref() == Some(owner.as_ref()))
                            && ret.as_ref() == "V",
                        "unsupported constructor invocation"
                    );
                    let receiver = register(&regs, inputs[0])?;
                    ensure!(receiver.text == "this", "constructor receiver");
                    for reg in &inputs[1..] {
                        ensure!(
                            register(&regs, *reg)?.text != "this",
                            "uninitialized this as constructor argument"
                        );
                    }
                    let keyword = if owner.as_ref() == class.descriptor.as_ref() {
                        ensure!(
                            !early_field_writes,
                            "field writes before this delegation not reconstructed"
                        );
                        ensure!(
                            *args != method.parameters,
                            "recursive constructor delegation"
                        );
                        "this"
                    } else {
                        "super"
                    };
                    out.line(
                        &format!("{keyword}({});", actual.join(", ")),
                        &[(0, keyword.len(), label)],
                    );
                    initialized = true;
                } else {
                    ensure!(allocation.is_none(), "invalid method call");
                    if !initialized {
                        ensure!(
                            constructor && kind != 0x6f,
                            "invalid early construction call"
                        );
                        for input in &inputs {
                            if regs
                                .get(*input)
                                .and_then(Option::as_ref)
                                .is_some_and(|value| value.ty == "<wide-tail>")
                            {
                                continue;
                            }
                            let value = register(&regs, *input)?;
                            ensure!(
                                value.text != "this" && value.text != "<uninitialized>",
                                "uninitialized this escapes through invocation"
                            );
                        }
                    }
                    let display_name = names::member(name)?;
                    if kind == 0x70 {
                        ensure!(
                            owner.as_ref() == class.descriptor.as_ref()
                                && class.methods.iter().any(|m| m.name.as_ref() == name
                                    && m.parameters == *args
                                    && m.return_type == *ret
                                    && m.access_flags & 2 != 0),
                            "nonprivate direct call not reconstructed"
                        );
                    }
                    let target = if kind == 0x6f {
                        let receiver = register(&regs, inputs[0])?;
                        // DEX may name an ancestor above the direct superclass.
                        // Both class invoke-super and Java super dispatch from
                        // the nearest superclass; do not turn this into a cast
                        // or a normal virtual call. Interface-super is distinct.
                        let ancestor = class.symbols.hierarchy.get().map_or_else(
                            || class.superclass.as_deref() == Some(owner.as_ref()),
                            |hierarchy| {
                                hierarchy.strict_superclass(&class.descriptor, owner)
                                    == crate::native_hierarchy::Relation::Proven
                            },
                        );
                        ensure!(
                            receiver.text == "this" && ancestor,
                            "invalid super receiver"
                        );
                        "super".into()
                    } else {
                        target
                    };
                    let shrink_index = if static_call && allocation.is_none() {
                        out.last_local.as_ref().and_then(|local| {
                            call.arguments.iter().enumerate().skip(1).find_map(
                                |(index, argument)| {
                                    let value =
                                        register(&regs, usize::from(argument.register)).ok()?;
                                    if value != local.value
                                        || local.indent != out.indent
                                        || local.value.ty != argument.descriptor.as_ref()
                                        || actual[index] != value.text
                                        || !actual[..index]
                                            .iter()
                                            .zip(&call.arguments[..index])
                                            .all(|(rendered, earlier)| {
                                                register(&regs, usize::from(earlier.register))
                                                    .is_ok_and(|value| {
                                                        value.ty == earlier.descriptor.as_ref()
                                                            && rendered == &value.text
                                                            && value
                                                                .text
                                                                .strip_prefix('v')
                                                                .or_else(|| {
                                                                    value.text.strip_prefix('p')
                                                                })
                                                                .is_some_and(|digits| {
                                                                    !digits.is_empty()
                                                                        && digits.bytes().all(
                                                                            |byte| {
                                                                                byte.is_ascii_digit(
                                                                                )
                                                                            },
                                                                        )
                                                                })
                                                    })
                                            })
                                        || !graph.single_use_call_result(
                                            method,
                                            pc,
                                            argument.register,
                                            &local.value.ty,
                                        )
                                    {
                                        return None;
                                    }
                                    Some(index)
                                },
                            )
                        })
                    } else {
                        None
                    };
                    let moved_local = shrink_index.and_then(|index| {
                        let prefix = format!(
                            "(({}) ",
                            java_type(&out.last_local.as_ref()?.value.ty).ok()?
                        );
                        let local = out.last_local.take()?;
                        actual[index] = format!("{prefix}{})", local.expression);
                        Some((index, local, prefix.chars().count()))
                    });
                    let expr = format!("{target}.{display_name}({})", actual.join(", "));
                    let mut refs = vec![(
                        target.chars().count() + 1,
                        display_name.chars().count(),
                        label,
                    )];
                    if static_call {
                        refs.push((
                            0,
                            target.chars().count(),
                            class_label(owner).context("invalid owner")?,
                        ));
                    }
                    if let Some((index, local, cast_prefix)) = moved_local {
                        let offset = target.chars().count()
                            + display_name.chars().count()
                            + 2
                            + actual[..index]
                                .iter()
                                .map(|arg| arg.chars().count() + 2)
                                .sum::<usize>();
                        refs.extend(local.refs.iter().map(|(start, len, label)| {
                            (offset + cast_prefix + start, *len, label.clone())
                        }));
                        out.text.truncate(local.byte_start);
                        out.chars = local.char_start;
                        out.links.truncate(local.link_start);
                    }
                    let consumes_result = if graph.front_end.is_some() {
                        call.result.is_some()
                    } else {
                        words
                            .get(pc + width)
                            .is_some_and(|word| matches!(word & 0xff, 0x0a..=0x0c))
                    };
                    if ret.as_ref() == "V" || !consumes_result {
                        out.line(&format!("{expr};"), &refs);
                    } else {
                        pending = Some(out.local(ret, &expr, &refs)?);
                    }
                }
            }
            0x7b..=0x8c => {
                let spec = numeric::Unary::decode(op).context("unary opcode")?;
                let (dst, src) = if let Some(instruction) = instruction {
                    let (dst, sources, _) = decoded_arithmetic(instruction, regs.len())?;
                    (dst, sources[0])
                } else {
                    (a & 15, a >> 4)
                };
                let source = argument(&register(&regs, src)?, spec.input.descriptor())?;
                let value = out.local(spec.result.descriptor(), &spec.expression(&source), &[])?;
                ensure!(value.ty == spec.result_descriptor, "unary result type");
                assign(&mut regs, dst, value)?;
            }
            0x9b..=0xaf | 0xbb..=0xcf => {
                let spec = numeric::Binary::decode(op).context("binary opcode")?;
                let (dst, left, right) = if let Some(instruction) = instruction {
                    let (dst, sources, _) = decoded_arithmetic(instruction, regs.len())?;
                    (dst, sources[0], sources[1])
                } else if op >= 0xb0 {
                    (a & 15, a & 15, a >> 4)
                } else {
                    let packed = words[pc + 1];
                    (a, (packed & 255) as usize, (packed >> 8) as usize)
                };
                let left = argument(&register(&regs, left)?, spec.left.descriptor())?;
                let right = argument(&register(&regs, right)?, spec.right.descriptor())?;
                let value = out.local(
                    spec.result.descriptor(),
                    &spec.expression(&left, &right),
                    &[],
                )?;
                assign(&mut regs, dst, value)?;
            }
            0x90..=0x9a | 0xb0..=0xba | 0xd0..=0xe2 => {
                let decoded = instruction
                    .map(|instruction| decoded_arithmetic(instruction, regs.len()))
                    .transpose()?;
                if let Some((dst, expression)) =
                    boolean_bitwise_expression(op, a, words, pc, &regs, decoded.as_ref())?
                {
                    let value = out.local("Z", &expression, &[])?;
                    assign(&mut regs, dst, value)?;
                } else {
                    let (dst, lhs, rhs, kind) = if let Some((dst, sources, literal)) = &decoded {
                        let kind = if op <= 0x9a {
                            op - 0x90
                        } else if op <= 0xba {
                            op - 0xb0
                        } else if op <= 0xd7 {
                            op - 0xd0
                        } else {
                            op - 0xd8
                        };
                        (
                            *dst,
                            integral(&register(&regs, sources[0])?)?,
                            if let Some(n) = literal {
                                n.to_string()
                            } else {
                                integral(&register(&regs, sources[1])?)?
                            },
                            kind,
                        )
                    } else if op <= 0x9a {
                        let w = words[pc + 1];
                        (
                            a,
                            integral(&register(&regs, (w & 255) as usize)?)?,
                            integral(&register(&regs, (w >> 8) as usize)?)?,
                            op - 0x90,
                        )
                    } else if op <= 0xba {
                        (
                            a & 15,
                            integral(&register(&regs, a & 15)?)?,
                            integral(&register(&regs, a >> 4)?)?,
                            op - 0xb0,
                        )
                    } else if op <= 0xd7 {
                        (
                            a & 15,
                            integral(&register(&regs, a >> 4)?)?,
                            (words[pc + 1] as i16).to_string(),
                            op - 0xd0,
                        )
                    } else {
                        (
                            a,
                            integral(&register(&regs, (words[pc + 1] & 255) as usize)?)?,
                            ((words[pc + 1] >> 8) as i8).to_string(),
                            op - 0xd8,
                        )
                    };
                    let (lhs, rhs) = if matches!(op, 0xd1 | 0xd9) {
                        (rhs, lhs)
                    } else {
                        (lhs, rhs)
                    };
                    let value = out.local("I", &format!("{lhs} {} ({rhs})", opname(kind)?), &[])?;
                    assign(&mut regs, dst, value)?;
                }
            }
            0x1d | 0x1e => bail!("unmatched or unproven monitor instruction"),
            _ => bail!("unsupported opcode 0x{op:02x} at {pc}"),
        }
        ensure!(
            out.text.len() <= 4 * 1024 * 1024,
            "reconstructed method exceeds output budget"
        );
        if !returned
            && !bare_return_tail(graph, words, pc + width)
            && let Some(slots) = exception_slots
        {
            sync_exception_registers(
                slots,
                &regs,
                &mut previous_exception_values,
                out,
                allocation.is_some(),
            )?;
        }
        pc += width;
        if returned {
            return Ok((regs, true));
        }
    }
    ensure!(allocation.is_none(), "unfinished allocation");
    Ok((regs, returned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use std::sync::Arc;
    #[test]
    fn retry_argument_guard_rejects_deferred_expressions_and_compound_literals() {
        let value = |text: &str| Value {
            text: text.into(),
            ty: "Ljava/lang/String;".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
        assert!(stable_retry_argument(&value("\"escaped \\\" quote\"")));
        assert!(stable_retry_argument(&value("v12")));
        assert!(!stable_retry_argument(&value(
            "\"prefix\" + effect() + \"suffix\""
        )));
        assert!(!stable_retry_argument(&value("this.field")));
    }
    #[test]
    fn terminal_loop_tail_proof_rejects_cycles_and_reentry() {
        let words = [0x0012, 0x0038, 3, 0xfe28, 0x000e];
        let mut graph = Graph::new(&words).unwrap();
        assert!(graph.non_reentering_tail(4, 0..4, &words).unwrap());
        assert!(graph.non_reentering_tail(1, 0..0, &words).unwrap());
        assert!(!graph.non_reentering_tail(3, 0..3, &words).unwrap());
        // A raw cycle without a separately validated loop remains unsupported.
        graph.loops.clear();
        assert!(!graph.non_reentering_tail(1, 0..0, &words).unwrap());
    }
    #[test]
    fn independent_loop_escape_proof_checks_secondary_edges_and_exception_ownership() {
        let words = [
            0x0012, 0x0628, 0x1071, 0, 0, 0x000a, 0x0928, 0x1035, 7, 0x2032, 0xfff9, 0x00d8,
            0x0100, 0xfa28, 0x000f, 0x0212, 0x1235, 7, 0x02d8, 0x0102, 0x00d8, 0x0200, 0xfa28,
            0x000f,
        ];
        let mut graph = Graph::new(&words).unwrap();
        assert!(graph.non_reentering_tail(2, 7..14, &words).unwrap());
        // Test the traversal directly, independently of global loop-overlap
        // rejection: a secondary exit must never disappear in a loop summary.
        graph.targets[16] = Some(7);
        assert!(!graph.non_reentering_tail(2, 7..14, &words).unwrap());
        graph.targets[16] = Some(23);
        graph.protected.push(2..5);
        assert!(!graph.non_reentering_tail(2, 7..14, &words).unwrap());
    }
    fn fixture(
        words: Vec<u16>,
        registers: u16,
        ins: u16,
        parameters: Vec<Arc<str>>,
        ret: &str,
    ) -> (DexClass, DexMethod) {
        let class = DexClass {
            symbols: Arc::new(DexSymbols::default()),
            descriptor: "Lsample/Example;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        };
        let method = DexMethod {
            declaring_type: class.descriptor.clone(),
            name: "run".into(),
            return_type: ret.into(),
            parameters,
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers,
                ins,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        };
        (class, method)
    }
    #[test]
    fn selected_loop_literal_uses_decoded_null_and_rejects_poison() {
        let words = vec![
            0x0012, 0x0338, 9, 0x0071, 0, 0, 0x000c, 0x03d8, 0xff03, 0xf828, 0x0011,
        ];
        let (mut class, mut method) =
            fixture(words.clone(), 4, 1, vec!["I".into()], "Ljava/lang/String;");
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into()],
            strings: vec!["next".into()],
            protos: vec![("Ljava/lang/String;".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(graph.shared_loops.len(), 1);
        let regs = vec![
            Some(Value {
                text: "0".into(),
                ty: "I".into(),
                literal: Some(0),
                wide_literal: None,
                raw_bits32: false,
            }),
            None,
            None,
            Some(Value {
                text: "p0".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            }),
        ];
        let region = graph.loops[0];
        let written = graph
            .written_in(method.code.as_ref().unwrap(), region.start, region.exit)
            .unwrap();
        let mut baseline = regs.clone();
        promote_loop_entry_literals(
            &class,
            &method,
            &graph,
            region,
            &mut baseline,
            Some(&written),
        )
        .unwrap();
        assert_eq!(baseline[0].as_ref().unwrap().ty, "Ljava/lang/String;");
        method.code.as_mut().unwrap().instructions[0] = 0x1012;
        let mut raw_poisoned = regs.clone();
        promote_loop_entry_literals(
            &class,
            &method,
            &graph,
            region,
            &mut raw_poisoned,
            Some(&written),
        )
        .unwrap();
        assert_eq!(
            raw_poisoned[0].as_ref().unwrap().ty,
            baseline[0].as_ref().unwrap().ty
        );
        graph.front_end.as_mut().unwrap().ir.instructions[0].literal = None;
        let mut decoded_poisoned = regs;
        let result = promote_loop_entry_literals(
            &class,
            &method,
            &graph,
            region,
            &mut decoded_poisoned,
            Some(&written),
        );
        assert!(result.is_err() || decoded_poisoned[0].as_ref().unwrap().ty == "I");
    }
    #[test]
    fn shared_straight_line_consumes_decoded_layout_and_bound_calls() {
        let (mut class, mut method) =
            fixture(vec![0x0071, 0, 0, 0x000a, 0x000f], 1, 0, vec![], "I");
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Source;".into()],
            strings: vec!["read".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(graph.instruction(3).unwrap().unwrap().opcode, 0x0a);
        assert!(graph.instruction(1).is_err());
        // Deliberately poison only the legacy layout and raw invoke index after
        // analysis. If rendering redecodes/rebinds instead of using the shared
        // front end, this cannot render. Production inputs remain immutable.
        graph.widths.fill(0);
        method.code.as_mut().unwrap().instructions[1] = u16::MAX;
        let mut output = Output::default();
        let (_, terminal) = render(
            &class,
            &method,
            &graph,
            0,
            5,
            vec![None],
            &mut output,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(terminal);
        assert!(output.text.contains("sample.Source.read()"));
        assert!(
            output
                .links
                .iter()
                .any(|link| link.label == "sample.Source.read()I")
        );
        // Missing shared binding is an invariant failure, never a raw fallback.
        graph.front_end.as_mut().unwrap().bound.calls.clear();
        assert!(
            render(
                &class,
                &method,
                &graph,
                0,
                5,
                vec![None],
                &mut Output::default(),
                0,
                true,
                None,
                None
            )
            .is_err()
        );
    }

    fn shared_natural_loop_fixture(goto: u8) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let width = usize::from(goto - 0x27);
        let mut words = vec![
            0x0012,
            0x0112,
            0x1071,
            0,
            0,
            0x3035,
            (6 + width) as u16,
            0x0190,
            0x0001,
            0x00d8,
            0x0100,
        ];
        let delta = -9i32;
        words.extend(match goto {
            0x28 => vec![0xf728],
            0x29 => vec![0x0029, delta as u16],
            _ => vec![0x002a, delta as u16, ((delta as u32) >> 16) as u16],
        });
        words.push(0x010f);
        let (mut class, mut method) = fixture(words, 4, 0, vec![], "I");
        method.code.as_mut().unwrap().outs = 1;
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Header;".into()],
            strings: vec!["touch".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let mut regs = vec![None; 4];
        assign(
            &mut regs,
            3,
            Value {
                text: "limit".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            },
        )
        .unwrap();
        (class, method, regs)
    }

    fn shared_posttest_fixture(ty: &str) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let (class, mut method, mut regs) = shared_natural_loop_fixture(0x28);
        let (value, returned) = match ty {
            "J" => (0x0181, 0x0110),
            "Ljava/lang/Object;" => {
                assign(
                    &mut regs,
                    2,
                    Value {
                        text: "object".into(),
                        ty: ty.into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )
                .unwrap();
                (0x2107, 0x0111)
            }
            _ => (0x0101, 0x010f),
        };
        method.return_type = ty.into();
        method.code.as_mut().unwrap().instructions = vec![
            0x0012, 0x1071, 0, 0, value, 0x00d8, 0x0100, 0x3034, 0xfffa, returned,
        ];
        (class, method, regs)
    }
    fn shared_dual_backedge_fixture() -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let words = vec![
            0x0012, 0x0112, 0x01d8, 0x0101, 0x2032, 19, 0x01d8, 0x0201, 0x3032, 15, 0x01d8, 0x0301,
            0x4032, 10, 0x00d8, 0x0100, 0x5034, 0xfff2, 0x01d8, 0x0401, 0x6034, 0xffee, 0x010f,
            0x010f,
        ];
        let (class, method) = fixture(words, 7, 5, vec!["I".into(); 5], "I");
        let mut regs = vec![None; 7];
        for (index, register) in (2..7).enumerate() {
            assign(
                &mut regs,
                register,
                Value {
                    text: format!("p{index}"),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
        }
        (class, method, regs)
    }
    #[test]
    fn shared_dual_backedges_revalidate_canonical_region_edges_and_operands() {
        let (class, method, regs) = shared_dual_backedge_fixture();
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(graph.shared_loops.len(), 1);
        assert_eq!(graph.shared_loops[0].start, 2);
        assert_eq!(graph.shared_loops[0].tail, Some(22));
        assert_eq!(
            graph
                .shared_loop_edges
                .iter()
                .filter(|e| e.kind == SharedLoopEdgeKind::Break)
                .count(),
            2
        );
        assert_eq!(
            graph
                .shared_loop_edges
                .iter()
                .filter(|e| e.kind == SharedLoopEdgeKind::Continue)
                .count(),
            1
        );
        let mut expected = Output::default();
        render(
            &class,
            &method,
            &graph,
            0,
            24,
            regs.clone(),
            &mut expected,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        // Once selected, emission consumes the canonical decoded operands and
        // does not silently retry altered raw branch words.
        let (_, mut raw, _) = shared_dual_backedge_fixture();
        for pc in [5, 9, 13, 17, 21] {
            raw.code.as_mut().unwrap().instructions[pc] = 0x7fff;
        }
        let mut actual = Output::default();
        render(
            &class,
            &raw,
            &graph,
            0,
            24,
            regs.clone(),
            &mut actual,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(actual.text, expected.text);
        for mutation in 0..7 {
            let mut poisoned = Graph::straight_line(&class, &method).unwrap().unwrap();
            match mutation {
                0 => poisoned.shared_loops[0].exit = 22,
                1 => poisoned.shared_loops[0].tail = None,
                2 => poisoned.shared_loop_edges[0].kind = SharedLoopEdgeKind::Continue,
                3 => poisoned.shared_loop_edges.swap(0, 1),
                4 => {
                    let index = poisoned.shared_cfg.as_ref().unwrap().block_at[&16];
                    poisoned.shared_cfg.as_mut().unwrap().blocks[index]
                        .successors
                        .swap(0, 1);
                }
                5 => {
                    poisoned
                        .front_end
                        .as_mut()
                        .unwrap()
                        .ir
                        .instructions
                        .iter_mut()
                        .find(|i| i.pc == 16)
                        .unwrap()
                        .branch_target = Some(23)
                }
                _ => {
                    poisoned.shared_loop_edges.pop();
                }
            }
            assert!(
                render(
                    &class,
                    &method,
                    &poisoned,
                    0,
                    24,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
    }
    #[test]
    fn shared_dual_backedges_reject_extra_exit_and_interior_entry() {
        let (class, mut method, _) = shared_dual_backedge_fixture();
        // Three guards to the common exit exceed the bounded shape.
        method.code.as_mut().unwrap().instructions[13] = 11; // terminal guard -> exit
        assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        let (class, mut method, _) = shared_dual_backedge_fixture();
        // Redirect one preheader branch into the loop interior while keeping
        // both backedges and the terminal return. Canonical ownership rejects.
        let words = &mut method.code.as_mut().unwrap().instructions;
        words.splice(0..0, [0x0238, 6]); // if-eqz p0, @0006 (inside body)
        assert!(Graph::straight_line(&class, &method).unwrap().is_none());
    }
    #[test]
    fn dual_backedge_selector_preserves_prefix_and_terminal_body_routes() {
        for arms in 1..=3 {
            let (class, mut method, _) = shared_dual_backedge_fixture();
            let words = &mut method.code.as_mut().unwrap().instructions;
            for _ in 0..arms {
                words.splice(2..2, [0x0038, 3, 0x010f]);
            }
            let selected = Graph::straight_line(&class, &method).unwrap();
            if arms == 1 {
                assert!(
                    selected.is_none(),
                    "single prefix return stays on legacy route"
                );
            } else {
                assert!(
                    selected.is_some(),
                    "closed prefix chain {arms} should be selected"
                );
            }
        }
        let (class, mut method, _) = shared_dual_backedge_fixture();
        method.code.as_mut().unwrap().instructions[2..4].copy_from_slice(&[0x010f, 0x0000]);
        assert!(
            Graph::straight_line(&class, &method).unwrap().is_none(),
            "terminal instruction inside loop body remains on legacy route"
        );
        let (class, mut method, _) = shared_dual_backedge_fixture();
        method.code.as_mut().unwrap().instructions[2] = 0x0113; // const/16 v1
        method.code.as_mut().unwrap().instructions[3] = 0x000f;
        assert!(
            Graph::straight_line(&class, &method).unwrap().is_some(),
            "operand low byte must not be mistaken for a return opcode"
        );
    }
    #[test]
    fn distinct_conditional_backedges_keep_their_existing_route() {
        let words = vec![
            0x0012, 0x00d8, 0x0100, 0x1034, 0xfffe, 0x00d8, 0x0100, 0x2034, 0xfffe, 0x000f,
        ];
        let (class, method) = fixture(words, 3, 2, vec!["I".into(); 2], "I");
        assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        let graph = Graph::new(&method.code.as_ref().unwrap().instructions).unwrap();
        let regs = vec![
            None,
            Some(Value {
                text: "p0".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            }),
            Some(Value {
                text: "p1".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            }),
        ];
        let mut out = Output::default();
        render(
            &class, &method, &graph, 0, 10, regs, &mut out, 0, true, None, None,
        )
        .unwrap();
        assert_eq!(out.text.matches("while (").count(), 2, "{}", out.text);
    }
    #[test]
    fn early_continue_skips_late_only_latch_operand() {
        let words = vec![
            0x0012, 0x0112, 0x01d8, 0x0101, 0x3032, 0x0014, 0x01d8, 0x0201, 0x4032, 0x0010, 0x01d8,
            0x0301, 0x5032, 0x000b, 0x00d8, 0x0100, 0x6034, 0xfff2, 0x4212, 0x01d8, 0x0401, 0x2034,
            0xffed, 0x010f, 0x010f,
        ];
        let (class, method) = fixture(words, 8, 5, vec!["I".into(); 5], "I");
        let mut regs = vec![None; 8];
        for (index, register) in (3..8).enumerate() {
            assign(
                &mut regs,
                register,
                Value {
                    text: format!("p{index}"),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
        }
        // v2 is undefined on the early backedge and assigned only after it.
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert!(!graph.live_at(graph.shared_loops[0].start, 2));
        let mut out = Output::default();
        render(
            &class, &method, &graph, 0, 25, regs, &mut out, 0, true, None, None,
        )
        .unwrap();
        assert_eq!(out.text.matches("continue;").count(), 2, "{}", out.text);
    }
    #[test]
    fn shared_posttest_body_diamond_preserves_canonical_ownership() {
        let (class, mut method, regs) = shared_posttest_fixture("I");
        // Header 1; branch 1 -> 6; goto 5 -> 7; latch 9 -> 1.
        method.code.as_mut().unwrap().instructions = vec![
            0x0012, 0x0338, 5, 0x1112, 0x1112, 0x0228, 0x2112, 0x00d8, 0x0100, 0x3034, 0xfff8,
            0x010f,
        ];
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(graph.shared_loops[0].start, 1);
        assert_eq!(graph.shared_loops[0].latch, 9);
        assert_eq!(
            graph.shared_branches,
            vec![SharedBranch {
                owner: Some(1),
                branch: 1,
                taken: 6,
                fallthrough: 3,
                join: 7,
            }]
        );
        let cfg = graph.shared_cfg.as_ref().unwrap();
        assert_eq!(cfg.blocks[cfg.block_at[&9]].successors.len(), 2);
        assert!(
            crate::native_dominators::DominatorTree::compute(cfg)
                .unwrap()
                .dominates(cfg.block_at[&1], cfg.block_at[&9])
        );
        assert!(graph.live_at(11, 1));
        let mut out = Output::default();
        render(
            &class,
            &method,
            &graph,
            0,
            12,
            regs.clone(),
            &mut out,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(out.text.contains("while (true)"));
        assert!(out.text.contains("if ("));
        for mutation in 0..5 {
            let mut broken = Graph::straight_line(&class, &method).unwrap().unwrap();
            match mutation {
                0 => broken.shared_branches[0].owner = None,
                1 => broken.shared_branches[0].join = 11,
                2 => broken.shared_branches.clear(),
                3 => broken.shared_branches[0].taken = 7,
                _ => {
                    let cfg = broken.shared_cfg.as_mut().unwrap();
                    cfg.blocks[cfg.block_at[&1]].successors[0].target = cfg.block_at[&7];
                }
            }
            let mut rejected = Output::default();
            assert!(
                render(
                    &class,
                    &method,
                    &broken,
                    0,
                    12,
                    regs.clone(),
                    &mut rejected,
                    0,
                    true,
                    None,
                    None
                )
                .is_err()
            );
        }
        let mut poisoned = Graph::straight_line(&class, &method).unwrap().unwrap();
        let mut baseline_out = Output::default();
        let baseline = render(
            &class,
            &method,
            &poisoned,
            0,
            12,
            regs.clone(),
            &mut baseline_out,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        method.code.as_mut().unwrap().instructions.fill(u16::MAX);
        poisoned.targets.fill(Some(usize::MAX));
        let mut actual = Output::default();
        let result = render(
            &class,
            &method,
            &poisoned,
            0,
            12,
            regs,
            &mut actual,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(result == baseline);
        assert_eq!(actual.text, baseline_out.text);
    }
    #[test]
    fn shared_posttest_poison_keeps_mandatory_body_exit_values_and_links() {
        for ty in ["I", "J", "Ljava/lang/Object;"] {
            let (class, mut method, regs) = shared_posttest_fixture(ty);
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert_eq!(
                graph.shared_loops,
                vec![Loop {
                    parent: None,
                    start: 1,
                    latch: 7,
                    guard: None,
                    exit: 9,
                    tail: None
                }]
            );
            assert!(graph.shared_branches.is_empty() && graph.shared_loop_edges.is_empty());
            assert!(!graph.live_at(1, 1));
            assert!(graph.live_at(9, 1));
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                10,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(!expected.links.is_empty());
            assert_eq!(expected.text.matches("Header.touch").count(), 1);
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                10,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(result == baseline);
            assert_eq!(actual.text, expected.text);
            assert_eq!(
                actual
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>(),
                expected
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn malformed_shared_posttest_metadata_rejects_after_valid_baseline() {
        for mutation in 0..12 {
            let (class, method, regs) = shared_posttest_fixture("I");
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    10,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_ok()
            );
            match mutation {
                0 => graph.shared_loops[0].start = 4,
                1 => graph.shared_loops[0].latch = 5,
                2 => graph.shared_loops[0].exit = 8,
                3 => graph.shared_loops[0].guard = Some(7),
                4 => graph.shared_loops[0].parent = Some(1),
                5 => graph.shared_loops.push(graph.shared_loops[0]),
                6 => graph.shared_cfg.as_mut().unwrap().blocks[1]
                    .successors
                    .swap(0, 1),
                7 => {
                    graph
                        .front_end
                        .as_mut()
                        .unwrap()
                        .ir
                        .instructions
                        .iter_mut()
                        .find(|i| i.pc == 7)
                        .unwrap()
                        .branch_target = Some(4)
                }
                8 => {
                    graph
                        .front_end
                        .as_mut()
                        .unwrap()
                        .ir
                        .instructions
                        .iter_mut()
                        .find(|i| i.pc == 7)
                        .unwrap()
                        .reads[0]
                        .register = 4
                }
                9 => {
                    graph
                        .front_end
                        .as_mut()
                        .unwrap()
                        .ir
                        .instructions
                        .iter_mut()
                        .find(|i| i.pc == 7)
                        .unwrap()
                        .width = 1
                }
                10 => graph.shared_branches.push(SharedBranch {
                    owner: None,
                    branch: 7,
                    taken: 1,
                    fallthrough: 9,
                    join: 9,
                }),
                _ => graph.shared_loop_edges.push(SharedLoopEdge {
                    owner: 1,
                    branch: 7,
                    taken: 1,
                    fallthrough: 9,
                    kind: SharedLoopEdgeKind::Continue,
                }),
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    10,
                    regs,
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
    }
    #[test]
    fn shared_posttest_budget_and_malformed_raw_target_do_not_retry_legacy() {
        let (class, method, _) = shared_posttest_fixture("I");
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let mut work = 0;
        assert!(
            decoded_posttest_loop(
                &graph.front_end.as_ref().unwrap().ir,
                graph.shared_cfg.as_ref().unwrap(),
                &mut work
            )
            .unwrap_err()
            .to_string()
            .contains("work budget")
        );
        assert_eq!(work, 0);
        // Target the const operand, which looks like a return/throw opcode.
        for operand in [0x000f, 0x0027] {
            let (class, method) = fixture(
                vec![0x0013, operand, 0x0038, 0xffff, 0x000e],
                1,
                0,
                vec![],
                "V",
            );
            assert!(Graph::straight_line(&class, &method).is_err());
        }
        for words in [
            vec![0x0038, 5, 0x0012, 0x0039, 0xfffe, 0x000e],
            vec![0x000e, 0x0038, 0xffff, 0x000e],
            vec![0x0012, 0x0029, 0xffff, 0x0038, 0xfffd, 0x000e],
        ] {
            let (class, method) = fixture(words, 1, 0, vec![], "V");
            assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        }
    }

    #[test]
    fn shared_posttest_break_revalidates_exit_edge_and_keeps_decoded_ownership() {
        // sum += 2; if (selector == 0) break; sum += 3; i++;
        // if (i < limit) repeat; return sum.
        let words = vec![
            0x0012, 0x0112, 0x01d8, 0x0201, 0x0338, 8, 0x01d8, 0x0301, 0x00d8, 0x0100, 0x2034,
            0xfff8, 0x010f,
        ];
        let (class, method) = fixture(words.clone(), 4, 2, vec!["I".into(), "I".into()], "I");
        let regs = [None, None, Some("p0"), Some("p1")]
            .into_iter()
            .map(|value| {
                value.map(|text| Value {
                    text: text.into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                })
            })
            .collect::<Vec<_>>();
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(graph.shared_loops[0].start, 2);
        assert_eq!(
            graph.shared_loop_edges,
            vec![SharedLoopEdge {
                owner: 2,
                branch: 4,
                taken: 12,
                fallthrough: 6,
                kind: SharedLoopEdgeKind::Break,
            }]
        );
        let mut expected = Output::default();
        let result = render(
            &class,
            &method,
            &graph,
            0,
            13,
            regs.clone(),
            &mut expected,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(expected.text.contains("break;"));

        for mutation in 0..4 {
            let mut broken = Graph::straight_line(&class, &method).unwrap().unwrap();
            match mutation {
                0 => broken.shared_loop_edges[0].taken = 10,
                1 => broken.shared_loop_edges[0].fallthrough = 8,
                2 => broken.shared_loop_edges[0].owner = 0,
                _ => {
                    let cfg = broken.shared_cfg.as_mut().unwrap();
                    cfg.blocks[cfg.block_at[&4]].successors.swap(0, 1);
                }
            }
            assert!(
                render(
                    &class,
                    &method,
                    &broken,
                    0,
                    13,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        let (_, mut raw) = fixture(words, 4, 2, vec!["I".into(), "I".into()], "I");
        let mut poisoned = Graph::straight_line(&class, &method).unwrap().unwrap();
        poisoned.targets.fill(Some(usize::MAX));
        raw.code.as_mut().unwrap().instructions.fill(u16::MAX);
        let mut actual = Output::default();
        let decoded = render(
            &class,
            &raw,
            &poisoned,
            0,
            13,
            regs,
            &mut actual,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(decoded == result);
        assert_eq!(actual.text, expected.text);
    }

    #[test]
    fn shared_posttest_break_rejects_extra_exit_and_interior_entry() {
        let cases = [
            // Two conditional body edges leave through distinct exits.
            vec![
                0x0012, 0x0112, 0x01d8, 0x0201, 0x0338, 10, 0x0338, 9, 0x01d8, 0x0301, 0x00d8,
                0x0100, 0x2034, 0xfff6, 0x010f, 0x000f,
            ],
            // A prefix branch bypasses the posttest header into its interior.
            vec![
                0x0338, 6, 0x0012, 0x0112, 0x01d8, 0x0201, 0x0338, 6, 0x00d8, 0x0100, 0x2034,
                0xfff8, 0x010f,
            ],
        ];
        for words in cases {
            let (class, method) = fixture(words.clone(), 4, 2, vec!["I".into(), "I".into()], "I");
            let front = crate::native_method::MethodFrontEnd::build(&class, &method).unwrap();
            let cfg =
                crate::native_cfg::ControlFlowGraph::from_decoded_loop(&front.ir, words.len())
                    .unwrap();
            assert!(decoded_posttest_loop(&front.ir, &cfg, &mut 16_000_000).is_err());
        }
    }

    #[test]
    fn shared_posttest_multiple_breaks_revalidate_order_and_decoded_operands() {
        // sum += 1; if (i == first) break; sum += 2;
        // if (i == second) break; sum += 3; i++; if (i < limit) repeat.
        let words = vec![
            0x0012, 0x0112, 0x01d8, 0x0101, 0x3032, 12, 0x01d8, 0x0201, 0x4032, 8, 0x01d8, 0x0301,
            0x00d8, 0x0100, 0x2034, 0xfff4, 0x010f,
        ];
        let (class, method) = fixture(
            words.clone(),
            5,
            3,
            vec!["I".into(), "I".into(), "I".into()],
            "I",
        );
        let regs = [None, None, Some("limit"), Some("first"), Some("second")]
            .into_iter()
            .map(|value| {
                value.map(|text| Value {
                    text: text.into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                })
            })
            .collect::<Vec<_>>();
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(
            graph.shared_loop_edges,
            vec![
                SharedLoopEdge {
                    owner: 2,
                    branch: 4,
                    taken: 16,
                    fallthrough: 6,
                    kind: SharedLoopEdgeKind::Break
                },
                SharedLoopEdge {
                    owner: 2,
                    branch: 8,
                    taken: 16,
                    fallthrough: 10,
                    kind: SharedLoopEdgeKind::Break
                },
            ]
        );
        let mut expected = Output::default();
        let result = render(
            &class,
            &method,
            &graph,
            0,
            words.len(),
            regs.clone(),
            &mut expected,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(expected.text.matches("break;").count(), 3);
        for mutation in 0..5 {
            let mut broken = Graph::straight_line(&class, &method).unwrap().unwrap();
            match mutation {
                0 => broken.shared_loop_edges.swap(0, 1),
                1 => broken.shared_loop_edges[1].taken = 15,
                2 => broken.shared_loop_edges[1].fallthrough = 12,
                3 => broken.shared_loop_edges[1].owner = 0,
                _ => {
                    let cfg = broken.shared_cfg.as_mut().unwrap();
                    cfg.blocks[cfg.block_at[&8]].successors.swap(0, 1);
                }
            }
            assert!(
                render(
                    &class,
                    &method,
                    &broken,
                    0,
                    words.len(),
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        let (_, mut raw) = fixture(words, 5, 3, vec!["I".into(), "I".into(), "I".into()], "I");
        let mut poisoned = Graph::straight_line(&class, &method).unwrap().unwrap();
        poisoned.targets.fill(Some(usize::MAX));
        raw.code.as_mut().unwrap().instructions.fill(u16::MAX);
        let mut actual = Output::default();
        let decoded = render(
            &class,
            &raw,
            &poisoned,
            0,
            17,
            regs,
            &mut actual,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(decoded == result);
        assert_eq!(actual.text, expected.text);
    }

    #[test]
    fn shared_posttest_multiple_break_budget_and_extra_backedge() {
        let build = |breaks: usize| {
            let mut words = vec![0x0012, 0x0112];
            let mut branches = Vec::new();
            for _ in 0..breaks {
                branches.push(words.len());
                words.extend([0x0338, 0]);
            }
            words.extend([0x01d8, 0x0101, 0x00d8, 0x0100]);
            let latch = words.len();
            words.extend([0x2034, (2isize - latch as isize) as i16 as u16]);
            let exit = words.len();
            words.push(0x010f);
            for pc in branches {
                words[pc + 1] = (exit as isize - pc as isize) as i16 as u16;
            }
            words
        };
        let words = build(8);
        let (class, method) = fixture(words.clone(), 4, 2, vec!["I".into(), "I".into()], "I");
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(graph.shared_loop_edges.len(), 8);
        let regs = [None, None, Some("limit"), Some("selector")]
            .into_iter()
            .map(|value| {
                value.map(|text| Value {
                    text: text.into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                })
            })
            .collect();
        render(
            &class,
            &method,
            &graph,
            0,
            words.len(),
            regs,
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .unwrap();
        let words = build(9);
        let (class, method) = fixture(words.clone(), 4, 2, vec!["I".into(), "I".into()], "I");
        let front = crate::native_method::MethodFrontEnd::build(&class, &method).unwrap();
        let cfg =
            crate::native_cfg::ControlFlowGraph::from_decoded_loop(&front.ir, words.len()).unwrap();
        assert!(
            decoded_posttest_loop(&front.ir, &cfg, &mut 16_000_000)
                .unwrap_err()
                .to_string()
                .contains("break budget")
        );
        assert!(Graph::straight_line(&class, &method).unwrap().is_none());

        let mut words = build(2);
        let second = 4;
        words[second + 1] = (2isize - second as isize) as i16 as u16;
        let (class, method) = fixture(words.clone(), 4, 2, vec!["I".into(), "I".into()], "I");
        let front = crate::native_method::MethodFrontEnd::build(&class, &method).unwrap();
        let cfg =
            crate::native_cfg::ControlFlowGraph::from_decoded_loop(&front.ir, words.len()).unwrap();
        assert!(decoded_posttest_loop(&front.ir, &cfg, &mut 16_000_000).is_err());
    }

    #[test]
    fn shared_pretest_terminal_exits_revalidate_metadata_and_decoded_operands() {
        let words = vec![
            0x0012, 0xf112, 0x1071, 0, 0, 0x2035, 12, 0x1071, 1, 0, 0x3032, 6, 0x00d8, 0x0100,
            0x2034, 0xfff4, 0x000f, 0x010f,
        ];
        let (mut class, method) = fixture(words.clone(), 4, 2, vec!["I".into(), "I".into()], "I");
        class.symbols = Arc::new(DexSymbols {
            strings: vec!["header".into(), "body".into()],
            types: vec!["Lsample/Hook;".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0), (0, 0, 1)],
            ..Default::default()
        });
        let mut method = method;
        method.code.as_mut().unwrap().outs = 1;
        let regs = [None, None, Some("p0"), Some("p1")]
            .into_iter()
            .map(|value| {
                value.map(|text| Value {
                    text: text.into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                })
            })
            .collect::<Vec<_>>();
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(
            graph.shared_loops,
            vec![Loop {
                parent: None,
                start: 2,
                latch: 14,
                guard: Some(5),
                exit: 18,
                tail: None,
            }]
        );
        assert_eq!(graph.loops, graph.shared_loops);
        assert_eq!(
            graph.shared_loop_edges,
            vec![SharedLoopEdge {
                owner: 2,
                branch: 10,
                taken: 16,
                fallthrough: 12,
                kind: SharedLoopEdgeKind::Terminal,
            }]
        );
        assert!(graph.non_reentering_tail(16, 2..16, &words).unwrap());
        let mut expected = Output::default();
        let result = render(
            &class,
            &method,
            &graph,
            0,
            18,
            regs.clone(),
            &mut expected,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        for mutation in 0..5 {
            let mut broken = Graph::straight_line(&class, &method).unwrap().unwrap();
            match mutation {
                0 => broken.shared_loops[0].exit = 17,
                1 => broken.shared_loops[0].guard = Some(7),
                2 => broken.shared_loop_edges[0].taken = 17,
                3 => broken.shared_loop_edges[0].kind = SharedLoopEdgeKind::Break,
                _ => {
                    let cfg = broken.shared_cfg.as_mut().unwrap();
                    cfg.blocks[cfg.block_at[&14]].successors.swap(0, 1);
                }
            }
            assert!(
                render(
                    &class,
                    &method,
                    &broken,
                    0,
                    18,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        let (_, mut raw) = fixture(words, 4, 2, vec!["I".into(), "I".into()], "I");
        raw.code.as_mut().unwrap().outs = 1;
        raw.code.as_mut().unwrap().instructions.fill(u16::MAX);
        let mut poisoned = Graph::straight_line(&class, &method).unwrap().unwrap();
        poisoned.targets.fill(Some(usize::MAX));
        let mut actual = Output::default();
        let decoded = render(
            &class,
            &raw,
            &poisoned,
            0,
            18,
            regs,
            &mut actual,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(decoded == result);
        assert_eq!(actual.text, expected.text);
    }

    fn shared_loop_body_fixture(empty: bool) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let (class, mut method, regs) = shared_natural_loop_fixture(0x28);
        method.code.as_mut().unwrap().instructions = if empty {
            vec![
                0x0012, 0x0112, 0x1071, 0, 0, 0x3035, 10, 0x0190, 0x0001, 0x00d8, 0x0100, 0x0338,
                2, 0xf528, 0x010f,
            ]
        } else {
            vec![
                0x0012, 0x0112, 0x1071, 0, 0, 0x3035, 9, 0x0338, 4, 0x0190, 0x0001, 0x00d8, 0x0100,
                0xf528, 0x010f,
            ]
        };
        // Both variants exit immediately after the single unconditional latch.
        method.code.as_mut().unwrap().instructions[6] = 9;
        (class, method, regs)
    }

    fn shared_nested_loop_fixture(
        depth: usize,
        siblings: bool,
        edges: bool,
        bypass: bool,
        adjacent: bool,
    ) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        fn emit(
            words: &mut Vec<u16>,
            level: usize,
            depth: usize,
            siblings: bool,
            edges: bool,
            bypass: bool,
            adjacent: bool,
        ) {
            words.push(((level as u16) << 8) | 0x12);
            let header = words.len();
            words.extend([0x1071, 0, level as u16]);
            let guard = words.len();
            words.extend([0x9035 | ((level as u16) << 8), 0]);
            words.extend([0x0890, ((level as u16) << 8) | 8]);
            if adjacent {
                words.extend([0xd8 | ((level as u16) << 8), 0x0100 | level as u16]);
            }
            let skip = if bypass && level + 1 < depth {
                let pc = words.len();
                words.extend([0x0739, 0]);
                Some(pc)
            } else {
                None
            };
            if level + 1 < depth {
                emit(words, level + 1, depth, siblings, edges, bypass, adjacent);
                if siblings && level == 0 {
                    emit(words, level + 1, depth, false, edges, bypass, adjacent);
                }
            }
            if let Some(pc) = skip {
                words[pc + 1] = (words.len() - pc) as u16;
            }
            if !adjacent {
                words.extend([0xd8 | ((level as u16) << 8), 0x0100 | level as u16]);
            }
            let mut breaks = Vec::new();
            if edges {
                let pc = words.len();
                words.extend([
                    0x7032 | ((level as u16) << 8),
                    (header as isize - pc as isize) as i16 as u16,
                ]);
                breaks.push(words.len());
                words.extend([0x9032 | ((level as u16) << 8), 0]);
            }
            let latch = words.len();
            words.extend([0x0029, (header as isize - latch as isize) as i16 as u16]);
            let exit = words.len();
            words[guard + 1] = (exit - guard) as u16;
            for pc in breaks {
                words[pc + 1] = (exit - pc) as u16;
            }
        }
        let (class, mut method, _) = shared_natural_loop_fixture(0x28);
        let mut words = vec![0x0812];
        emit(&mut words, 0, depth, siblings, edges, bypass, adjacent);
        words.push(0x080f);
        method.code.as_mut().unwrap().instructions = words;
        method.code.as_mut().unwrap().registers = 10;
        let mut regs = vec![None; 10];
        for (r, name) in [(7, "mode"), (9, "limit")] {
            assign(
                &mut regs,
                r,
                Value {
                    text: name.into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
        }
        (class, method, regs)
    }

    #[test]
    fn shared_nested_loops_poison_preserves_parent_child_plans_and_carried_state() {
        for (depth, siblings, edges, bypass, adjacent) in [
            (2, false, true, true, false),
            (3, false, true, true, false),
            (4, false, true, true, false),
            (2, true, true, true, false),
            (2, false, false, true, true),
        ] {
            let (class, mut method, regs) =
                shared_nested_loop_fixture(depth, siblings, edges, bypass, adjacent);
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert_eq!(graph.shared_loops.len(), depth + usize::from(siblings));
            assert_eq!(graph.shared_loops[0].parent, None);
            for region in graph.shared_loops.iter().skip(1) {
                assert!(region.parent.is_some());
            }
            for plan in &graph.shared_branches {
                assert!(plan.owner.is_some());
            }
            for region in &graph.shared_loops {
                assert!(graph.live_at(region.start, 8));
                assert_eq!(
                    graph
                        .shared_loop_edges
                        .iter()
                        .filter(|edge| edge.owner == region.start)
                        .count(),
                    if edges { 2 } else { 0 }
                );
            }
            if adjacent {
                assert_eq!(graph.shared_loops[1].exit, graph.shared_loops[0].latch);
            }
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(
                expected.text.matches("sample.Header.touch(").count(),
                graph.shared_loops.len()
            );
            assert!(!expected.links.is_empty());
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == result);
            assert_eq!(expected.text, actual.text);
            assert_eq!(
                expected
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>(),
                actual
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn malformed_shared_nested_loops_reject_parent_owner_and_cross_entries() {
        let (class, method, regs) = shared_nested_loop_fixture(3, false, true, true, false);
        let end = method.code.as_ref().unwrap().instructions.len();
        for mutation in 0..10 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            let outer = graph.shared_loops[0];
            let child = graph.shared_loops[1];
            let grandchild = graph.shared_loops[2];
            match mutation {
                0 => graph.shared_loops[1].parent = None,
                1 => graph.shared_loops[1].parent = Some(grandchild.start),
                2 => graph.shared_loops[2].parent = Some(outer.start),
                3 => graph.shared_loops[0].parent = Some(child.start),
                4 => graph.shared_branches[0].owner = Some(child.start),
                5 => graph.shared_branches[0].join = child.guard.unwrap() + 2,
                6 => graph.shared_loop_edges[0].owner = outer.start,
                7..=9 => {
                    let pc = if mutation == 9 {
                        graph.shared_branches[0].branch
                    } else {
                        graph
                            .shared_loop_edges
                            .iter()
                            .find(|edge| edge.owner == child.start)
                            .unwrap()
                            .branch
                    };
                    let ir = &mut graph.front_end.as_mut().unwrap().ir;
                    ir.instructions
                        .iter_mut()
                        .find(|i| i.pc == pc)
                        .unwrap()
                        .branch_target = Some(if mutation == 7 {
                        outer.start
                    } else if mutation == 8 {
                        outer.exit
                    } else {
                        grandchild.start
                    });
                    graph.shared_cfg = Some(
                        crate::native_cfg::ControlFlowGraph::from_decoded_loop(ir, end).unwrap(),
                    );
                }
                _ => unreachable!(),
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let branch = graph.shared_branches[0].branch;
        let mut excluded = method;
        excluded.code.as_mut().unwrap().instructions[branch + 1] =
            (graph.shared_loops[2].start - branch) as u16;
        assert!(Graph::straight_line(&class, &excluded).unwrap().is_none());
        let (class, mut siblings, sibling_regs) =
            shared_nested_loop_fixture(2, true, true, true, false);
        let mut graph = Graph::straight_line(&class, &siblings).unwrap().unwrap();
        render(
            &class,
            &siblings,
            &graph,
            0,
            siblings.code.as_ref().unwrap().instructions.len(),
            sibling_regs.clone(),
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .unwrap();
        let first = graph.shared_loops[1];
        let sibling = graph.shared_loops[2];
        graph.shared_loops[1].parent = Some(sibling.start);
        assert!(
            render(
                &class,
                &siblings,
                &graph,
                0,
                siblings.code.as_ref().unwrap().instructions.len(),
                sibling_regs,
                &mut Output::default(),
                0,
                true,
                None,
                None
            )
            .is_err()
        );
        let pc = graph
            .shared_loop_edges
            .iter()
            .find(|edge| edge.owner == first.start)
            .unwrap()
            .branch;
        siblings.code.as_mut().unwrap().instructions[pc + 1] =
            (graph.shared_loops[0].start as isize - pc as isize) as i16 as u16;
        assert!(Graph::straight_line(&class, &siblings).unwrap().is_none());
    }

    #[test]
    fn shared_nested_loop_body_closure_budget_rejects_without_partial_plan() {
        let (class, method, regs) = shared_nested_loop_fixture(2, false, false, true, false);
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let len = method.code.as_ref().unwrap().instructions.len();
        render(
            &class,
            &method,
            &graph,
            0,
            len,
            regs,
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .unwrap();
        let parent = graph.shared_loops[0];
        let child = graph.shared_loops[1];
        let cfg = graph.shared_cfg.as_ref().unwrap();
        let mut projection = cfg.clone();
        projection.blocks[cfg.block_at[&parent.latch]]
            .successors
            .clear();
        collapse_shared_loop(&mut projection, cfg, child).unwrap();
        let postdom = projection.forward_postdominators().unwrap();
        let mut work = 0;
        let result = decoded_branch_plans_with_postdom(
            &graph.front_end.as_ref().unwrap().ir,
            &projection,
            len,
            parent.guard.unwrap() + 2..parent.latch,
            &[],
            &[child],
            &postdom,
            &mut work,
        );
        assert!(result.err().unwrap().to_string().contains("work budget"));
        assert_eq!(work, 0);
    }

    #[test]
    fn shared_nested_loop_depth_budget_renders_four_and_retains_legacy_five() {
        for depth in [MAX_SHARED_LOOP_DEPTH, MAX_SHARED_LOOP_DEPTH + 1] {
            let (class, method, regs) =
                shared_nested_loop_fixture(depth, false, false, false, false);
            let len = method.code.as_ref().unwrap().instructions.len();
            let front = crate::native_method::MethodFrontEnd::build(&class, &method).unwrap();
            let cfg =
                crate::native_cfg::ControlFlowGraph::from_decoded_loop(&front.ir, len).unwrap();
            let canonical = decoded_natural_loops(&front.ir, &cfg);
            let selected = Graph::straight_line(&class, &method).unwrap();
            if depth == MAX_SHARED_LOOP_DEPTH {
                assert_eq!(canonical.unwrap().len(), depth);
                let graph = selected.unwrap();
                let mut out = Output::default();
                render(
                    &class, &method, &graph, 0, len, regs, &mut out, 0, true, None, None,
                )
                .unwrap();
                assert_eq!(out.text.matches("sample.Header.touch(").count(), depth);
            } else {
                assert!(
                    canonical
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("nesting budget")
                );
                assert!(selected.is_none());
            }
        }
    }

    fn shared_disjoint_loop_fixture(
        count: usize,
        alternative: bool,
        mixed: bool,
        adjacent: bool,
    ) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let (class, mut method, mut regs) = shared_natural_loop_fixture(0x28);
        let mut words = vec![0x0112];
        if alternative {
            words.extend([0x0239, 0]);
        }
        let mut bypass = None;
        let mut second = None;
        let mut join = None;
        for index in 0..count {
            if index == 1 {
                second = Some(words.len());
            }
            if index == 2 {
                join = Some(words.len());
            }
            if !adjacent || index == 0 {
                words.push(0x0012);
            }
            let header = words.len();
            words.extend([0x1071, 0, if adjacent && index > 0 { 1 } else { 0 }]);
            let guard = words.len();
            words.extend([0x3035, 0]);
            let mut exits = Vec::new();
            if mixed {
                words.extend([0x00d8, 0x0100]);
                let pc = words.len();
                words.extend([0x2032, (header as isize - pc as isize) as i16 as u16]);
                exits.push(words.len());
                words.extend([0x3032, 0]);
            }
            words.extend([0x0190, 0x0001]);
            if !mixed {
                words.extend([0x00d8, 0x0100]);
            }
            let latch = words.len();
            words.push(0x28 | (((header as isize - latch as isize) as i8 as u8 as u16) << 8));
            let exit = words.len();
            words[guard + 1] = (exit - guard) as u16;
            for pc in exits {
                words[pc + 1] = (exit - pc) as u16;
            }
            if alternative && index == 0 {
                bypass = Some(words.len());
                words.extend([0x0029, 0]);
            }
        }
        let end = words.len();
        words.push(0x010f);
        if alternative {
            words[2] = (second.unwrap() - 1) as u16;
            let pc = bypass.unwrap();
            words[pc + 1] = (join.unwrap_or(end) - pc) as u16;
        }
        method.code.as_mut().unwrap().instructions = words;
        assign(
            &mut regs,
            2,
            Value {
                text: "mode".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            },
        )
        .unwrap();
        (class, method, regs)
    }

    #[test]
    fn shared_disjoint_loops_poison_preserves_all_owners_plans_links_and_state() {
        for (count, alternative, mixed, adjacent) in [
            (2, false, true, false),
            (3, false, true, false),
            (2, true, true, false),
            (3, true, true, false),
            (2, false, false, true),
        ] {
            let (class, mut method, regs) =
                shared_disjoint_loop_fixture(count, alternative, mixed, adjacent);
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert_eq!(graph.shared_loops.len(), count);
            assert_eq!(
                graph.shared_loop_edges.len(),
                if mixed { 2 * count } else { 0 }
            );
            assert_eq!(graph.shared_branches.len(), usize::from(alternative));
            for region in &graph.shared_loops {
                assert!(graph.live_at(region.start, 1));
                assert_eq!(
                    graph
                        .shared_loop_edges
                        .iter()
                        .filter(|edge| edge.owner == region.start)
                        .count(),
                    if mixed { 2 } else { 0 }
                );
            }
            if adjacent {
                assert_eq!(graph.shared_loops[0].exit, graph.shared_loops[1].start);
            }
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline.1);
            assert_eq!(expected.text.matches("sample.Header.touch(").count(), count);
            assert!(!expected.links.is_empty());
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == result);
            assert_eq!(expected.text, actual.text);
            assert_eq!(
                expected
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>(),
                actual
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn malformed_shared_disjoint_loops_reject_owner_order_and_cross_entries() {
        let (class, method, regs) = shared_disjoint_loop_fixture(3, true, true, false);
        let end = method.code.as_ref().unwrap().instructions.len();
        for mutation in 0..12 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            let other = graph.shared_loops[1];
            match mutation {
                0 => graph.shared_loops.swap(0, 1),
                1 => {
                    graph.shared_loops.pop();
                }
                2 => graph.shared_loops.push(graph.shared_loops[0]),
                3 => graph.shared_loops[0].exit = other.exit,
                4 => graph.loops.swap(0, 1),
                5 => graph.shared_loop_edges[0].owner = other.start,
                6 => graph.shared_loop_edges[0].taken = other.start,
                7 => {
                    graph.shared_loop_edges.pop();
                }
                8 => graph.shared_branches[0].join = other.latch,
                9 | 10 => {
                    let first_exit = graph.shared_loops[0].exit;
                    let ir = &mut graph.front_end.as_mut().unwrap().ir;
                    ir.instructions
                        .iter_mut()
                        .find(|i| i.pc == 1)
                        .unwrap()
                        .branch_target = Some(if mutation == 9 {
                        other.guard.unwrap() + 2
                    } else {
                        other.latch
                    });
                    // Keep the second preheader reachable through the normal
                    // first-loop exit while adding the bypass interior entry.
                    ir.instructions
                        .iter_mut()
                        .find(|i| i.pc == first_exit)
                        .unwrap()
                        .branch_target = Some(other.start - 1);
                    graph.shared_cfg = Some(
                        crate::native_cfg::ControlFlowGraph::from_decoded_loop(ir, end).unwrap(),
                    );
                }
                _ => {
                    let pc = graph.shared_loop_edges[0].branch;
                    let ir = &mut graph.front_end.as_mut().unwrap().ir;
                    ir.instructions
                        .iter_mut()
                        .find(|i| i.pc == pc)
                        .unwrap()
                        .branch_target = Some(other.start);
                    graph.shared_cfg = Some(
                        crate::native_cfg::ControlFlowGraph::from_decoded_loop(ir, end).unwrap(),
                    );
                }
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        let baseline = Graph::straight_line(&class, &method).unwrap().unwrap();
        for target in [
            baseline.shared_loops[1].guard.unwrap() + 2,
            baseline.shared_loops[1].latch,
        ] {
            let (_, mut excluded, _) = shared_disjoint_loop_fixture(3, true, true, false);
            excluded.code.as_mut().unwrap().instructions[2] = (target - 1) as u16;
            assert!(Graph::straight_line(&class, &excluded).unwrap().is_none());
        }
        // Extend the first loop around the second while preserving the inner
        // backedge. The nested increment now selects this bounded parent tree.
        let (_, mut nested, _) = shared_disjoint_loop_fixture(2, false, false, false);
        let baseline = Graph::straight_line(&class, &nested).unwrap().unwrap();
        let outer = baseline.shared_loops[0];
        let inner = baseline.shared_loops[1];
        let instructions = &mut nested.code.as_mut().unwrap().instructions;
        instructions[outer.guard.unwrap() + 1] = (inner.exit + 1 - outer.guard.unwrap()) as u16;
        instructions[outer.latch] = 0x0128;
        // Preserve the inner latch and add an enclosing latch at its exit.
        instructions.insert(
            inner.exit,
            0x28 | (((outer.start as isize - inner.exit as isize) as i8 as u8 as u16) << 8),
        );
        let front = crate::native_method::MethodFrontEnd::build(&class, &nested).unwrap();
        let cfg = crate::native_cfg::ControlFlowGraph::from_decoded_loop(
            &front.ir,
            nested.code.as_ref().unwrap().instructions.len(),
        )
        .unwrap();
        assert_eq!(
            cfg.blocks
                .iter()
                .enumerate()
                .flat_map(|(from, block)| block
                    .successors
                    .iter()
                    .filter(move |edge| edge.target <= from))
                .count(),
            2
        );
        let selected = Graph::straight_line(&class, &nested).unwrap().unwrap();
        assert_eq!(selected.shared_loops.len(), 2);
        assert_eq!(
            selected.shared_loops[1].parent,
            Some(selected.shared_loops[0].start)
        );
    }

    #[test]
    fn shared_disjoint_loop_region_budget_preserves_legacy_above_cap() {
        for count in [MAX_SHARED_LOOPS, MAX_SHARED_LOOPS + 1] {
            let (class, method, _) = shared_disjoint_loop_fixture(count, false, false, false);
            let front = crate::native_method::MethodFrontEnd::build(&class, &method).unwrap();
            let len = method.code.as_ref().unwrap().instructions.len();
            let cfg =
                crate::native_cfg::ControlFlowGraph::from_decoded_loop(&front.ir, len).unwrap();
            let canonical = decoded_natural_loops(&front.ir, &cfg);
            let selected = Graph::straight_line(&class, &method).unwrap();
            if count == MAX_SHARED_LOOPS {
                assert_eq!(canonical.unwrap().len(), count);
                assert_eq!(selected.unwrap().shared_loops.len(), count);
            } else {
                assert!(
                    canonical
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("region budget")
                );
                assert!(selected.is_none());
            }
        }
    }

    fn shared_loop_outer_fixture(variant: usize) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let (class, mut method, mut regs) = if variant == 4 {
            shared_loop_edges_fixture(true)
        } else {
            shared_natural_loop_fixture(0x28)
        };
        let words = match variant {
            0 => vec![
                0x0012, 0x0112, 0x0239, 12, 0x1071, 0, 0, 0x3035, 7, 0x0190, 0x0001, 0x00d8,
                0x0100, 0xf728, 0x0238, 4, 0x01d8, 0x0101, 0x010f,
            ],
            1 => vec![
                0x0012, 0x0112, 0x0239, 4, 0x0029, 12, 0x1071, 0, 0, 0x3035, 7, 0x0190, 0x0001,
                0x00d8, 0x0100, 0xf728, 0x0238, 4, 0x01d8, 0x0101, 0x010f,
            ],
            2 => vec![
                0x0012, 0x0112, 0x0239, 4, 0x01d8, 0x0101, 0x1071, 0, 0, 0x3035, 7, 0x0190, 0x0001,
                0x00d8, 0x0100, 0xf728, 0x0238, 4, 0x01d8, 0x0101, 0x010f,
            ],
            3 => vec![
                0x0012, 0x0112, 0x0239, 13, 0x1071, 0, 0, 0x3035, 7, 0x0190, 0x0001, 0x00d8,
                0x0100, 0xf728, 0x010f, 0x1112, 0x010f,
            ],
            _ => {
                let mut words = std::mem::take(&mut method.code.as_mut().unwrap().instructions);
                words.splice(2..2, [0x0239, (words.len() - 1) as u16]);
                let exit = words.len() - 1;
                words.splice(exit..exit, [0x0238, 4, 0x01d8, 0x0101]);
                words
            }
        };
        method.code.as_mut().unwrap().instructions = words;
        assign(
            &mut regs,
            2,
            Value {
                text: "mode".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            },
        )
        .unwrap();
        (class, method, regs)
    }

    #[test]
    fn shared_loop_outer_poison_preserves_bypass_prefix_tail_and_terminal_arms() {
        for variant in 0..5 {
            let (class, mut method, regs) = shared_loop_outer_fixture(variant);
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let region = graph.shared_loops[0];
            assert_eq!(region.start, if matches!(variant, 1 | 2) { 6 } else { 4 });
            assert_eq!(
                graph.shared_branches.len(),
                if variant == 3 {
                    1
                } else if variant == 4 {
                    3
                } else {
                    2
                }
            );
            let outer = graph.shared_branches[0];
            assert_eq!(outer.branch, 2);
            assert_eq!(
                outer.join,
                if variant == 2 {
                    region.start
                } else if variant == 3 {
                    17
                } else {
                    region.exit
                }
            );
            assert_eq!(
                graph.shared_loop_edges.len(),
                if variant == 4 { 4 } else { 0 }
            );
            assert!(graph.live_at(region.start, 1));
            if variant != 3 {
                assert!(graph.live_at(region.start, 2));
            }
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline.1);
            assert!(!expected.links.is_empty());
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == result);
            assert_eq!(expected.text, actual.text);
            assert_eq!(
                expected
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>(),
                actual
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn malformed_shared_loop_outer_plans_and_external_entries_reject_without_retry() {
        let (class, method, regs) = shared_loop_outer_fixture(0);
        let end = method.code.as_ref().unwrap().instructions.len();
        for mutation in 0..9 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            match mutation {
                0 => graph.shared_branches[0].join = 13,
                1 => graph.shared_branches[0].taken = 9,
                2 => graph.shared_branches[0].fallthrough = 7,
                3 => graph.shared_branches[0].branch = 7,
                4 => graph.shared_branches.swap(0, 1),
                5 => {
                    graph.shared_branches.pop();
                }
                6 | 7 => {
                    let ir = &mut graph.front_end.as_mut().unwrap().ir;
                    ir.instructions
                        .iter_mut()
                        .find(|i| i.pc == 2)
                        .unwrap()
                        .branch_target = Some(if mutation == 6 { 9 } else { 13 });
                    graph.shared_cfg = Some(
                        crate::native_cfg::ControlFlowGraph::from_decoded_loop(ir, end).unwrap(),
                    );
                }
                _ => graph
                    .front_end
                    .as_mut()
                    .unwrap()
                    .ir
                    .instructions
                    .iter_mut()
                    .find(|i| i.pc == 14)
                    .unwrap()
                    .reads
                    .clear(),
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        for target in [7usize, 9, 13] {
            let (_, mut excluded, _) = shared_loop_outer_fixture(0);
            excluded.code.as_mut().unwrap().instructions[3] = (target - 2) as u16;
            assert!(Graph::straight_line(&class, &excluded).unwrap().is_none());
        }
    }

    fn shared_loop_edges_fixture(nested: bool) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let (class, mut method, regs) = shared_natural_loop_fixture(0x28);
        method.code.as_mut().unwrap().instructions = if nested {
            vec![
                0x0012,
                0x0112,
                0x1071,
                0,
                0,
                0x3035,
                18,
                0x0338,
                9,
                0x00d8,
                0x0100,
                0x3032,
                (-9i16) as u16,
                0x0038,
                10,
                0x0528,
                0x00d8,
                0x0100,
                0x3032,
                (-16i16) as u16,
                0x0038,
                3,
                0xec28,
                0x010f,
            ]
        } else {
            vec![
                0x0012,
                0x0112,
                0x1071,
                0,
                0,
                0x3035,
                15,
                0x00d8,
                0x0100,
                0x3032,
                (-7i16) as u16,
                0x0038,
                9,
                0x3032,
                (-11i16) as u16,
                0x0038,
                5,
                0x0190,
                0x0001,
                0xef28,
                0x010f,
            ]
        };
        (class, method, regs)
    }

    #[test]
    fn shared_loop_edge_composition_poison_preserves_order_links_and_carried_state() {
        for nested in [false, true] {
            let (class, mut method, regs) = shared_loop_edges_fixture(nested);
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert_eq!(graph.shared_loop_edges.len(), 4);
            assert_eq!(
                graph
                    .shared_loop_edges
                    .iter()
                    .filter(|e| e.kind == SharedLoopEdgeKind::Break)
                    .count(),
                2
            );
            assert_eq!(graph.shared_branches.len(), usize::from(nested));
            if nested {
                assert_eq!(graph.shared_branches[0].join, 20);
            }
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(expected.text.matches("continue;").count(), 2);
            assert_eq!(expected.text.matches("break;").count(), 3);
            assert!(!expected.links.is_empty());
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == result);
            assert_eq!(expected.text, actual.text);
            assert_eq!(
                expected
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>(),
                actual
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn shared_loop_edge_composition_each_edge_generates_liveness() {
        let (class, mut method, _) = shared_natural_loop_fixture(0x28);
        method.code.as_mut().unwrap().registers = 8;
        method.code.as_mut().unwrap().instructions = vec![
            0x0012,
            0x0212,
            0x1071,
            0,
            2,
            0x1112,
            0x6035,
            21,
            0x7032,
            (-6i16) as u16,
            0x0213,
            2,
            0x7032,
            15,
            0x0113,
            2,
            0x7032,
            (-14i16) as u16,
            0x0213,
            3,
            0x7032,
            7,
            0x0113,
            3,
            0x00d8,
            0x0100,
            0xe828,
            0x010f,
        ];
        let mut regs = vec![None; 8];
        for (r, name) in [(6, "limit"), (7, "stop")] {
            assign(
                &mut regs,
                r,
                Value {
                    text: name.into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
        }
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(graph.shared_loop_edges.len(), 4);
        render(
            &class,
            &method,
            &graph,
            0,
            28,
            regs,
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .unwrap();
        for edge in &graph.shared_loop_edges {
            let reg = if edge.kind == SharedLoopEdgeKind::Continue {
                2
            } else {
                1
            };
            let mut cut = graph.shared_cfg.as_ref().unwrap().clone();
            let block = cut.block_at[&edge.branch];
            // The block prefix writes the other register, so this mask
            // isolates the value observed by the selected special edge.
            assert!(graph.live_at(cut.blocks[block].start, reg));
            cut.blocks[block].successors.remove(0);
            let live = decoded_cfg_liveness(&graph.front_end.as_ref().unwrap().ir, &cut, 8)
                .unwrap()
                .unwrap();
            assert_eq!(
                live.bits[block * live.stride] & (1 << reg),
                0,
                "edge {}",
                edge.branch
            );
        }
    }

    #[test]
    fn shared_loop_edge_composition_each_escape_checks_restored_literal() {
        for changed in [None, Some(0), Some(1), Some(2), Some(3)] {
            let mut words = vec![
                0x0012, 0x0114, 0x2345, 0x7fc1, 0x4035, 0, 0x1201, 0x0112, 0x2101,
            ];
            let mut edges = Vec::new();
            for index in 0..4 {
                if changed == Some(index) {
                    words.push(0x0112);
                }
                let pc = words.len();
                words.extend([if index % 2 == 0 { 0x5032 } else { 0x6032 }, 0]);
                edges.push((pc, index % 2 == 0));
                if changed == Some(index) {
                    words.push(0x2101);
                }
            }
            words.extend([0x00d8, 0x0100]);
            let latch = words.len();
            words.push(0x28 | (((4isize - latch as isize) as i8 as u8 as u16) << 8));
            let exit = words.len();
            words.push(0x010f);
            words[5] = (exit - 4) as u16;
            for (pc, continuing) in edges {
                words[pc + 1] =
                    (if continuing { 4isize } else { exit as isize } - pc as isize) as i16 as u16;
            }
            let end = words.len();
            let (class, mut method) = fixture(words, 7, 0, vec![], "F");
            let mut regs = vec![None; 7];
            for (r, name) in [(4, "limit"), (5, "stop"), (6, "breakAt")] {
                assign(
                    &mut regs,
                    r,
                    Value {
                        text: name.into(),
                        ty: "I".into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )
                .unwrap();
            }
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert_eq!(graph.shared_loop_edges.len(), 4);
            let invariant = loop_invariant_registers(
                method.code.as_ref().unwrap(),
                &graph,
                graph.shared_loops[0],
            )
            .unwrap();
            assert_eq!(invariant[1], changed.is_none(), "edge {changed:?}");
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            );
            if changed.is_some() {
                assert!(baseline.is_err());
                continue;
            }
            let baseline = baseline.unwrap();
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == result);
            assert_eq!(expected.text, actual.text);
        }
    }

    #[test]
    fn shared_loop_edge_collection_total_control_budget_boundary() {
        // Helper/routing budget proof; repeated identical conditions are not
        // runtime termination evidence for this synthetic method.
        for count in [1022usize, 1023] {
            let mut words = vec![0x0012, 0x0038, 0];
            for _ in 0..count {
                let pc = words.len();
                words.extend([0x0038, (1isize - pc as isize) as i16 as u16]);
            }
            let latch = words.len();
            words.extend([0x0029, (1isize - latch as isize) as i16 as u16]);
            let exit = words.len();
            words.push(0x000f);
            words[2] = (exit - 1) as u16;
            let (class, method) = fixture(words, 1, 0, vec![], "I");
            let front = crate::native_method::MethodFrontEnd::build(&class, &method).unwrap();
            let region = Loop {
                parent: None,
                start: 1,
                latch,
                guard: Some(1),
                exit,
                tail: None,
            };
            let canonical = decoded_loop_edges(&front.ir, region);
            let selected = Graph::straight_line(&class, &method);
            if count == 1022 {
                assert_eq!(canonical.unwrap().len(), 1022);
                assert_eq!(selected.unwrap().unwrap().shared_loop_edges.len(), 1022);
            } else {
                assert!(
                    canonical
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("control budget")
                );
                assert!(
                    selected
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("branch budget")
                );
            }
        }
    }

    #[test]
    fn malformed_shared_loop_edge_collection_rejects_duplicates_order_and_region() {
        let (class, method, regs) = shared_loop_edges_fixture(true);
        let end = method.code.as_ref().unwrap().instructions.len();
        for mutation in 0..9 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            match mutation {
                0 => graph.shared_loop_edges.push(graph.shared_loop_edges[0]),
                1 => graph.shared_loop_edges.swap(0, 1),
                2 => {
                    graph.shared_loop_edges.remove(3);
                }
                3 => graph.shared_loop_edges[3].branch = 5,
                4 => graph.shared_loop_edges[2].taken = 5,
                5 => graph.shared_loop_edges[3].fallthrough = 23,
                6 => graph.shared_loop_edges[2].kind = SharedLoopEdgeKind::Break,
                7 => {
                    let cfg = graph.shared_cfg.as_mut().unwrap();
                    let block = cfg.block_at[&18];
                    cfg.blocks[block].successors.swap(0, 1);
                }
                _ => graph
                    .front_end
                    .as_mut()
                    .unwrap()
                    .ir
                    .instructions
                    .iter_mut()
                    .find(|i| i.pc == 20)
                    .unwrap()
                    .reads
                    .clear(),
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
    }

    fn shared_loop_continue_fixture(variant: usize) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let (class, mut method, regs) = shared_natural_loop_fixture(0x28);
        method.code.as_mut().unwrap().instructions = match variant {
            0 => vec![
                0x0012,
                0x0112,
                0x1071,
                0,
                0,
                0x3035,
                9,
                0x00d8,
                0x0100,
                0x0039,
                (-7i16) as u16,
                0x0190,
                0x0001,
                0xf528,
                0x010f,
            ],
            1 => vec![
                0x0012,
                0x0112,
                0x1071,
                0,
                0,
                0x3035,
                13,
                0x0338,
                6,
                0x00d8,
                0x0100,
                0x0039,
                (-9i16) as u16,
                0x0038,
                5,
                0x0190,
                0x0001,
                0xf128,
                0x010f,
            ],
            _ => vec![
                0x0012,
                0x0112,
                0x1071,
                0,
                0,
                0x3035,
                9,
                0x0190,
                0x0001,
                0x00d8,
                0x0100,
                0x0039,
                (-9i16) as u16,
                0xf528,
                0x010f,
            ],
        };
        (class, method, regs)
    }

    #[test]
    fn shared_loop_continue_poison_preserves_header_sync_links_and_state() {
        for variant in 0..3 {
            let (class, mut method, regs) = shared_loop_continue_fixture(variant);
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let edge = graph.edge_kind(SharedLoopEdgeKind::Continue).unwrap();
            assert_eq!(edge.taken, 2);
            assert_eq!(edge.branch, if variant == 0 { 9 } else { 11 });
            assert_eq!(graph.shared_branches.len(), usize::from(variant == 1));
            assert_eq!(
                graph
                    .shared_loop_edges
                    .iter()
                    .any(|edge| edge.kind == SharedLoopEdgeKind::Break),
                variant == 1
            );
            if variant == 2 {
                assert_eq!(edge.fallthrough, graph.shared_loops[0].latch);
            }
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(expected.text.contains("continue;"));
            assert!(!expected.links.is_empty());
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == result);
            assert_eq!(expected.text, actual.text);
            assert_eq!(
                expected
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>(),
                actual
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn shared_loop_continue_backedge_generates_header_value_liveness() {
        let (class, mut method, mut regs) = shared_natural_loop_fixture(0x28);
        method.code.as_mut().unwrap().registers = 5;
        method.code.as_mut().unwrap().instructions = vec![
            0x0012,
            0x0112,
            0x1071,
            0,
            1,
            0x3035,
            9,
            0x4032,
            (-5i16) as u16,
            0x0113,
            2,
            0x00d8,
            0x0100,
            0xf528,
            0x000f,
        ];
        regs.push(None);
        assign(
            &mut regs,
            4,
            Value {
                text: "stop".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            },
        )
        .unwrap();
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(
            graph
                .edge_kind(SharedLoopEdgeKind::Continue)
                .unwrap()
                .branch,
            7
        );
        render(
            &class,
            &method,
            &graph,
            0,
            15,
            regs,
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .unwrap();
        // Header call observes v1 only on the continue path: fallthrough writes
        // it before reaching the ordinary latch, and the exit returns v0.
        assert!(graph.live_at(7, 1));
        let mut cut = graph.shared_cfg.as_ref().unwrap().clone();
        let block = cut.block_at[&7];
        assert_eq!(cut.blocks[block].start, 7);
        cut.blocks[block].successors.remove(0);
        let live = decoded_cfg_liveness(&graph.front_end.as_ref().unwrap().ir, &cut, 5)
            .unwrap()
            .unwrap();
        assert_eq!(live.bits[block * live.stride] & (1 << 1), 0);
    }

    #[test]
    fn shared_loop_continue_restored_literal_checks_early_backedge() {
        let words = vec![
            0x0012,
            0x0114,
            0x2345,
            0x7fc1,
            0x4035,
            10,
            0x1201,
            0x0112,
            0x2101,
            0x5032,
            (-5i16) as u16,
            0x00d8,
            0x0100,
            0xf728,
            0x010f,
        ];
        let (class, mut method) = fixture(words, 6, 0, vec![], "F");
        let mut regs = vec![None; 6];
        for (r, name) in [(4, "limit"), (5, "stop")] {
            assign(
                &mut regs,
                r,
                Value {
                    text: name.into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
        }
        let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let mut expected = Output::default();
        let baseline = render(
            &class,
            &method,
            &graph,
            0,
            15,
            regs.clone(),
            &mut expected,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        method.code.as_mut().unwrap().instructions.fill(u16::MAX);
        graph.targets.fill(Some(usize::MAX));
        let mut actual = Output::default();
        let result = render(
            &class,
            &method,
            &graph,
            0,
            15,
            regs.clone(),
            &mut actual,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(baseline == result);
        assert_eq!(actual.text, expected.text);
        let changed = vec![
            0x0012,
            0x0114,
            0x2345,
            0x7fc1,
            0x4035,
            10,
            0x1201,
            0x0112,
            0x5032,
            (-4i16) as u16,
            0x2101,
            0x00d8,
            0x0100,
            0xf728,
            0x010f,
        ];
        let (class, method) = fixture(changed, 6, 0, vec![], "F");
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert!(
            !loop_invariant_registers(method.code.as_ref().unwrap(), &graph, graph.shared_loops[0])
                .unwrap()[1]
        );
        assert!(
            render(
                &class,
                &method,
                &graph,
                0,
                15,
                regs,
                &mut Output::default(),
                0,
                true,
                None,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_shared_loop_continue_rejects_cache_operands_and_backedges() {
        let (class, method, regs) = shared_loop_continue_fixture(1);
        let end = method.code.as_ref().unwrap().instructions.len();
        for mutation in 0..8 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            match mutation {
                0 => graph
                    .shared_loop_edges
                    .retain(|edge| edge.kind != SharedLoopEdgeKind::Continue),
                1 => {
                    graph
                        .edge_kind_mut(SharedLoopEdgeKind::Continue)
                        .unwrap()
                        .branch = 9
                }
                2 => {
                    graph
                        .edge_kind_mut(SharedLoopEdgeKind::Continue)
                        .unwrap()
                        .taken = 5
                }
                3 => {
                    graph
                        .edge_kind_mut(SharedLoopEdgeKind::Continue)
                        .unwrap()
                        .fallthrough = 17
                }
                4 => graph
                    .front_end
                    .as_mut()
                    .unwrap()
                    .ir
                    .instructions
                    .iter_mut()
                    .find(|i| i.pc == 11)
                    .unwrap()
                    .reads
                    .clear(),
                5 => {
                    let cfg = graph.shared_cfg.as_mut().unwrap();
                    let block = cfg.block_at[&11];
                    cfg.blocks[block].successors.swap(0, 1);
                }
                6 => {
                    let ir = &mut graph.front_end.as_mut().unwrap().ir;
                    ir.instructions
                        .iter_mut()
                        .find(|i| i.pc == 7)
                        .unwrap()
                        .branch_target = Some(2);
                    graph.shared_cfg = Some(
                        crate::native_cfg::ControlFlowGraph::from_decoded_loop(ir, end).unwrap(),
                    );
                }
                _ => {
                    let ir = &mut graph.front_end.as_mut().unwrap().ir;
                    ir.instructions
                        .iter_mut()
                        .find(|i| i.pc == 11)
                        .unwrap()
                        .branch_target = Some(5);
                    graph.shared_cfg = Some(
                        crate::native_cfg::ControlFlowGraph::from_decoded_loop(ir, end).unwrap(),
                    );
                }
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        for (pc, delta) in [(8, (-4i16) as u16), (12, (-6i16) as u16)] {
            let (_, mut excluded, _) = shared_loop_continue_fixture(1);
            excluded.code.as_mut().unwrap().instructions[pc] = delta;
            assert!(Graph::straight_line(&class, &excluded).unwrap().is_none());
        }
    }

    fn shared_loop_break_fixture(nested: bool) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let (class, mut method, regs) = shared_natural_loop_fixture(0x28);
        method.code.as_mut().unwrap().instructions = if nested {
            vec![
                0x0012, 0x0112, 0x1071, 0, 0, 0x3035, 12, 0x0338, 6, 0x0038, 9, 0x0190, 0x0001,
                0x0190, 0x0001, 0x00d8, 0x0100, 0xf128, 0x010f,
            ]
        } else {
            vec![
                0x0012, 0x0112, 0x1071, 0, 0, 0x3035, 9, 0x0038, 7, 0x0190, 0x0001, 0x00d8, 0x0100,
                0xf528, 0x010f,
            ]
        };
        if nested {
            method.code.as_mut().unwrap().instructions[6] = 13;
        }
        (class, method, regs)
    }

    #[test]
    fn shared_loop_break_poison_preserves_exit_sync_links_and_state() {
        for variant in 0..3 {
            let nested = variant == 1;
            let (class, mut method, regs) = shared_loop_break_fixture(nested);
            if variant == 2 {
                method.code.as_mut().unwrap().instructions = vec![
                    0x0012, 0x0112, 0x1071, 0, 0, 0x3035, 9, 0x0113, 1, 0x00d8, 0x0100, 0x0038, 3,
                    0xf528, 0x010f,
                ];
            }
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let edge = graph.edge_kind(SharedLoopEdgeKind::Break).unwrap();
            assert_eq!(
                edge.branch,
                if nested {
                    9
                } else if variant == 2 {
                    11
                } else {
                    7
                }
            );
            if variant == 2 {
                assert_eq!(edge.fallthrough, graph.shared_loops[0].latch);
            }
            assert_eq!(graph.shared_branches.len(), usize::from(nested));
            assert!(graph.live_at(edge.branch, 1));
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(expected.text.matches("break;").count() >= 2);
            assert!(!expected.links.is_empty());
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == result);
            assert_eq!(expected.text, actual.text);
            assert_eq!(
                expected
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>(),
                actual
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn shared_loop_break_restored_float_uses_proven_exit_invariant() {
        let words = vec![
            0x0012, 0x0114, 0x2345, 0x7fc1, 0x4035, 10, 0x1201, 0x0112, 0x2101, 0x5032, 5, 0x00d8,
            0x0100, 0xf728, 0x010f,
        ];
        let (class, mut method) = fixture(words, 6, 0, vec![], "F");
        let mut regs = vec![None; 6];
        for (r, name) in [(4, "limit"), (5, "stop")] {
            assign(
                &mut regs,
                r,
                Value {
                    text: name.into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
        }
        let mut legacy = Graph::new(&method.code.as_ref().unwrap().instructions).unwrap();
        legacy.live = super::super::liveness::analyze(method.code.as_ref().unwrap());
        let error = render(
            &class,
            &method,
            &legacy,
            0,
            15,
            regs.clone(),
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(
            error
                .to_string()
                .contains("unsupported register type conversion I to F"),
            "{error:#}"
        );
        let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let mut expected = Output::default();
        let baseline = render(
            &class,
            &method,
            &graph,
            0,
            15,
            regs.clone(),
            &mut expected,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        method.code.as_mut().unwrap().instructions.fill(u16::MAX);
        graph.targets.fill(Some(usize::MAX));
        let mut actual = Output::default();
        let result = render(
            &class,
            &method,
            &graph,
            0,
            15,
            regs.clone(),
            &mut actual,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(baseline == result);
        assert_eq!(actual.text, expected.text);
        // A break before restoration does not prove the entry literal invariant.
        let changed = vec![
            0x0012, 0x0114, 0x2345, 0x7fc1, 0x4035, 10, 0x1201, 0x0112, 0x5032, 6, 0x2101, 0x00d8,
            0x0100, 0xf728, 0x010f,
        ];
        let (class, method) = fixture(changed, 6, 0, vec![], "F");
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let region = graph.shared_loops[0];
        let invariant =
            loop_invariant_registers(method.code.as_ref().unwrap(), &graph, region).unwrap();
        assert!(!invariant[1]);
        assert!(
            render(
                &class,
                &method,
                &graph,
                0,
                15,
                regs,
                &mut Output::default(),
                0,
                true,
                None,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn shared_nested_loop_literal_proof_checks_child_escape_before_restore() {
        for restored in [true, false] {
            let mut words = vec![
                0x0012, 0x0114, 0x2345, 0x7fc1, 0x4035, 18, 0x0312, 0x4335, 11, 0x1201, 0x0112,
            ];
            if restored {
                words.extend([0x2101, 0x5332, 6]);
            } else {
                words.extend([0x5332, 7, 0x2101]);
            }
            words.extend([
                0x03d8, 0x0103, 0x0029, 0xfff7, 0x00d8, 0x0100, 0x0029, 0xfff0, 0x010f,
            ]);
            let (class, mut method) = fixture(words, 6, 0, vec![], "F");
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert_eq!(graph.shared_loops.len(), 2);
            let invariant = loop_invariant_registers(
                method.code.as_ref().unwrap(),
                &graph,
                graph.shared_loops[0],
            )
            .unwrap();
            assert_eq!(invariant[1], restored);
            let mut regs = vec![None; 6];
            for (r, name) in [(4, "limit"), (5, "stop")] {
                assign(
                    &mut regs,
                    r,
                    Value {
                        text: name.into(),
                        ty: "I".into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )
                .unwrap();
            }
            let mut expected = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                23,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            );
            if !restored {
                assert!(
                    result.is_err(),
                    "child escape must not emit the old literal"
                );
                continue;
            }
            let baseline = result.unwrap();
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let poisoned = render(
                &class,
                &method,
                &graph,
                0,
                23,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == poisoned);
            assert_eq!(expected.text, actual.text);
        }
    }

    #[test]
    fn shared_loop_break_exit_edge_generates_body_liveness() {
        let words = vec![
            0x0012, 0x1112, 0x3035, 8, 0x4032, 6, 0x2112, 0x00d8, 0x0100, 0xf828, 0x010f,
        ];
        let (class, method) = fixture(words, 5, 0, vec![], "I");
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(
            graph.edge_kind(SharedLoopEdgeKind::Break).unwrap().branch,
            4
        );
        let mut regs = vec![None; 5];
        for (r, name) in [(3, "limit"), (4, "stop")] {
            assign(
                &mut regs,
                r,
                Value {
                    text: name.into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
        }
        render(
            &class,
            &method,
            &graph,
            0,
            11,
            regs,
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .unwrap();
        // Only the break edge observes the header's v1: continuing execution
        // overwrites it before the latch and the next header overwrites it too.
        assert!(graph.live_at(4, 1));
        let mut cut = graph.shared_cfg.as_ref().unwrap().clone();
        let block = cut.block_at[&4];
        assert_eq!(cut.blocks[block].start, 4);
        cut.blocks[block].successors.remove(0);
        let live = decoded_cfg_liveness(&graph.front_end.as_ref().unwrap().ir, &cut, 5)
            .unwrap()
            .unwrap();
        assert_eq!(live.bits[block * live.stride] & (1 << 1), 0);
    }

    #[test]
    fn malformed_shared_loop_break_rejects_cached_exit_and_canonical_edges() {
        let (class, method, regs) = shared_loop_break_fixture(false);
        let end = method.code.as_ref().unwrap().instructions.len();
        for mutation in 0..8 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            match mutation {
                0 => graph
                    .shared_loop_edges
                    .retain(|edge| edge.kind != SharedLoopEdgeKind::Break),
                1 => {
                    graph
                        .edge_kind_mut(SharedLoopEdgeKind::Break)
                        .unwrap()
                        .branch = 9
                }
                2 => {
                    graph
                        .edge_kind_mut(SharedLoopEdgeKind::Break)
                        .unwrap()
                        .taken = 13
                }
                3 => {
                    graph
                        .edge_kind_mut(SharedLoopEdgeKind::Break)
                        .unwrap()
                        .fallthrough = 11
                }
                4 => graph
                    .front_end
                    .as_mut()
                    .unwrap()
                    .ir
                    .instructions
                    .iter_mut()
                    .find(|i| i.pc == 7)
                    .unwrap()
                    .reads
                    .clear(),
                5 => {
                    let cfg = graph.shared_cfg.as_mut().unwrap();
                    let block = cfg.block_at[&7];
                    cfg.blocks[block].successors.swap(0, 1);
                }
                6 => {
                    let ir = &mut graph.front_end.as_mut().unwrap().ir;
                    ir.instructions
                        .iter_mut()
                        .find(|i| i.pc == 7)
                        .unwrap()
                        .branch_target = Some(13);
                    graph.shared_cfg = Some(
                        crate::native_cfg::ControlFlowGraph::from_decoded_loop(ir, end).unwrap(),
                    );
                }
                _ => {
                    graph
                        .edge_kind_mut(SharedLoopEdgeKind::Break)
                        .unwrap()
                        .fallthrough = 14
                }
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        for words in [
            vec![
                0x0012, 0x0112, 0x1071, 0, 0, 0x3035, 11, 0x0038, 9, 0x0138, 8, 0x0190, 0x0001,
                0x00d8, 0x0100, 0xf328, 0x010f,
            ],
            vec![
                0x0012, 0x0112, 0x1071, 0, 0, 0x3035, 9, 0x0728, 0, 0x0190, 0x0001, 0x00d8, 0x0100,
                0xf528, 0x010f,
            ],
        ] {
            let (_, mut excluded, _) = shared_loop_break_fixture(false);
            excluded.code.as_mut().unwrap().instructions = words;
            assert!(Graph::straight_line(&class, &excluded).unwrap().is_none());
        }
    }

    #[test]
    fn shared_loop_body_poison_preserves_branch_links_and_carried_state() {
        for empty in [false, true] {
            let (class, mut method, regs) = shared_loop_body_fixture(empty);
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert_eq!(graph.shared_branches.len(), 1);
            assert_eq!(graph.shared_branches[0].join, if empty { 13 } else { 11 });
            assert!(graph.live_at(7, 3));
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(!expected.links.is_empty());
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == result);
            assert_eq!(expected.text, actual.text);
            assert_eq!(
                expected
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>(),
                actual
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn shared_loop_body_sequential_and_nested_plans_own_poisoned_branches() {
        let variants = [
            vec![
                0x0012, 0x0112, 0x1071, 0, 0, 0x3035, 13, 0x0338, 4, 0x0190, 0x0001, 0x0338, 4,
                0x0190, 0x0001, 0x00d8, 0x0100, 0xf128, 0x010f,
            ],
            vec![
                0x0012, 0x0112, 0x1071, 0, 0, 0x3035, 16, 0x0338, 9, 0x0338, 4, 0x0190, 0x0001,
                0x0190, 0x0001, 0x0328, 0x0190, 0x0001, 0x00d8, 0x0100, 0xee28, 0x010f,
            ],
        ];
        for (index, words) in variants.into_iter().enumerate() {
            let (class, mut method, regs) = shared_natural_loop_fixture(0x28);
            let end = words.len();
            method.code.as_mut().unwrap().instructions = words;
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert!(!graph.shared_loops.is_empty());
            assert_eq!(graph.shared_branches.len(), 2);
            assert_eq!(
                graph
                    .shared_branches
                    .iter()
                    .map(|p| p.join)
                    .collect::<Vec<_>>(),
                if index == 0 {
                    vec![11, 15]
                } else {
                    vec![18, 13]
                }
            );
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(!expected.links.is_empty());
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(result == baseline);
            assert_eq!(actual.text, expected.text);
            assert_eq!(
                expected
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>(),
                actual
                    .links
                    .iter()
                    .map(|l| (l.start, l.end, &l.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn shared_loop_body_restored_float_literal_survives_conditional_merge() {
        for bits in [1u32, 0x80000000, 0x7fc12345] {
            let words = vec![
                0x0014,
                bits as u16,
                (bits >> 16) as u16,
                0x0112,
                0x4132,
                10,
                0x0438,
                5,
                0x0201,
                0x0012,
                0x2001,
                0x01d8,
                0x0101,
                0xf728,
                0x000f,
            ];
            let (class, mut method) = fixture(words, 5, 0, vec![], "F");
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert_eq!(graph.shared_branches.len(), 1);
            let mut regs = vec![None; 5];
            assign(
                &mut regs,
                4,
                Value {
                    text: "limit".into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
            let mut expected = Output::default();
            let baseline = render(
                &class,
                &method,
                &graph,
                0,
                15,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let result = render(
                &class,
                &method,
                &graph,
                0,
                15,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(baseline == result);
            assert_eq!(actual.text, expected.text);
        }
    }

    #[test]
    fn malformed_shared_loop_body_rejects_plans_edges_and_operands() {
        let (class, method, regs) = shared_loop_body_fixture(false);
        let end = method.code.as_ref().unwrap().instructions.len();
        for mutation in 0..6 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            match mutation {
                0 => graph.shared_branches.clear(),
                1 => graph.shared_branches[0].join = 14,
                2 => graph.shared_branches[0].fallthrough = 7,
                3 => {
                    let cfg = graph.shared_cfg.as_mut().unwrap();
                    let block = cfg.block_at[&7];
                    cfg.blocks[block].successors[0].target = cfg.block_at[&2];
                }
                4 => {
                    graph
                        .front_end
                        .as_mut()
                        .unwrap()
                        .ir
                        .instructions
                        .iter_mut()
                        .find(|i| i.pc == 7)
                        .unwrap()
                        .branch_target = Some(14)
                }
                _ => graph
                    .front_end
                    .as_mut()
                    .unwrap()
                    .ir
                    .instructions
                    .iter_mut()
                    .find(|i| i.pc == 7)
                    .unwrap()
                    .reads
                    .clear(),
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        for (pc, word) in [(7, 0x0e), (8, 8), (8, (-4i16) as u16), (8, 0)] {
            let (_, mut excluded, _) = shared_loop_body_fixture(false);
            excluded.code.as_mut().unwrap().instructions[pc] = word;
            assert!(Graph::straight_line(&class, &excluded).unwrap().is_none());
        }
    }

    #[test]
    fn shared_natural_loop_poison_preserves_header_latch_body_links_and_state() {
        for goto in 0x28..=0x2a {
            let (class, mut method, regs) = shared_natural_loop_fixture(goto);
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let region = graph.shared_loops[0];
            assert_eq!(region.start, 2);
            assert_eq!(region.guard, Some(5));
            assert_eq!(region.latch, 11);
            // Only the next header guard reads limit. A single reverse DAG
            // sweep cannot propagate it across the latch into this body block.
            assert!(graph.live_at(7, 3));
            assert!(graph.live_at(region.start, 0) && graph.live_at(region.start, 1));
            assert!(!graph.live_at(region.start, 2));
            assert!(graph.live_at(region.exit, 1));
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            let (expected_state, expected_terminal) = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(expected.text.matches("sample.Header.touch(").count(), 1);
            assert!(!expected.links.is_empty());
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            let (state, terminal) = render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(terminal, expected_terminal);
            assert!(state == expected_state);
            assert_eq!(actual.text, expected.text, "latch opcode {goto:02x}");
            assert_eq!(
                actual
                    .links
                    .iter()
                    .map(|link| (link.start, link.end, &link.label))
                    .collect::<Vec<_>>(),
                expected
                    .links
                    .iter()
                    .map(|link| (link.start, link.end, &link.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn shared_natural_loop_poison_consumes_move_invariants_and_exit_cast() {
        for bits in [1u32, 0x80000000, 0x7fc12345] {
            let words = vec![
                0x0014,
                bits as u16,
                (bits >> 16) as u16,
                0x0112,
                0x4132,
                8,
                0x0201,
                0x0012,
                0x2001,
                0x01d8,
                0x0101,
                0xf928,
                0x000f,
            ];
            let (class, mut method) = fixture(words, 5, 0, vec![], "F");
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert!(!graph.shared_loops.is_empty());
            let mut regs = vec![None; 5];
            assign(
                &mut regs,
                4,
                Value {
                    text: "limit".into(),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
            let mut expected = Output::default();
            let (expected_state, _) = render(
                &class,
                &method,
                &graph,
                0,
                13,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            let mut actual = Output::default();
            let (state, _) = render(
                &class,
                &method,
                &graph,
                0,
                13,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(actual.text, expected.text, "float bits {bits:08x}");
            assert!(state == expected_state);
        }
        let (mut class, mut method) = fixture(
            vec![
                0x0012, 0x0112, 0x2035, 5, 0x00d8, 0x0100, 0xfb28, 0x011f, 0, 0x0111,
            ],
            3,
            0,
            vec![],
            "Ljava/lang/String;",
        );
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Ljava/lang/String;".into()],
            ..Default::default()
        });
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert!(!graph.shared_loops.is_empty());
        let mut regs = vec![None; 3];
        assign(
            &mut regs,
            2,
            Value {
                text: "limit".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            },
        )
        .unwrap();
        let mut expected = Output::default();
        render(
            &class,
            &method,
            &graph,
            0,
            10,
            regs.clone(),
            &mut expected,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(!expected.text.contains("null ="));
        assert!(expected.text.contains("java.lang.String"));
        method.code.as_mut().unwrap().instructions.fill(u16::MAX);
        let mut actual = Output::default();
        render(
            &class,
            &method,
            &graph,
            0,
            10,
            regs,
            &mut actual,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(actual.text, expected.text);
        assert_eq!(
            actual
                .links
                .iter()
                .map(|link| (link.start, link.end, &link.label))
                .collect::<Vec<_>>(),
            expected
                .links
                .iter()
                .map(|link| (link.start, link.end, &link.label))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn malformed_shared_natural_loop_rejects_metadata_and_edges_without_raw_retry() {
        use crate::native_cfg::EdgeKind;
        use crate::native_ir::ValueKind;
        let (class, method, regs) = shared_natural_loop_fixture(0x28);
        let baseline = Graph::straight_line(&class, &method).unwrap().unwrap();
        render(
            &class,
            &method,
            &baseline,
            0,
            13,
            regs.clone(),
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .unwrap();
        for mutation in 0..13 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let region = graph.shared_loops[0];
            let header = graph.shared_cfg.as_ref().unwrap().block_at[&region.start];
            let latch = graph.shared_cfg.as_ref().unwrap().block_at[&region.latch];
            match mutation {
                0 => graph.shared_loops.first_mut().unwrap().start += 1,
                1 => graph.shared_loops.first_mut().unwrap().latch -= 1,
                2 => graph.shared_loops.first_mut().unwrap().guard = None,
                3 => graph.shared_loops.first_mut().unwrap().exit += 1,
                4 => graph.shared_loops.first_mut().unwrap().tail = Some(region.exit),
                5 => graph.loops.clear(),
                6 => graph.shared_cfg.as_mut().unwrap().blocks[latch].successors[0].target = latch,
                7 => {
                    graph.shared_cfg.as_mut().unwrap().blocks[header].successors[0].kind =
                        EdgeKind::Exceptional
                }
                8 => {
                    graph
                        .front_end
                        .as_mut()
                        .unwrap()
                        .ir
                        .instructions
                        .iter_mut()
                        .find(|instruction| instruction.pc == region.latch)
                        .unwrap()
                        .branch_target = Some(region.start + 1)
                }
                9 => {
                    graph
                        .front_end
                        .as_mut()
                        .unwrap()
                        .ir
                        .instructions
                        .iter_mut()
                        .find(|instruction| instruction.pc == region.guard.unwrap())
                        .unwrap()
                        .branch_target = Some(region.exit + 1)
                }
                10 => {
                    graph
                        .front_end
                        .as_mut()
                        .unwrap()
                        .ir
                        .instructions
                        .iter_mut()
                        .find(|instruction| instruction.pc == region.guard.unwrap())
                        .unwrap()
                        .reads[0]
                        .kind = ValueKind::Reference
                }
                11 => graph
                    .front_end
                    .as_mut()
                    .unwrap()
                    .ir
                    .instructions
                    .iter_mut()
                    .find(|instruction| instruction.pc == region.latch)
                    .unwrap()
                    .reads
                    .push(crate::native_ir::RegisterOperand {
                        register: 0,
                        kind: ValueKind::Unknown32,
                    }),
                _ => {
                    graph.shared_cfg.as_mut().unwrap().blocks[header].successors[1].target =
                        latch + 1
                }
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    13,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn shared_natural_loop_dominance_and_legacy_routing_are_explicit() {
        let (class, method) = fixture(
            vec![0x0038, 4, 0x0038, 5, 0x00d8, 0x0100, 0xfc28, 0x000e],
            1,
            0,
            vec![],
            "V",
        );
        assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        let ir = crate::native_ir::DecodedMethod::decode(method.code.as_ref().unwrap()).unwrap();
        let cfg = crate::native_cfg::ControlFlowGraph::from_decoded_loop(&ir, 8).unwrap();
        assert!(
            decoded_natural_loop(&ir, &cfg)
                .err()
                .unwrap()
                .to_string()
                .contains("not uniquely dominated")
        );
        // The formerly excluded two-backedge nested shape is now canonical.
        let (class, method) = fixture(
            vec![0x0038, 8, 0x0038, 4, 0x0000, 0xfd28, 0x0000, 0xf928, 0x000e],
            1,
            0,
            vec![],
            "V",
        );
        let nested = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert_eq!(nested.shared_loops.len(), 2);
        assert_eq!(nested.shared_loops[1].parent, Some(0));
        for words in [
            vec![0x0038, 7, 0x0038, 5, 0x0000, 0xfb28, 0xfa28, 0x000e],
            vec![0x0038, 5, 0x0000, 0xfd28, 0x0000, 0x000e],
            vec![0x0038, 5, 0x0022, 0, 0xfc28, 0x000e],
            vec![0x0038, 5, 0x001d, 0x001e, 0xfc28, 0x000e],
            vec![0x0038, 5, 0x002b, 0, 0, 0xfb28, 0x000e],
        ] {
            let (class, method) = fixture(words, 1, 0, vec![], "V");
            assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        }
        let (class, mut method, _) = shared_natural_loop_fixture(0x28);
        method.name = "<init>".into();
        assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        method.name = "run".into();
        method.code.as_mut().unwrap().tries = 1;
        assert!(Graph::straight_line(&class, &method).unwrap().is_none());
    }

    #[test]
    fn shared_natural_loop_retained_literal_slots_are_never_assignment_targets() {
        let (class, method, _) = shared_natural_loop_fixture(0x28);
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let slot = Value {
            text: "null".into(),
            ty: "Ljava/lang/Object;".into(),
            literal: Some(0),
            wide_literal: None,
            raw_bits32: false,
        };
        let mut value = Value {
            text: "0".into(),
            ty: "I".into(),
            literal: Some(0),
            wide_literal: None,
            raw_bits32: false,
        };
        let mut output = Output::default();
        graph
            .carry_loop_values(&[Some(slot.clone())], &[Some(value.clone())], &mut output)
            .unwrap();
        assert!(output.text.is_empty());
        value.literal = Some(1);
        value.text = "1".into();
        assert!(
            graph
                .carry_loop_values(&[Some(slot)], &[Some(value)], &mut output)
                .is_err()
        );
    }

    fn shared_composition_fixture(
        nested: bool,
        terminal_inner: bool,
    ) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let mut words = if nested {
            vec![
                0x0038,
                8,
                0x0139,
                4,
                0x1212,
                if terminal_inner { 0x020f } else { 0x0228 },
                0x2212,
                0x0228,
                0x3212,
                0x020f,
            ]
        } else {
            vec![
                0x0038, 4, 0x1212, 0x0228, 0x2212, 0x0139, 4, 0x1312, 0x0228, 0x2312, 0x0290,
                0x0302, 0x020f,
            ]
        };
        words.splice(0..0, [0x0071, 0, 0]);
        let (mut class, method) = fixture(words, 4, 0, vec![], "I");
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Source;".into()],
            strings: vec!["touch".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let mut regs = vec![None; 4];
        for r in [0, 1] {
            assign(
                &mut regs,
                r,
                Value {
                    text: format!("input{r}"),
                    ty: "I".into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
        }
        (class, method, regs)
    }

    #[test]
    fn shared_forward_composition_poison_preserves_plans_liveness_and_navigation() {
        for (nested, terminal) in [(false, false), (true, false), (true, true)] {
            let (class, mut method, regs) = shared_composition_fixture(nested, terminal);
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            assert_eq!(graph.shared_branches.len(), 2);
            assert!(graph.shared_live.is_some());
            assert!(graph.live.is_none());
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(expected.text.matches("sample.Source.touch()").count(), 1);
            assert!(!expected.links.is_empty());
            if !nested {
                let first = graph.shared_branches[0].join;
                let second = graph.shared_branches[1].join;
                assert!(graph.live_at(first, 1) && graph.live_at(first, 2));
                assert!(!graph.live_at(first, 0) && !graph.live_at(first, 3));
                assert!(graph.live_at(second, 2) && graph.live_at(second, 3));
                assert!(!graph.live_at(second, 0) && !graph.live_at(second, 1));
            } else {
                assert!(graph.live_at(graph.shared_branches[0].join, 2));
                assert_eq!(graph.live_at(graph.shared_branches[1].join, 2), !terminal);
                if terminal {
                    assert_eq!(graph.shared_branches[1].join, 9);
                }
            }
            for instruction in graph
                .front_end
                .as_ref()
                .unwrap()
                .ir
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction.opcode, 0x28..=0x2a | 0x32..=0x3d))
            {
                method.code.as_mut().unwrap().instructions
                    [instruction.pc..instruction.pc + instruction.width]
                    .fill(u16::MAX);
            }
            graph.targets.fill(Some(usize::MAX));
            let mut actual = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(actual.text, expected.text);
            assert_eq!(
                actual
                    .links
                    .iter()
                    .map(|link| (link.start, link.end, &link.label))
                    .collect::<Vec<_>>(),
                expected
                    .links
                    .iter()
                    .map(|link| (link.start, link.end, &link.label))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn malformed_shared_forward_composition_rejects_edges_and_region_joins() {
        use crate::native_cfg::{Edge, EdgeKind};
        for nested in [false, true] {
            let (class, method, regs) = shared_composition_fixture(nested, false);
            let end = method.code.as_ref().unwrap().instructions.len();
            let baseline = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &baseline,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            for mutation in 0..7 {
                let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
                let inner_pc = graph.shared_branches[1].branch;
                let inner_block = graph.shared_cfg.as_ref().unwrap().block_at[&inner_pc];
                match mutation {
                    0 => graph.shared_branches[1].join -= 1,
                    1 => graph.shared_branches[1].join = end + 1,
                    2 => graph.shared_branches[1].join = graph.shared_branches[0].join + 1,
                    3 => {
                        graph.shared_cfg.as_mut().unwrap().blocks[inner_block].successors[0].kind =
                            EdgeKind::Exceptional
                    }
                    4 => graph.shared_cfg.as_mut().unwrap().blocks[inner_block]
                        .successors
                        .push(Edge {
                            target: inner_block + 1,
                            kind: EdgeKind::Normal,
                        }),
                    5 => {
                        let instruction = graph
                            .front_end
                            .as_mut()
                            .unwrap()
                            .ir
                            .instructions
                            .iter_mut()
                            .find(|instruction| instruction.pc == inner_pc)
                            .unwrap();
                        instruction.branch_target = Some(end - 1);
                    }
                    _ => graph.shared_branches.swap(0, 1),
                }
                assert!(
                    render(
                        &class,
                        &method,
                        &graph,
                        0,
                        end,
                        regs.clone(),
                        &mut Output::default(),
                        0,
                        true,
                        None,
                        None
                    )
                    .is_err(),
                    "nested={nested}, mutation={mutation}"
                );
            }
        }
    }

    #[test]
    fn shared_forward_composition_liveness_uses_decoded_reads_and_wide_spans() {
        let (class, method, _) = shared_composition_fixture(false, false);
        let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let first = graph.shared_branches[0].join;
        assert!(graph.live_at(first, 1));
        graph
            .front_end
            .as_mut()
            .unwrap()
            .ir
            .instructions
            .iter_mut()
            .find(|instruction| instruction.pc == first)
            .unwrap()
            .reads[0]
            .register = 0;
        graph.shared_live = decoded_cfg_liveness(
            &graph.front_end.as_ref().unwrap().ir,
            graph.shared_cfg.as_ref().unwrap(),
            4,
        )
        .unwrap();
        assert!(graph.live_at(first, 0));
        assert!(!graph.live_at(first, 1));
        let (class, method) = fixture(
            vec![0x0438, 5, 0x0216, 1, 0x0328, 0x0216, 2, 0x0210],
            5,
            0,
            vec![],
            "J",
        );
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let join = graph.shared_branches[0].join;
        assert!(graph.live_at(join, 2) && graph.live_at(join, 3));
        assert!(!graph.live_at(join, 0) && !graph.live_at(join, 1));
    }

    #[test]
    fn shared_forward_composition_liveness_budget_retains_registers() {
        let mut words = Vec::new();
        for _ in 0..2050 {
            words.extend([0x0038, 3, 0x0000]);
        }
        words.push(0x000e);
        let (_, method) = fixture(words, u16::MAX, 0, vec![], "V");
        let code = method.code.as_ref().unwrap();
        let ir = crate::native_ir::DecodedMethod::decode(code).unwrap();
        let cfg = crate::native_cfg::ControlFlowGraph::from_decoded(&ir, code.instructions.len())
            .unwrap();
        assert!(
            decoded_cfg_liveness(&ir, &cfg, usize::from(code.registers))
                .unwrap()
                .is_none()
        );
        let mut graph = Graph::empty(code.instructions.len());
        graph.shared_cfg = Some(cfg);
        assert!(graph.live_at(0, 0));
        assert!(graph.live_at(0, 65534));
    }

    fn shared_diamond_fixture(op: u8, goto: u8) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let goto_width = usize::from(goto - 0x27);
        let taken = 3 + goto_width;
        let mut words = vec![
            u16::from(op) | if op < 0x38 { 0x1000 } else { 0 },
            taken as u16,
            0x1212,
        ];
        words.extend(match goto {
            0x28 => vec![u16::from(goto) | ((goto_width + 1) as u16) << 8],
            0x29 => vec![u16::from(goto), (goto_width + 1) as u16],
            _ => vec![u16::from(goto), (goto_width + 1) as u16, 0],
        });
        words.extend([0x2212, 0x020f]);
        let (class, method) = fixture(words, 3, 0, vec![], "I");
        let regs = vec![
            Some(Value {
                text: "left".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            }),
            Some(Value {
                text: "right".into(),
                ty: "I".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            }),
            None,
        ];
        (class, method, regs)
    }

    #[test]
    fn shared_diamond_consumes_decoded_blocks_edges_and_conditions() {
        for op in 0x32..=0x3d {
            for goto in 0x28..=0x2a {
                let (class, mut method, regs) = shared_diamond_fixture(op, goto);
                let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
                assert!(graph.shared_cfg.is_some());
                let end = method.code.as_ref().unwrap().instructions.len();
                let mut expected = Output::default();
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs.clone(),
                    &mut expected,
                    0,
                    true,
                    None,
                    None,
                )
                .unwrap();
                assert!(expected.text.contains("int v0;"));
                let instructions = &graph.front_end.as_ref().unwrap().ir.instructions;
                for instruction in instructions
                    .iter()
                    .filter(|instruction| matches!(instruction.opcode, 0x28..=0x2a | 0x32..=0x3d))
                {
                    method.code.as_mut().unwrap().instructions
                        [instruction.pc..instruction.pc + instruction.width]
                        .fill(u16::MAX);
                }
                graph.targets.fill(Some(usize::MAX));
                let mut actual = Output::default();
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs,
                    &mut actual,
                    0,
                    true,
                    None,
                    None,
                )
                .unwrap();
                assert_eq!(
                    actual.text, expected.text,
                    "condition {op:02x}, goto {goto:02x}"
                );
            }
        }
        for op in [0x32, 0x33, 0x38, 0x39] {
            let (class, mut method, mut regs) = shared_diamond_fixture(op, 0x28);
            regs[0].as_mut().unwrap().ty = "Ljava/lang/String;".into();
            regs[1].as_mut().unwrap().ty = "Ljava/lang/Object;".into();
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            method.code.as_mut().unwrap().instructions[0..2].fill(u16::MAX);
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut actual = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(actual.text.contains(if op < 0x38 {
                "((java.lang.Object) left)"
            } else {
                "left"
            }));
            if op >= 0x38 {
                assert!(actual.text.contains("null"));
            }
        }
    }

    #[test]
    fn shared_diamond_uses_canonical_no_goto_and_terminal_joins() {
        for words in [
            vec![0x0038, 3, 0x1212, 0x020f],
            vec![0x0038, 4, 0x1212, 0x020f, 0x2212, 0x020f],
        ] {
            let (class, mut method) = fixture(words, 3, 0, vec![], "I");
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut regs = vec![None; 3];
            for r in [0, 2] {
                assign(
                    &mut regs,
                    r,
                    Value {
                        text: "input".into(),
                        ty: "I".into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )
                .unwrap();
            }
            let expected_join = if end == 4 { 3 } else { end };
            assert_eq!(graph.shared_branches[0].join, expected_join);
            let mut expected = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            method.code.as_mut().unwrap().instructions[0..2].fill(u16::MAX);
            let mut actual = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(actual.text, expected.text);
        }
    }

    #[test]
    fn shared_diamond_poison_preserves_effectful_prefix_navigation() {
        let (mut class, mut method, regs) = shared_diamond_fixture(0x32, 0x28);
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Source;".into()],
            strings: vec!["touch".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        method
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(0..0, [0x0071, 0, 0]);
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let end = method.code.as_ref().unwrap().instructions.len();
        let mut expected = Output::default();
        render(
            &class,
            &method,
            &graph,
            0,
            end,
            regs.clone(),
            &mut expected,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(!expected.links.is_empty());
        assert_eq!(expected.text.matches("sample.Source.touch()").count(), 1);
        for instruction in graph
            .front_end
            .as_ref()
            .unwrap()
            .ir
            .instructions
            .iter()
            .filter(|instruction| matches!(instruction.opcode, 0x28..=0x2a | 0x32..=0x3d))
        {
            method.code.as_mut().unwrap().instructions
                [instruction.pc..instruction.pc + instruction.width]
                .fill(u16::MAX);
        }
        let mut actual = Output::default();
        render(
            &class,
            &method,
            &graph,
            0,
            end,
            regs,
            &mut actual,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(actual.text, expected.text);
        assert_eq!(
            actual
                .links
                .iter()
                .map(|link| (link.start, link.end, &link.label))
                .collect::<Vec<_>>(),
            expected
                .links
                .iter()
                .map(|link| (link.start, link.end, &link.label))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn shared_diamond_eligibility_preserves_excluded_legacy_shapes() {
        // A sole malformed backward condition is now selected for posttest
        // validation and fails closed instead of retrying legacy rendering.
        let (class, method) = fixture(vec![0x0038, 0xffff, 0x000e], 3, 0, vec![], "V");
        assert!(Graph::straight_line(&class, &method).is_err());
        for words in [
            vec![0x0038, 4, 0x0000, 0xff28, 0x000e],
            vec![0x002b, 0, 0],
            vec![0x0022, 0],
            vec![0x0024, 0, 0],
            vec![0x001d, 0x001e, 0x000e],
        ] {
            let (class, method) = fixture(words, 3, 0, vec![], "V");
            assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        }
        let (class, mut method, _) = shared_diamond_fixture(0x32, 0x28);
        method.name = "<init>".into();
        assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        method.name = "run".into();
        method.code.as_mut().unwrap().tries = 1;
        assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        let (class, method) = fixture(vec![0x000e], 0, 0, vec![], "V");
        assert!(
            Graph::straight_line(&class, &method)
                .unwrap()
                .unwrap()
                .shared_cfg
                .is_none()
        );
    }

    #[test]
    fn malformed_shared_diamond_gotos_never_retry_raw() {
        use crate::native_ir::{PoolKind, PoolReference, RegisterOperand, ValueKind};
        for op in 0x28..=0x2a {
            let (class, method, regs) = shared_diamond_fixture(0x38, op);
            let end = method.code.as_ref().unwrap().instructions.len();
            let baseline = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &baseline,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            for mutation in 0..8 {
                let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
                let goto = &mut graph.front_end.as_mut().unwrap().ir.instructions[2];
                match mutation {
                    0 => goto.reads.push(RegisterOperand {
                        register: 0,
                        kind: ValueKind::Unknown32,
                    }),
                    1 => goto.writes.push(RegisterOperand {
                        register: 0,
                        kind: ValueKind::Unknown32,
                    }),
                    2 => goto.width += 1,
                    3 => goto.may_throw = true,
                    4 => goto.literal = Some(0),
                    5 => {
                        goto.reference = Some(PoolReference {
                            kind: PoolKind::Type,
                            index: 0,
                        })
                    }
                    6 => goto.prototype = Some(0),
                    _ => goto.payload_target = Some(0),
                }
                assert!(
                    render(
                        &class,
                        &method,
                        &graph,
                        0,
                        end,
                        regs.clone(),
                        &mut Output::default(),
                        0,
                        true,
                        None,
                        None
                    )
                    .is_err()
                );
            }
        }
    }

    #[test]
    fn malformed_shared_diamond_never_retries_raw_cfg() {
        use crate::native_cfg::{Edge, EdgeKind};
        use crate::native_ir::{PoolKind, PoolReference, ValueKind};
        let (class, method, regs) = shared_diamond_fixture(0x32, 0x28);
        let end = method.code.as_ref().unwrap().instructions.len();
        let baseline = Graph::straight_line(&class, &method).unwrap().unwrap();
        render(
            &class,
            &method,
            &baseline,
            0,
            end,
            regs.clone(),
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .unwrap();
        for mutation in 0..24 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let branch = &mut graph.front_end.as_mut().unwrap().ir.instructions[0];
            match mutation {
                0 => branch.reads.clear(),
                1 => branch.reads.push(branch.reads[0]),
                2 => branch.reads[0].kind = ValueKind::Bits32,
                3 => branch.reads[1].kind = ValueKind::Reference,
                4 => branch.reads[0].register = 3,
                5 => branch.writes.push(branch.reads[0]),
                6 => branch.width = 1,
                7 => branch.may_throw = true,
                8 => branch.literal = Some(0),
                9 => {
                    branch.reference = Some(PoolReference {
                        kind: PoolKind::Type,
                        index: 0,
                    })
                }
                10 => branch.prototype = Some(0),
                11 => branch.payload_target = Some(0),
                12 => branch.branch_target = None,
                13 => branch.branch_target = Some(1),
                14 => branch.branch_target = Some(end),
                15 => branch.branch_target = Some(5),
                16 => graph.shared_cfg.as_mut().unwrap().blocks[0]
                    .successors
                    .swap(0, 1),
                17 => {
                    graph.shared_cfg.as_mut().unwrap().blocks[0].successors[0].kind =
                        EdgeKind::Exceptional
                }
                18 => {
                    graph.shared_cfg.as_mut().unwrap().blocks[0].successors[0].target = usize::MAX
                }
                19 => graph.shared_cfg.as_mut().unwrap().blocks[1]
                    .successors
                    .push(Edge {
                        target: 2,
                        kind: EdgeKind::Normal,
                    }),
                20 => graph.shared_cfg.as_mut().unwrap().blocks[1].start += 1,
                21 => {
                    graph.shared_cfg.as_mut().unwrap().block_at.remove(&2);
                }
                22 => graph.shared_branches[0].join -= 1,
                _ => graph.front_end.as_mut().unwrap().ir.instructions[2].branch_target = Some(4),
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn shared_throw_ignores_poisoned_raw_operands() {
        for (ty, literal, declared, caught, expected) in [
            (
                "Ljava/lang/RuntimeException;",
                None,
                false,
                false,
                "failure",
            ),
            ("Ljava/io/IOException;", None, true, false, "failure"),
            ("I", Some(0), false, false, "null"),
            ("Ljava/lang/Throwable;", None, false, true, "caught"),
        ] {
            let (class, mut method) = fixture(vec![0x0127], 2, 0, vec![], "V");
            if declared {
                method.thrown_types.push(ty.into());
            }
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            if caught {
                graph.caught_values.borrow_mut().insert("failure".into());
                graph
                    .catch_rethrows
                    .borrow_mut()
                    .insert("failure".into(), "caught".into());
            }
            let regs = vec![
                None,
                Some(Value {
                    text: "failure".into(),
                    ty: ty.into(),
                    literal,
                    wide_literal: None,
                    raw_bits32: false,
                }),
            ];
            let mut baseline = Output::default();
            let (_, terminal) = render(
                &class,
                &method,
                &graph,
                0,
                1,
                regs.clone(),
                &mut baseline,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(terminal);
            method.code.as_mut().unwrap().instructions[0] = u16::MAX;
            let mut actual = Output::default();
            let (_, terminal) = render(
                &class,
                &method,
                &graph,
                0,
                1,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(terminal);
            assert_eq!(actual.text, baseline.text);
            assert!(actual.text.contains(&format!("throw {expected};")));
        }
    }

    #[test]
    fn malformed_shared_throw_never_retries_raw() {
        use crate::native_ir::{PoolKind, PoolReference, ValueKind};
        let (class, method) = fixture(vec![0x0127], 2, 0, vec![], "V");
        let regs = vec![
            None,
            Some(Value {
                text: "failure".into(),
                ty: "Ljava/lang/RuntimeException;".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            }),
        ];
        let baseline = Graph::straight_line(&class, &method).unwrap().unwrap();
        render(
            &class,
            &method,
            &baseline,
            0,
            1,
            regs.clone(),
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .unwrap();
        for mutation in 0..12 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let instruction = &mut graph.front_end.as_mut().unwrap().ir.instructions[0];
            match mutation {
                0 => instruction.reads.clear(),
                1 => instruction.reads.push(instruction.reads[0]),
                2 => instruction.writes.push(instruction.reads[0]),
                3 => instruction.reads[0].kind = ValueKind::Unknown32,
                4 => instruction.reads[0].register = 2,
                5 => instruction.width = 2,
                6 => instruction.may_throw = false,
                7 => instruction.literal = Some(0),
                8 => {
                    instruction.reference = Some(PoolReference {
                        kind: PoolKind::Type,
                        index: 0,
                    })
                }
                9 => instruction.prototype = Some(0),
                10 => instruction.branch_target = Some(0),
                _ => instruction.payload_target = Some(0),
            }
            assert!(
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    1,
                    regs.clone(),
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn shared_throw_preserves_constructor_and_terminal_guards() {
        let (mut class, mut method) = fixture(vec![0x0127], 2, 0, vec![], "V");
        method.name = "<init>".into();
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let regs = vec![
            None,
            Some(Value {
                text: "failure".into(),
                ty: "Ljava/lang/RuntimeException;".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            }),
        ];
        method.code.as_mut().unwrap().instructions[0] = u16::MAX;
        let mut actual = Output::default();
        let (_, terminal) = render(
            &class,
            &method,
            &graph,
            0,
            1,
            regs.clone(),
            &mut actual,
            0,
            false,
            None,
            None,
        )
        .unwrap();
        assert!(terminal);
        assert!(actual.text.contains("throw failure;"));
        assert!(actual.text.contains("super();"));
        class.superclass = Some("Lsample/UnknownSuper;".into());
        let error = render(
            &class,
            &method,
            &graph,
            0,
            1,
            regs,
            &mut Output::default(),
            0,
            false,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("throw before initialization"));
        let (class, method) = fixture(vec![0x0027, 0x000e], 1, 0, vec![], "V");
        assert!(Graph::straight_line(&class, &method).is_err());
    }

    fn shared_array_type_fixture(op: u8) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let mut regs = vec![None; 6];
        let mut set = |r, ty: &str, text: &str| {
            assign(
                &mut regs,
                r,
                Value {
                    text: text.into(),
                    ty: ty.into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap()
        };
        let (mut words, ret) = match op {
            0x1f => {
                set(0, "Ljava/lang/Object;", "source");
                (vec![0x001f, 0], "Ljava/lang/String;")
            }
            0x20 => {
                set(2, "Ljava/lang/Object;", "source");
                (vec![0x2020, 0], "Z")
            }
            0x21 => {
                set(2, "[I", "array");
                (vec![0x2021], "I")
            }
            0x23 => {
                set(2, "I", "size");
                (vec![0x2023, 1], "[Ljava/lang/String;")
            }
            _ => {
                let put = op >= 0x4b;
                let family = op - if put { 0x4b } else { 0x44 };
                let element = ["I", "J", "Ljava/lang/String;", "Z", "B", "C", "S"][family as usize];
                set(2, &format!("[{element}"), "array");
                set(3, "I", "index");
                if put {
                    set(
                        0,
                        if family == 2 {
                            "Ljava/lang/Object;"
                        } else if family >= 4 {
                            "I"
                        } else {
                            element
                        },
                        "source",
                    );
                }
                (vec![u16::from(op), 0x0302], if put { "V" } else { element })
            }
        };
        words.push(if ret == "V" {
            0x0e
        } else if wide(ret) {
            0x10
        } else if reference(ret) {
            0x11
        } else {
            0x0f
        });
        let (mut class, method) = fixture(words, 6, 0, vec![], ret);
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Ljava/lang/String;".into(), "[Ljava/lang/String;".into()],
            ..Default::default()
        });
        (class, method, regs)
    }

    #[test]
    fn shared_array_types_ignore_poisoned_raw_operands_and_references() {
        for op in (0x1f..=0x21).chain([0x23]).chain(0x44..=0x51) {
            let (class, mut method, regs) = shared_array_type_fixture(op);
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            let mut actual = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(actual.text, expected.text, "array/type opcode {op:02x}");
            assert_eq!(
                actual
                    .links
                    .iter()
                    .map(|link| (link.start, link.end, &link.label))
                    .collect::<Vec<_>>(),
                expected
                    .links
                    .iter()
                    .map(|link| (link.start, link.end, &link.label))
                    .collect::<Vec<_>>()
            );
            if op == 0x4d {
                assert!(
                    actual
                        .text
                        .contains("((java.lang.Object[]) array)[index] = source;")
                );
            }
            if (0x4f..=0x51).contains(&op) {
                assert!(actual.text.contains("[index] = ("));
            }
        }
    }

    #[test]
    fn shared_array_type_constructor_guards_consume_all_decoded_inputs() {
        for op in (0x1f..=0x21).chain([0x23]).chain(0x44..=0x51) {
            let (class, mut method, regs) = shared_array_type_fixture(op);
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let end = graph.front_end.as_ref().unwrap().ir.instructions[0].width;
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                false,
                None,
                None,
            )
            .unwrap();
            for operand in &graph.front_end.as_ref().unwrap().ir.instructions[0].reads {
                let mut guarded = regs.clone();
                guarded[usize::from(operand.register)]
                    .as_mut()
                    .unwrap()
                    .text = "this".into();
                let error = render(
                    &class,
                    &method,
                    &graph,
                    0,
                    end,
                    guarded,
                    &mut Output::default(),
                    0,
                    false,
                    None,
                    None,
                )
                .err()
                .expect("constructor guard must reject");
                assert!(
                    error
                        .to_string()
                        .contains("uninitialized this used by array/type operation"),
                    "opcode {op:02x}, register {}, {error}",
                    operand.register
                );
            }
        }
    }

    #[test]
    fn shared_array_types_consume_changed_valid_type_identity() {
        for op in [0x1f, 0x20, 0x23] {
            let (mut class, mut method, regs) = shared_array_type_fixture(op);
            if op != 0x20 {
                method.return_type = "Ljava/lang/Object;".into();
            }
            let symbols = Arc::get_mut(&mut class.symbols).unwrap();
            symbols.types.push(if op == 0x23 {
                "[Ljava/lang/Integer;".into()
            } else {
                "Ljava/lang/Integer;".into()
            });
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            graph.front_end.as_mut().unwrap().ir.instructions[0]
                .reference
                .as_mut()
                .unwrap()
                .index = 2;
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut actual = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(actual.text.contains("java.lang.Integer"));
            assert!(
                actual
                    .links
                    .iter()
                    .any(|link| link.label == "java.lang.Integer")
            );
        }
    }

    #[test]
    fn malformed_shared_array_types_never_retry_raw() {
        use crate::native_ir::{PoolKind, PoolReference, ValueKind};
        for op in (0x1f..=0x21).chain([0x23]).chain(0x44..=0x51) {
            let (class, method, regs) = shared_array_type_fixture(op);
            let baseline = Graph::straight_line(&class, &method).unwrap().unwrap();
            let end = method.code.as_ref().unwrap().instructions.len();
            render(
                &class,
                &method,
                &baseline,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            for mutation in 0..23 {
                let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
                let instruction = &mut graph.front_end.as_mut().unwrap().ir.instructions[0];
                match mutation {
                    0 => instruction.reads.clear(),
                    1 => instruction.reads.push(instruction.reads[0]),
                    2 => instruction.writes.push(crate::native_ir::RegisterOperand {
                        register: 0,
                        kind: ValueKind::Bits32,
                    }),
                    3 if !instruction.writes.is_empty() => instruction.writes.clear(),
                    4 => instruction.reads[0].kind = ValueKind::Unknown32,
                    5 if !instruction.writes.is_empty() => {
                        instruction.writes[0].kind = ValueKind::Unknown32
                    }
                    6 => instruction.reads[0].register = 6,
                    7 if !instruction.writes.is_empty() => instruction.writes[0].register = 6,
                    8 if instruction.reference.is_some() => instruction.reference = None,
                    9 => {
                        instruction.reference = Some(PoolReference {
                            kind: PoolKind::Field,
                            index: 0,
                        })
                    }
                    10 => {
                        instruction.reference = Some(PoolReference {
                            kind: PoolKind::Type,
                            index: 65536,
                        })
                    }
                    11 => {
                        instruction.reference = Some(PoolReference {
                            kind: PoolKind::Type,
                            index: 2,
                        })
                    }
                    12 => instruction.literal = Some(0),
                    13 => instruction.may_throw = false,
                    14 => instruction.width += 1,
                    15 => instruction.branch_target = Some(0),
                    16 => instruction.payload_target = Some(0),
                    17 => instruction.prototype = Some(0),
                    18 if op == 0x1f => instruction.writes[0].register = 4,
                    19 if instruction.reads.len() >= 2 => {
                        instruction.reads[1].kind = ValueKind::Unknown32
                    }
                    20 if op == 0x45 => instruction.writes[0].register = 5,
                    21 if op >= 0x4b => instruction.reads[2].kind = ValueKind::Unknown32,
                    22 if op == 0x4c => instruction.reads[2].register = 5,
                    _ => continue,
                }
                assert!(
                    render(
                        &class,
                        &method,
                        &graph,
                        0,
                        end,
                        regs.clone(),
                        &mut Output::default(),
                        0,
                        true,
                        None,
                        None
                    )
                    .is_err(),
                    "array/type opcode {op:02x}, mutation {mutation}"
                );
            }
        }
    }

    fn shared_field_fixture(op: u8) -> (DexClass, DexMethod, Vec<Option<Value>>) {
        let is_static = op >= 0x60;
        let put = if is_static { op >= 0x67 } else { op >= 0x59 };
        let family = if op >= 0x67 {
            op - 0x67
        } else if is_static {
            op - 0x60
        } else if put {
            op - 0x59
        } else {
            op - 0x52
        };
        let ty = ["I", "J", "Lsample/Example;", "Z", "B", "C", "S"][family as usize];
        let words = vec![
            u16::from(op) | if is_static { 0 } else { 0x2000 },
            0,
            if put {
                0x0e
            } else if wide(ty) {
                0x10
            } else if reference(ty) {
                0x11
            } else {
                0x0f
            },
        ];
        let (mut class, method) = fixture(words, 4, 0, vec![], if put { "V" } else { ty });
        class.symbols = Arc::new(DexSymbols {
            types: vec![class.descriptor.clone(), ty.into()],
            strings: vec!["value".into()],
            fields: vec![(0, 1, 0)],
            ..Default::default()
        });
        let mut regs = vec![None; 4];
        assign(
            &mut regs,
            2,
            Value {
                text: "receiver".into(),
                ty: class.descriptor.to_string(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            },
        )
        .unwrap();
        if put {
            assign(
                &mut regs,
                0,
                Value {
                    text: "source".into(),
                    ty: ty.into(),
                    literal: None,
                    wide_literal: None,
                    raw_bits32: false,
                },
            )
            .unwrap();
        }
        (class, method, regs)
    }

    #[test]
    fn shared_fields_ignore_poisoned_raw_operands_and_references() {
        for op in 0x52..=0x6d {
            let (class, mut method, regs) = shared_field_fixture(op);
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let mut expected = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                3,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            let mut actual = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                3,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(actual.text, expected.text, "field opcode {op:02x}");
            assert_eq!(
                actual
                    .links
                    .iter()
                    .map(|link| (link.start, link.end, &link.label))
                    .collect::<Vec<_>>(),
                expected
                    .links
                    .iter()
                    .map(|link| (link.start, link.end, &link.label))
                    .collect::<Vec<_>>(),
                "field navigation {op:02x}"
            );
            assert!(
                actual
                    .links
                    .iter()
                    .any(|link| link.label.starts_with("sample.Example.value:"))
            );
            if (0x59..=0x5f).contains(&op) {
                assert!(actual.text.contains("receiver.value = source"));
            }
        }
    }

    #[test]
    fn shared_fields_consume_changed_valid_pool_identity() {
        for op in 0x52..=0x6d {
            let (mut class, method, regs) = shared_field_fixture(op);
            let symbols = Arc::get_mut(&mut class.symbols).unwrap();
            symbols.strings.push("other".into());
            symbols.fields.push((0, 1, 1));
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            graph.front_end.as_mut().unwrap().ir.instructions[0]
                .reference
                .as_mut()
                .unwrap()
                .index = 1;
            let mut actual = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                3,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(actual.text.contains(".other"), "field opcode {op:02x}");
            assert!(
                actual
                    .links
                    .iter()
                    .any(|link| link.label.starts_with("sample.Example.other:"))
            );
            assert!(
                !actual
                    .links
                    .iter()
                    .any(|link| link.label.starts_with("sample.Example.value:"))
            );
        }
    }

    #[test]
    fn malformed_shared_fields_never_retry_raw_operands_or_references() {
        use crate::native_ir::{PoolKind, PoolReference, ValueKind};
        for op in 0x52..=0x6d {
            let (class, method, regs) = shared_field_fixture(op);
            let baseline = Graph::straight_line(&class, &method).unwrap().unwrap();
            render(
                &class,
                &method,
                &baseline,
                0,
                3,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            for mutation in 0..20 {
                let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
                let instruction = &mut graph.front_end.as_mut().unwrap().ir.instructions[0];
                match mutation {
                    0 => instruction.reads.push(crate::native_ir::RegisterOperand {
                        register: 0,
                        kind: ValueKind::Bits32,
                    }),
                    1 => instruction.writes.push(crate::native_ir::RegisterOperand {
                        register: 0,
                        kind: ValueKind::Bits32,
                    }),
                    2 if !instruction.reads.is_empty() => {
                        instruction.reads.pop();
                    }
                    3 if !instruction.writes.is_empty() => instruction.writes.clear(),
                    4 if !instruction.reads.is_empty() => {
                        instruction.reads[0].kind = ValueKind::Unknown32
                    }
                    5 if !instruction.writes.is_empty() => {
                        instruction.writes[0].kind = ValueKind::Unknown32
                    }
                    6 if !instruction.reads.is_empty() => instruction.reads[0].register = 4,
                    7 if !instruction.writes.is_empty() => instruction.writes[0].register = 4,
                    8 => instruction.reference = None,
                    9 => {
                        instruction.reference = Some(PoolReference {
                            kind: PoolKind::Type,
                            index: 0,
                        })
                    }
                    10 => {
                        instruction.reference = Some(PoolReference {
                            kind: PoolKind::Field,
                            index: 65536,
                        })
                    }
                    11 => {
                        instruction.reference = Some(PoolReference {
                            kind: PoolKind::Field,
                            index: 1,
                        })
                    }
                    12 => instruction.literal = Some(0),
                    13 => instruction.may_throw = false,
                    14 => instruction.width = 1,
                    15 => instruction.branch_target = Some(0),
                    16 if instruction.reads.len() == 2 => {
                        instruction.reads[1].kind = ValueKind::Unknown32
                    }
                    17 if instruction
                        .writes
                        .first()
                        .is_some_and(|operand| operand.kind == ValueKind::Wide64) =>
                    {
                        instruction.writes[0].register = 3
                    }
                    18 if instruction
                        .reads
                        .last()
                        .is_some_and(|operand| operand.kind == ValueKind::Wide64) =>
                    {
                        instruction.reads.last_mut().unwrap().register = 3
                    }
                    19 => instruction.payload_target = Some(0),
                    _ => continue,
                }
                assert!(
                    render(
                        &class,
                        &method,
                        &graph,
                        0,
                        3,
                        regs.clone(),
                        &mut Output::default(),
                        0,
                        true,
                        None,
                        None
                    )
                    .is_err(),
                    "field opcode {op:02x}, mutation {mutation}"
                );
            }
        }
    }

    #[test]
    fn shared_arithmetic_ignores_poisoned_raw_operands() {
        for op in (0x2d..=0x31).chain(0x7b..=0x8f).chain(0x90..=0xe2) {
            let (words, left_ty, right_ty, ret, dst) = match op {
                0x2d..=0x31 => {
                    let spec = numeric::Compare::decode(op).unwrap();
                    (
                        vec![0x0400 | u16::from(op), 0x0200],
                        spec.input.descriptor(),
                        spec.input.descriptor(),
                        "I",
                        4,
                    )
                }
                0x7b..=0x8f => {
                    let spec = numeric::Unary::decode(op).unwrap();
                    (
                        vec![0x0400 | u16::from(op)],
                        spec.input.descriptor(),
                        "I",
                        spec.result_descriptor,
                        4,
                    )
                }
                0x90..=0xcf => {
                    let spec = numeric::Binary::decode(op).unwrap();
                    let words = if op >= 0xb0 {
                        vec![0x2000 | u16::from(op)]
                    } else {
                        vec![0x0400 | u16::from(op), 0x0200]
                    };
                    (
                        words,
                        spec.left.descriptor(),
                        spec.right.descriptor(),
                        spec.result.descriptor(),
                        if op >= 0xb0 { 0 } else { 4 },
                    )
                }
                _ => (
                    if op <= 0xd7 {
                        vec![0x0400 | u16::from(op), 0xfffd]
                    } else {
                        vec![0x0400 | u16::from(op), 0xfd00]
                    },
                    "I",
                    "I",
                    "I",
                    4,
                ),
            };
            let mut words = words;
            words.push((dst << 8) | if wide(ret) { 0x10 } else { 0x0f });
            let (class, mut method) = fixture(words, 6, 0, vec![], ret);
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let mut regs = vec![None; 6];
            for (register, ty, name) in [(0, left_ty, "left"), (2, right_ty, "right")] {
                assign(
                    &mut regs,
                    register,
                    Value {
                        text: name.into(),
                        ty: ty.into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )
                .unwrap();
            }
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            let mut actual = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(actual.text, expected.text, "opcode {op:02x}");
        }
    }

    #[test]
    fn shared_boolean_arithmetic_ignores_poisoned_raw_operands() {
        for op in [
            0x95u8, 0x96, 0x97, 0xb5, 0xb6, 0xb7, 0xd5, 0xd6, 0xd7, 0xdd, 0xde, 0xdf,
        ] {
            let (mut words, dst) = if op <= 0x97 {
                (vec![0x0400 | u16::from(op), 0x0200], 4)
            } else if op <= 0xb7 {
                (vec![0x2000 | u16::from(op)], 0)
            } else if op <= 0xd7 {
                (vec![0x0400 | u16::from(op), 1], 4)
            } else {
                (vec![0x0400 | u16::from(op), 0x0100], 4)
            };
            words.push((dst << 8) | 0x0f);
            let (class, mut method) = fixture(words, 5, 0, vec![], "Z");
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let mut regs = vec![None; 5];
            for r in [0, 2] {
                assign(
                    &mut regs,
                    r,
                    Value {
                        text: format!("flag{r}"),
                        ty: "Z".into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )
                .unwrap();
            }
            let end = method.code.as_ref().unwrap().instructions.len();
            let mut expected = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs.clone(),
                &mut expected,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            method.code.as_mut().unwrap().instructions.fill(u16::MAX);
            let mut actual = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                end,
                regs,
                &mut actual,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert_eq!(actual.text, expected.text, "boolean opcode {op:02x}");
            assert!(actual.text.contains("return "));
            assert!(!actual.text.contains("int "));
        }
    }

    #[test]
    fn malformed_shared_arithmetic_never_retries_raw() {
        use crate::native_ir::ValueKind;
        for op in [
            0x2du8, 0x2f, 0x31, 0x7b, 0x81, 0x8d, 0x90, 0xa3, 0xb0, 0xc3, 0xd0, 0xd8,
        ] {
            let (left_ty, right_ty, ret) = match op {
                0x2d..=0x31 => {
                    let spec = numeric::Compare::decode(op).unwrap();
                    (spec.input.descriptor(), spec.input.descriptor(), "I")
                }
                0x7b..=0x8f => {
                    let spec = numeric::Unary::decode(op).unwrap();
                    (spec.input.descriptor(), "I", spec.result_descriptor)
                }
                0x90..=0xcf => {
                    let spec = numeric::Binary::decode(op).unwrap();
                    (
                        spec.left.descriptor(),
                        spec.right.descriptor(),
                        spec.result.descriptor(),
                    )
                }
                _ => ("I", "I", "I"),
            };
            let mut words = match op {
                0x2d..=0x31 | 0x90..=0xaf => vec![u16::from(op), 0x0200, 0x000f],
                0xd0..=0xe2 => vec![u16::from(op), 1, 0x000f],
                0xb0..=0xcf => vec![0x2000 | u16::from(op), 0x000f],
                _ => vec![u16::from(op), 0x000f],
            };
            *words.last_mut().unwrap() = if wide(ret) { 0x0010 } else { 0x000f };
            let (class, method) = fixture(words, 6, 0, vec![], ret);
            let mut regs = vec![None; 6];
            let original = Graph::straight_line(&class, &method).unwrap().unwrap();
            let instruction = &original.front_end.as_ref().unwrap().ir.instructions[0];
            for (index, operand) in instruction.reads.iter().enumerate() {
                let ty = if index == 0 { left_ty } else { right_ty };
                assign(
                    &mut regs,
                    usize::from(operand.register),
                    Value {
                        text: format!("input{index}"),
                        ty: ty.into(),
                        literal: None,
                        wide_literal: None,
                        raw_bits32: false,
                    },
                )
                .unwrap();
            }
            let end = method.code.as_ref().unwrap().instructions.len();
            render(
                &class,
                &method,
                &original,
                0,
                end,
                regs.clone(),
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .unwrap();
            for mutation in 0..16 {
                let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
                let instruction = &mut graph.front_end.as_mut().unwrap().ir.instructions[0];
                match mutation {
                    0 => instruction.reads.clear(),
                    1 => instruction.writes.clear(),
                    2 => instruction.reads[0].kind = ValueKind::Reference,
                    3 => instruction.writes[0].kind = ValueKind::Reference,
                    4 => instruction.reads[0].register = 6,
                    5 => instruction.writes[0].register = 6,
                    6 => {
                        instruction.literal = if instruction.literal.is_some() {
                            None
                        } else {
                            Some(0)
                        }
                    }
                    7 => instruction.may_throw = !instruction.may_throw,
                    8 => instruction.branch_target = Some(0),
                    9 => instruction.width += 1,
                    10 => instruction.reads.push(instruction.reads[0]),
                    11 => instruction.writes.push(instruction.writes[0]),
                    12 if (0xb0..=0xcf).contains(&op) => instruction.writes[0].register = 4,
                    13 if op >= 0xd0 => {
                        instruction.literal = Some(if op <= 0xd7 { 32768 } else { 128 })
                    }
                    14 if instruction.reads[0].kind == ValueKind::Wide64 => {
                        instruction.reads[0].register = 5
                    }
                    15 if matches!(op, 0xa3 | 0xc3) => {
                        instruction.reads[1].kind = ValueKind::Wide64
                    }
                    _ => continue,
                }
                assert!(
                    render(
                        &class,
                        &method,
                        &graph,
                        0,
                        end,
                        regs.clone(),
                        &mut Output::default(),
                        0,
                        true,
                        None,
                        None
                    )
                    .is_err(),
                    "opcode {op:02x}, mutation {mutation}"
                );
            }
        }
    }

    #[test]
    fn shared_results_and_returns_ignore_poisoned_raw_operands() {
        for (result_op, return_op, ty, registers) in [
            (0x0a, 0x0f, "I", 1),
            (0x0b, 0x10, "J", 2),
            (0x0c, 0x11, "Ljava/lang/Object;", 1),
        ] {
            let (mut class, mut method) = fixture(
                vec![0x0071, 0, 0, result_op, return_op],
                registers,
                0,
                vec![],
                ty,
            );
            class.symbols = Arc::new(DexSymbols {
                types: vec!["Lsample/Source;".into()],
                strings: vec!["read".into()],
                protos: vec![(ty.into(), vec![])],
                methods: vec![(0, 0, 0)],
                ..Default::default()
            });
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            method.code.as_mut().unwrap().instructions[3..5].fill(u16::MAX);
            let mut output = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                5,
                vec![None; usize::from(registers)],
                &mut output,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(
                output.text.contains("sample.Source.read()"),
                "{}",
                output.text
            );
        }

        let (class, mut method) = fixture(vec![0x000e], 0, 0, vec![], "V");
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        method.code.as_mut().unwrap().instructions[0] = u16::MAX;
        let mut output = Output::default();
        render(
            &class,
            &method,
            &graph,
            0,
            1,
            vec![],
            &mut output,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(output.text.contains("return;"));
    }

    #[test]
    fn malformed_shared_results_and_returns_never_retry_raw() {
        use crate::native_ir::ValueKind;
        for (words, ret, registers, target) in [
            (vec![0x0071, 0, 0, 0x000a, 0x000f], "I", 1, 1),
            (vec![0x0071, 0, 0, 0x000b, 0x0010], "J", 2, 1),
            (
                vec![0x0071, 0, 0, 0x000c, 0x0011],
                "Ljava/lang/Object;",
                1,
                1,
            ),
            (vec![0x0071, 0, 0, 0x000a, 0x000f], "I", 1, 2),
            (vec![0x0071, 0, 0, 0x000b, 0x0010], "J", 2, 2),
            (
                vec![0x0071, 0, 0, 0x000c, 0x0011],
                "Ljava/lang/Object;",
                1,
                2,
            ),
            (vec![0x0012, 0x000f], "I", 1, 1),
            (vec![0x000e], "V", 0, 0),
        ] {
            let (mut class, method) = fixture(words, registers, 0, vec![], ret);
            class.symbols = Arc::new(DexSymbols {
                types: vec!["Lsample/Source;".into()],
                strings: vec!["read".into()],
                protos: vec![(ret.into(), vec![])],
                methods: vec![(0, 0, 0)],
                ..Default::default()
            });
            for mutation in 0..5 {
                let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
                let insn = &mut graph.front_end.as_mut().unwrap().ir.instructions[target];
                match mutation {
                    0 => {
                        if insn.opcode == 0x0e {
                            insn.reads.push(crate::native_ir::RegisterOperand {
                                register: 0,
                                kind: ValueKind::Bits32,
                            });
                        } else if insn.opcode <= 0x0c {
                            insn.writes.clear();
                        } else {
                            insn.reads.clear();
                        }
                    }
                    1 => {
                        if insn.opcode <= 0x0c && insn.opcode != 0x0e {
                            insn.writes[0].kind = ValueKind::Bits32;
                        } else if insn.opcode == 0x0e {
                            insn.writes.push(crate::native_ir::RegisterOperand {
                                register: 0,
                                kind: ValueKind::Bits32,
                            });
                        } else {
                            insn.reads[0].kind = ValueKind::Unknown32;
                        }
                    }
                    2 => insn.literal = Some(1),
                    3 => insn.may_throw = true,
                    _ => insn.width = 2,
                }
                let error = render(
                    &class,
                    &method,
                    &graph,
                    0,
                    method.code.as_ref().unwrap().instructions.len(),
                    vec![None; usize::from(registers)],
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None,
                )
                .err()
                .expect("malformed shared result/return must reject");
                assert!(error.to_string().contains("shared"), "{error:#}");
            }
        }

        let (mut class, method) = fixture(vec![0x0071, 0, 0, 0x000a, 0x000f], 2, 0, vec![], "I");
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Source;".into()],
            strings: vec!["read".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        graph.front_end.as_mut().unwrap().ir.instructions[1].writes[0].register = 1;
        let error = render(
            &class,
            &method,
            &graph,
            0,
            5,
            vec![None; 2],
            &mut Output::default(),
            0,
            true,
            None,
            None,
        )
        .err()
        .expect("shared result destination must remain bound to its call");
        assert!(error.to_string().contains("shared move-result destination"));
    }

    #[test]
    fn shared_straight_line_consumes_move_and_constant_operands() {
        // Poison every raw operand (including opcode-byte operands) only after
        // shared decoding. Each case must still use its decoded source,
        // destination and literal. Production bytecode remains immutable.
        let constants = [
            (vec![0xf012], "I", "-1"),
            (vec![0x0013, 0x8000], "I", "-32768"),
            (vec![0x0014, 0x4321, 0x8765], "I", "-2023406815"),
            (vec![0x0015, 0x8000], "I", "-2147483648"),
            (vec![0x0016, 0x8000], "J", "-32768L"),
            (vec![0x0017, 0, 0x8000], "J", "-2147483648L"),
            (
                vec![0x0018, 0x4321, 0x8765, 0xcba9, 0xfed0],
                "J",
                "-85344463938567391L",
            ),
            (vec![0x0019, 0x8000], "J", "-9223372036854775808L"),
        ];
        for (constant, ty, expected) in constants {
            let first_move = if ty == "J" { 0x04 } else { 0x01 };
            for encoding in 0..3 {
                let op = first_move + encoding;
                let movement = match encoding {
                    0 => vec![0x0200 | op],
                    1 => vec![0x0200 | op, 0],
                    _ => vec![op, 2, 0],
                };
                let mut words = constant.clone();
                words.extend(movement);
                let operands_end = words.len();
                words.push(if ty == "J" { 0x0210 } else { 0x020f });
                let (class, mut method) = fixture(words, 4, 0, vec![], ty);
                let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
                method.code.as_mut().unwrap().instructions[..operands_end].fill(0xffff);
                let mut output = Output::default();
                render(
                    &class,
                    &method,
                    &graph,
                    0,
                    operands_end + 1,
                    vec![None; 4],
                    &mut output,
                    0,
                    true,
                    None,
                    None,
                )
                .unwrap();
                assert!(
                    output.text.contains(&format!("return {expected};")),
                    "{}",
                    output.text
                );
            }
        }
        // move-object retains the null-bit-pattern interpretation while also
        // consuming shared operands for all three instruction encodings.
        for movement in [vec![0x0207], vec![0x0208, 0], vec![0x0009, 2, 0]] {
            let mut words = vec![0x0012];
            words.extend(movement);
            let operands_end = words.len();
            words.push(0x0211);
            let (class, mut method) = fixture(words, 3, 0, vec![], "Ljava/lang/Object;");
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            method.code.as_mut().unwrap().instructions[..operands_end].fill(0xffff);
            let mut output = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                operands_end + 1,
                vec![None; 3],
                &mut output,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(output.text.contains("null"));
        }
    }

    #[test]
    fn shared_straight_line_invalid_move_constant_operands_do_not_retry_raw() {
        use crate::native_ir::ValueKind;
        let (class, method) = fixture(vec![0x7012, 0x0101, 0x010f], 2, 0, vec![], "I");
        for mutation in 0..9 {
            let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            let insns = &mut graph.front_end.as_mut().unwrap().ir.instructions;
            match mutation {
                0 => insns[0].literal = None,
                1 => insns[0].literal = Some(i64::MAX),
                2 => insns[0].writes.clear(),
                3 => insns[0].writes[0].kind = ValueKind::Wide64,
                4 => insns[1].reads.clear(),
                5 => insns[1].reads[0].kind = ValueKind::Reference,
                6 => insns[1].reads[0].register = 2,
                7 => insns[1].writes[0].register = 2,
                _ => insns[1].literal = Some(7),
            }
            let error = render(
                &class,
                &method,
                &graph,
                0,
                3,
                vec![None; 2],
                &mut Output::default(),
                0,
                true,
                None,
                None,
            )
            .err()
            .expect("malformed shared operands must reject");
            assert!(error.to_string().contains("shared"), "{error:#}");
        }
    }

    #[test]
    fn shared_straight_line_consumes_reference_constant_operands() {
        for constant in [vec![0x001a, 0], vec![0x001b, 0, 0]] {
            let mut words = constant.clone();
            words.push(0x0011);
            let (mut class, mut method) = fixture(words, 1, 0, vec![], "Ljava/lang/String;");
            class.symbols = Arc::new(DexSymbols {
                strings: vec!["quote \" and newline\n".into()],
                ..Default::default()
            });
            let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
            method.code.as_mut().unwrap().instructions[..constant.len()].fill(u16::MAX);
            let mut output = Output::default();
            render(
                &class,
                &method,
                &graph,
                0,
                constant.len() + 1,
                vec![None],
                &mut output,
                0,
                true,
                None,
                None,
            )
            .unwrap();
            assert!(
                output
                    .text
                    .contains("return \"quote \\\" and newline\\n\";"),
                "{}",
                output.text
            );
        }

        let (mut class, mut method) =
            fixture(vec![0x001c, 0, 0x0011], 1, 0, vec![], "Ljava/lang/Class;");
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Target;".into()],
            ..Default::default()
        });
        let graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        method.code.as_mut().unwrap().instructions[..2].fill(u16::MAX);
        let mut output = Output::default();
        render(
            &class,
            &method,
            &graph,
            0,
            3,
            vec![None],
            &mut output,
            0,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(output.text.contains("sample.Target.class"));
        assert!(
            output
                .links
                .iter()
                .any(|link| link.label == "sample.Target")
        );
    }

    #[test]
    fn shared_straight_line_invalid_reference_constants_do_not_retry_raw() {
        use crate::native_ir::{PoolKind, ValueKind};
        for (op, words, ret) in [
            (0x1a, vec![0x001a, 0, 0x0011], "Ljava/lang/String;"),
            (0x1b, vec![0x001b, 0, 0, 0x0011], "Ljava/lang/String;"),
            (0x1c, vec![0x001c, 0, 0x0011], "Ljava/lang/Class;"),
        ] {
            let (mut class, method) = fixture(words, 1, 0, vec![], ret);
            class.symbols = Arc::new(DexSymbols {
                strings: vec!["text".into()],
                types: vec!["Lsample/Target;".into()],
                ..Default::default()
            });
            for mutation in 0..9 {
                let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
                let instruction = &mut graph.front_end.as_mut().unwrap().ir.instructions[0];
                match mutation {
                    0 => instruction.reference = None,
                    1 => {
                        instruction.reference.as_mut().unwrap().kind = if op == 0x1c {
                            PoolKind::String
                        } else {
                            PoolKind::Type
                        }
                    }
                    2 => instruction.writes.clear(),
                    3 => instruction.writes[0].kind = ValueKind::Bits32,
                    4 => instruction.writes[0].register = 1,
                    5 => instruction.reads.push(instruction.writes[0]),
                    6 => instruction.literal = Some(0),
                    7 => instruction.may_throw = false,
                    _ => instruction.reference.as_mut().unwrap().index = u32::MAX,
                }
                let error = render(
                    &class,
                    &method,
                    &graph,
                    0,
                    method.code.as_ref().unwrap().instructions.len(),
                    vec![None],
                    &mut Output::default(),
                    0,
                    true,
                    None,
                    None,
                )
                .err()
                .expect("malformed shared reference constant must reject");
                assert!(
                    mutation == 8 || error.to_string().contains("shared"),
                    "{error:#}"
                );
            }
        }
    }

    #[test]
    fn shared_straight_line_eligibility_reads_boundaries_not_operand_words() {
        let (class, method) = fixture(vec![0x0013, 0x0038, 0x000f], 1, 0, vec![], "I");
        assert!(Graph::straight_line(&class, &method).unwrap().is_some());
        assert!(reconstruct("sample.Example", &class, &method).is_ok());
        for words in [vec![0x0028], vec![0x0022], vec![0x0024], vec![0x001d]] {
            let (class, method) = fixture(words, 1, 0, vec![], "V");
            assert!(Graph::straight_line(&class, &method).unwrap().is_none());
        }
    }

    #[test]
    fn shared_straight_line_invalid_inputs_do_not_retry_legacy_renderer() {
        for words in [
            vec![0x000a, 0x000f],
            vec![0x0112, 0x000f],
            vec![0x0013],
            vec![0x0012, 0x000f, 0x1012],
            vec![0x0012, 0x000f, 0x0000],
        ] {
            let (class, method) = fixture(words, 1, 0, vec![], "I");
            assert!(Graph::straight_line(&class, &method).is_err());
            assert!(reconstruct("sample.Example", &class, &method).is_err());
        }
    }

    #[test]
    fn shared_straight_line_unlowered_ir_opcode_rejects_without_panicking() {
        let (class, method) = fixture(vec![0x00ff, 0, 0x0011], 1, 0, vec![], "Ljava/lang/Object;");
        assert!(Graph::straight_line(&class, &method).unwrap().is_some());
        assert!(reconstruct("sample.Example", &class, &method).is_err());
    }

    #[test]
    fn shared_straight_line_move_exception_without_handler_rejects() {
        let (class, method) = fixture(vec![0x000d, 0x0011], 1, 0, vec![], "Ljava/lang/Object;");
        assert!(Graph::straight_line(&class, &method).unwrap().is_some());
        let error = reconstruct("sample.Example", &class, &method)
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("move-exception outside handled entry")
        );
    }

    // Independent evaluator for this test's emitted straight-line Java subset.
    // Unknown syntax fails rather than being treated as equivalent.
    fn eval_narrow_java(body: &str, input: i32) -> i32 {
        fn expr(text: &str, values: &std::collections::HashMap<String, i32>) -> i32 {
            let text = text.trim();
            if let Some((left, right)) = text.split_once('^') {
                return expr(left, values) ^ expr(right, values);
            }
            if let Some(inner) = text.strip_prefix('!') {
                return i32::from(expr(inner, values) == 0);
            }
            if let Some(inner) = text.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
                return expr(inner, values);
            }
            match text {
                "true" => 1,
                "false" => 0,
                _ => text.parse().unwrap_or_else(|_| {
                    *values
                        .get(text)
                        .unwrap_or_else(|| panic!("unsupported expression: {text}"))
                }),
            }
        }
        let mut values = std::collections::HashMap::from([("p0".to_owned(), input)]);
        for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let line = line.strip_suffix(';').expect("Java statement");
            if let Some(ret) = line.strip_prefix("return ") {
                return expr(ret, &values);
            }
            let (declaration, rhs) = line.split_once(" = ").expect("Java assignment");
            values.insert(
                declaration.split_whitespace().last().unwrap().into(),
                expr(rhs, &values),
            );
        }
        panic!("missing return")
    }

    #[test]
    fn focused_xor_behavior_truth_table_and_integer_boundaries() {
        for mask in [0, 1] {
            for (operation, result) in [
                (vec![0x01b7], 1),
                (vec![0x0097, 0x0001], 0),
                (vec![0x10b7], 0),
            ] {
                let mut words = vec![(mask << 12) | 0x12];
                words.extend(operation);
                words.push((result << 8) | 0x0f);
                for (ty, inputs) in [
                    ("Z", vec![0, 1]),
                    ("I", vec![i32::MIN, -1, 0, 1, 2, i32::MAX]),
                ] {
                    let (class, method) = fixture(words.clone(), 2, 1, vec![ty.into()], ty);
                    let body = reconstruct("sample.Example", &class, &method).unwrap();
                    for input in inputs {
                        assert_eq!(
                            eval_narrow_java(&body.text, input),
                            input ^ i32::from(mask),
                            "{ty} {input} mask={mask}: {}",
                            body.text
                        );
                    }
                    if ty == "Z" && mask == 1 {
                        let mutant = body.text.replace("!(p0)", "p0");
                        assert_ne!(
                            eval_narrow_java(&mutant, 0),
                            1,
                            "negative control must detect lost negation"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn focused_super_dispatch_preserves_parent_target() {
        let (mut class, mut method) =
            fixture(vec![0x106f, 0, 0, 0x000a, 0x000f], 1, 1, vec![], "I");
        class.superclass = Some("Lsample/Base;".into());
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Base;".into()],
            strings: vec!["value".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        method.access_flags = 1;
        let body = reconstruct("sample.Example", &class, &method).unwrap();
        // Model different overriding implementations so a virtual-call rewrite fails.
        let eval = |source: &str| {
            let line = source
                .lines()
                .find(|line| line.contains(".value()"))
                .unwrap();
            let statement = line.trim().trim_end_matches(';');
            let rhs = statement
                .strip_prefix("return ")
                .or_else(|| statement.split_once(" = ").map(|(_, rhs)| rhs))
                .unwrap();
            match rhs {
                "super.value()" => (7, "Base.value"),
                "this.value()" => (99, "Example.value"),
                _ => panic!("unknown dispatch {rhs}"),
            }
        };
        let dex = match method.code.as_ref().unwrap().instructions[0] as u8 {
            0x6f => (7, "Base.value"),
            0x6e => (99, "Example.value"),
            _ => panic!("unknown invoke"),
        };
        assert_eq!(eval(&body.text), dex);
        assert_ne!(
            eval(&body.text.replace("super.value()", "this.value()")),
            dex
        );
    }

    #[test]
    fn register_xor_preserves_boolean_type_for_zero_one_operands() {
        for (operation, result) in [
            (vec![0x01b7], 1),
            (vec![0x0097, 0x0001], 0),
            (vec![0x10b7], 0),
        ] {
            let mut words = vec![0x1012];
            words.extend(operation);
            words.push((result << 8) | 0x0f);
            let (class, method) = fixture(words.clone(), 2, 1, vec!["Z".into()], "Z");
            let body = reconstruct("sample.Example", &class, &method).unwrap();
            assert!(body.text.contains("!(p0)"), "{}", body.text);
            let (class, method) = fixture(words, 2, 1, vec!["I".into()], "I");
            let body = reconstruct("sample.Example", &class, &method).unwrap();
            assert!(body.text.contains(" ^ "));
            assert!(!body.text.contains("!(p0)"));
        }
    }

    #[test]
    fn discarded_invoke_results_keep_calls_and_links_without_unused_locals() {
        let (mut class, method) = fixture(
            vec![0x0071, 0, 0, 0x0071, 0, 0, 0x000a, 0x000f],
            1,
            0,
            vec![],
            "I",
        );
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into()],
            strings: vec!["effect".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &class, &method).unwrap();
        assert_eq!(body.text.matches("sample.Example.effect()").count(), 2);
        assert!(
            body.text.starts_with("        sample.Example.effect();\n"),
            "{}",
            body.text
        );
        assert_eq!(
            body.links
                .iter()
                .filter(|l| l.label == "sample.Example.effect()I")
                .count(),
            2
        );
        assert!(body.text.contains("return "));
    }

    #[test]
    fn dead_incompatible_branch_values_do_not_block_reconstruction() {
        let (c, mut m) = fixture(
            vec![0x0138, 4, 0x3001, 0x0228, 0x2007, 0x000e],
            4,
            3,
            vec!["I".into(), "Ljava/lang/Object;".into(), "I".into()],
            "V",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.ends_with("        return;\n"));
        assert!(!body.text.contains("java.lang.Object v"));
        m.return_type = "Ljava/lang/Object;".into();
        m.code.as_mut().unwrap().instructions[5] = 0x0011;
        assert!(
            reconstruct("sample.Example", &c, &m).is_err(),
            "a genuinely live incompatible join must still reject"
        );
    }
    #[test]
    fn dead_switch_joins_discard_types_but_preserve_case_structure() {
        let (c, m) = fixture(
            vec![
                0x012b, 10, 0, 0x2007, 0x0528, 0x3001, 0x0328, 0x7012, 0x0128, 0x000e, 0x0100, 2,
                0, 0, 5, 0, 7, 0,
            ],
            4,
            3,
            vec!["I".into(), "Ljava/lang/Object;".into(), "I".into()],
            "V",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("switch (p0)"));
        assert!(!body.text.contains("java.lang.Object v"));
    }
    #[test]
    fn dead_join_elimination_keeps_effectful_field_reads() {
        let (mut c, m) = fixture(
            vec![0x0138, 5, 0x0060, 0, 0x0328, 0x0062, 1, 0x000e],
            2,
            1,
            vec!["I".into()],
            "V",
        );
        c.symbols = Arc::new(DexSymbols {
            types: vec![
                "Lsample/Example;".into(),
                "I".into(),
                "Ljava/lang/Object;".into(),
            ],
            strings: vec!["number".into(), "object".into()],
            fields: vec![(0, 1, 0), (0, 2, 1)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("sample.Example.number").count(), 1);
        assert_eq!(body.text.matches("sample.Example.object").count(), 1);
        assert!(!body.text.contains("java.lang.Object v2;"));
        assert_eq!(
            body.links
                .iter()
                .filter(|link| link.label.ends_with(".number:I")
                    || link.label.ends_with(".object:Ljava/lang/Object;"))
                .count(),
            2
        );
    }
    #[test]
    fn interleaved_handler_and_multiple_goto_exits_preserve_shared_tail() {
        let (c, mut m) = fixture(
            vec![
                0x0138, 7, 0x1012, 0x0628, 0x000d, 0xf012, 0x0428, 0x2012, 0x0228, 0x0128, 0x000f,
            ],
            2,
            1,
            vec!["I".into()],
            "I",
        );
        let code = m.code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions.push(crate::native_dex::DexTryRegion {
            start: 0,
            end: 9,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 4)].into(),
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(
            body.text.matches("catch (java.lang.Exception").count(),
            1,
            "{}",
            body.text
        );
        assert_eq!(body.text.matches("return ").count(), 1, "{}", body.text);
        assert!(body.text.contains("= -1;"), "{}", body.text);
    }

    #[test]
    fn interleaved_handler_cannot_lose_self_catching_throw_edges() {
        let (c, mut m) = fixture(
            vec![
                0x0138, 8, 0x1012, 0x0728, 0x000d, 0x001a, 0, 0x0428, 0x2012, 0x0228, 0x0128,
                0x000f,
            ],
            2,
            1,
            vec!["I".into()],
            "I",
        );
        let code = m.code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions.push(crate::native_dex::DexTryRegion {
            start: 0,
            end: 10,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 4)].into(),
        });
        assert!(
            reconstruct("sample.Example", &c, &m)
                .err()
                .unwrap()
                .to_string()
                .contains("throwing handler instruction inside protected region")
        );
    }

    #[test]
    fn normal_flow_cannot_enter_interleaved_handler() {
        let (c, mut m) = fixture(vec![0x0012, 0x000d, 0x0012, 0x000f], 1, 0, vec![], "I");
        let code = m.code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions.push(crate::native_dex::DexTryRegion {
            start: 0,
            end: 3,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 1)].into(),
        });
        assert!(
            reconstruct("sample.Example", &c, &m)
                .err()
                .unwrap()
                .to_string()
                .contains("normal flow enters")
        );
    }

    #[test]
    fn unused_exception_join_does_not_merge_incompatible_entry_parameter() {
        let (c, mut m) = fixture(
            vec![0x7012, 0x000e],
            1,
            1,
            vec!["Ljava/lang/Object;".into()],
            "V",
        );
        let code = m.code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions.push(crate::native_dex::DexTryRegion {
            start: 0,
            end: 1,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 1)].into(),
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("catch (java.lang.Exception"));
        assert!(!body.text.contains("java.lang.Object v"));
    }
    #[test]
    fn sibling_reference_branches_join_as_object_without_downcasts() {
        let (c, m) = fixture(
            vec![0x0138, 4, 0x2007, 0x0228, 0x3007, 0x0011],
            4,
            3,
            vec!["I".into(), "Lsample/Left;".into(), "Lsample/Right;".into()],
            "Ljava/lang/Object;",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("java.lang.Object v0;"));
        assert!(body.text.contains("((java.lang.Object) p1)"));
        assert!(body.text.contains("((java.lang.Object) p2)"));
        assert!(body.text.ends_with("        return v0;\n"));
        assert!(!body.text.contains("((sample.Left)"));
        assert!(!body.text.contains("((sample.Right)"));
    }
    #[test]
    fn null_reference_joins_preserve_reference_type_and_object_loop_slots_accept_references() {
        let null = Value {
            text: "0".into(),
            ty: "I".into(),
            literal: Some(0),
            wide_literal: None,
            raw_bits32: false,
        };
        let reference_value = Value {
            text: "p0".into(),
            ty: "Lsample/Left;".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
        assert_eq!(
            merge_type(Some(&null), Some(&reference_value)).unwrap(),
            "Lsample/Left;"
        );
        assert_eq!(
            merge_type(Some(&reference_value), Some(&null)).unwrap(),
            "Lsample/Left;"
        );
        let slot = Value {
            text: "slot".into(),
            ty: "Ljava/lang/Object;".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
        let mut out = Output::default();
        carry_loop_values(&[Some(slot)], &[Some(reference_value)], &mut out).unwrap();
        assert!(out.text.contains("((java.lang.Object) p0)"));
        assert!(!out.text.contains("((sample.Left)"));
    }
    #[test]
    fn wide_loop_slots_keep_pair_ownership_and_never_lower_a_tail() {
        let wide = Value {
            text: "p0".into(),
            ty: "J".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
        let regs = vec![
            Some(wide),
            Some(Value {
                text: "0".into(),
                ty: "<wide-tail>".into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            }),
        ];
        let graph = Graph::new(&[0x000e]).unwrap();
        let mut out = Output::default();
        let slots = loop_slots(&regs, &graph, 0, &mut out, None, None).unwrap();
        assert_eq!(slots[0].as_ref().unwrap().ty, "J");
        assert_eq!(slots[1].as_ref().unwrap().ty, "<wide-tail>");
        assert_eq!(slots[1].as_ref().unwrap().text, "0");
        assert!(out.text.contains("long v0 = p0;"));
        assert!(!out.text.contains("wide-tail"));
    }
    #[test]
    fn malformed_wide_frame_is_rejected_before_loop_lowering() {
        let regs = vec![Some(Value {
            text: "p0".into(),
            ty: "D".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        })];
        let graph = Graph::new(&[0x000e]).unwrap();
        assert!(loop_slots(&regs, &graph, 0, &mut Output::default(), None, None).is_err());
    }
    #[test]
    fn wide_diamond_merges_a_single_long_head() {
        // if (p2 == 0) return p0; else return p1; with the shared return
        // reached through v0/v1 in both arms.
        let (c, m) = fixture(
            vec![0x0438, 4, 0x2004, 0x0228, 0x0000, 0x0010],
            5,
            5,
            vec!["J".into(), "J".into(), "I".into()],
            "J",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("long v").count(), 1, "{}", body.text);
        assert!(body.text.ends_with("        return v0;\n"), "{}", body.text);
        assert!(!body.text.contains("wide-tail"));
    }
    #[test]
    fn double_branch_merges_a_single_head() {
        let (c, m) = fixture(
            vec![0x0438, 4, 0x2004, 0x0228, 0x0000, 0x0010],
            5,
            5,
            vec!["D".into(), "D".into(), "I".into()],
            "D",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("double v").count(), 1, "{}", body.text);
        assert!(body.text.ends_with("        return v0;\n"), "{}", body.text);
        assert!(!body.text.contains("wide-tail"));
    }
    #[test]
    fn ambiguous_raw_double_branch_preserves_the_literal_fallback() {
        // A branch cannot infer whether raw const-wide bits are a long or a
        // double before a downstream typed use.  Keep the existing fail-closed
        // fallback; in particular, do not invent tail locals or reinterpret
        // signed-zero bits as long values.
        let (c, m) = fixture(
            vec![
                0x0438, 8, 0x0018, 0, 0, 0, 0x8000, 0x0628, 0x0018, 0, 0, 0, 0, 0x0010,
            ],
            5,
            1,
            vec!["I".into()],
            "D",
        );
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn wide_loop_carries_only_the_long_head() {
        // while (p1 != 0) p1--; return p0;
        let (c, m) = fixture(
            vec![0x0238, 5, 0x02d8, 0xff02, 0xfc28, 0x0010],
            3,
            3,
            vec!["J".into(), "I".into()],
            "J",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("while (true)"), "{}", body.text);
        assert!(body.text.contains("long v0 = p0;"), "{}", body.text);
        assert!(body.text.ends_with("        return v0;\n"), "{}", body.text);
        assert!(!body.text.contains("wide-tail"));
    }
    #[test]
    fn switch_reference_joins_support_typed_receiver_after_merge() {
        let (mut c, m) = fixture(
            vec![
                0x012b, 14, 0, 0x2007, 0x0528, 0x3007, 0x0328, 0x4007, 0x0128, 0x106e, 0, 0,
                0x000a, 0x000f, 0x0100, 2, 0, 0, 5, 0, 7, 0,
            ],
            5,
            4,
            vec![
                "I".into(),
                "Lsample/Left;".into(),
                "Lsample/Right;".into(),
                "Ljava/lang/Object;".into(),
            ],
            "I",
        );
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Ljava/lang/Object;".into()],
            strings: vec!["hashCode".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("java.lang.Object v0;"));
        assert_eq!(body.text.matches("v0.hashCode()").count(), 1);
        assert_eq!(
            body.links
                .iter()
                .filter(|l| l.label == "java.lang.Object.hashCode()I")
                .count(),
            1
        );
    }
    #[test]
    fn invocation_widening_cast_preserves_integer_overload() {
        let (mut c, m) = fixture(vec![0x1071, 0, 0, 0x000e], 1, 1, vec!["B".into()], "V");
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into()],
            strings: vec!["accept".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        assert!(
            reconstruct("sample.Example", &c, &m)
                .unwrap()
                .text
                .contains("accept(((int) p0))")
        );
    }
    #[test]
    fn ancestor_super_calls_emit_super_and_preserve_raw_method_links() {
        let (mut c, mut m) = fixture(vec![0x206f, 0, 0x0010, 0x000e], 2, 2, vec!["I".into()], "V");
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Grandparent;".into(), "Lsample/Interface;".into()],
            strings: vec!["callback".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0), (1, 0, 0)],
            ..Default::default()
        });
        let (mut parent, _) = fixture(vec![0x000e], 0, 0, vec![], "V");
        parent.descriptor = "Lsample/Parent;".into();
        parent.superclass = Some("Lsample/Grandparent;".into());
        parent.interfaces = vec!["Lsample/Interface;".into()];
        let hierarchy =
            crate::native_hierarchy::TypeHierarchy::from_classes([&c, &parent]).unwrap();
        c.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
        m.access_flags = 1;
        for invoke in [vec![0x206f, 0, 0x0010, 0x000e], vec![0x0275, 0, 0, 0x000e]] {
            m.code.as_mut().unwrap().instructions = invoke;
            let body = reconstruct("sample.Example", &c, &m).unwrap();
            assert_eq!(body.text, "        super.callback(p0);\n        return;\n");
            assert_eq!(body.links[0].label, "sample.Grandparent.callback(I)V");
            assert_eq!(
                &body.text[body.links[0].start..body.links[0].end],
                "callback"
            );
        }
        // An interface is assignable but is not a superclass dispatch target.
        m.code.as_mut().unwrap().instructions = vec![0x206f, 1, 0x0010, 0x000e];
        assert!(reconstruct("sample.Example", &c, &m).is_err());
        // Merely having the same receiver type is insufficient: it must be this.
        m.access_flags = 9;
        m.parameters = vec!["Lsample/Example;".into(), "I".into()];
        m.code.as_mut().unwrap().instructions = vec![0x206f, 0, 0x0010, 0x000e];
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }

    #[test]
    fn superclass_constructor_accepts_parameters_and_preserves_navigation() {
        let (mut c, mut m) = fixture(vec![0x2070, 0, 0x0010, 0x000e], 2, 2, vec!["I".into()], "V");
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Parent;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text, "        super(p0);\n        return;\n");
        assert_eq!(body.links[0].label, "sample.Parent.<init>(I)V");
        assert_eq!(&body.text[body.links[0].start..body.links[0].end], "super");
        // invoke-direct/range has identical constructor semantics.
        m.code.as_mut().unwrap().instructions = vec![0x0276, 0, 0, 0x000e];
        assert_eq!(
            reconstruct("sample.Example", &c, &m).unwrap().text,
            body.text
        );
        c.superclass = Some("Lsample/Unrelated;".into());
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn delegated_constructor_accepts_constant_and_java25_prologue_computation() {
        let (mut c, mut m) = fixture(vec![0x7012, 0x2070, 0, 0x0001, 0x000e], 2, 1, vec![], "V");
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text, "        this(7);\n        return;\n");
        m.code.as_mut().unwrap().instructions =
            vec![0x7012, 0x00d8, 0x0100, 0x2070, 0, 0x0001, 0x000e];
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(
            body.text.find('+').unwrap() < body.text.find("this(").unwrap(),
            "{}",
            body.text
        );
        assert_eq!(body.text.matches("this(").count(), 1);
    }
    #[test]
    fn constructor_prologue_constructs_independent_super_argument() {
        let (mut c, mut m) = fixture(
            vec![0x0022, 0, 0x1070, 0, 0, 0x2070, 1, 0x0001, 0x000e],
            2,
            1,
            vec![],
            "V",
        );
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Ljava/lang/Object;".into(), "Lsample/Parent;".into()],
            strings: vec!["<init>".into()],
            protos: vec![
                ("V".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/Object;".into()]),
            ],
            methods: vec![(0, 0, 0), (1, 1, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(
            body.text.find("new java.lang.Object()").unwrap() < body.text.find("super(").unwrap(),
            "{}",
            body.text
        );
        // The independent allocation must never capture the uninitialized receiver.
        Arc::get_mut(&mut c.symbols).unwrap().methods[0].1 = 1;
        m.code.as_mut().unwrap().instructions =
            vec![0x0022, 0, 0x2070, 0, 0x0010, 0x2070, 1, 0x0001, 0x000e];
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }

    #[test]
    fn constructor_prologue_casts_parameter_but_not_uninitialized_receiver() {
        let (mut c, mut m) = fixture(
            vec![0x011f, 0, 0x2070, 0, 0x0010, 0x000e],
            2,
            2,
            vec!["Ljava/lang/Object;".into()],
            "V",
        );
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Ljava/lang/String;".into(), "Lsample/Parent;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["Ljava/lang/String;".into()])],
            methods: vec![(1, 0, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("java.lang.String"), "{}", body.text);
        m.code.as_mut().unwrap().instructions[0] = 0x001f;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }

    #[test]
    fn constructor_prologue_reads_parameter_field_and_rejects_this_field_read() {
        let (mut c, mut m) = fixture(
            vec![0x2052, 0, 0x2070, 0, 0x0001, 0x000e],
            3,
            2,
            vec!["Lsample/Holder;".into()],
            "V",
        );
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec![
                "Lsample/Parent;".into(),
                "Lsample/Holder;".into(),
                "I".into(),
            ],
            strings: vec!["<init>".into(), "value".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            fields: vec![(1, 2, 1)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(
            body.text.find(".value").unwrap() < body.text.find("super(").unwrap(),
            "{}",
            body.text
        );
        m.code.as_mut().unwrap().instructions[0] = 0x1052;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }

    #[test]
    fn constructor_prologue_allocates_array_and_preserves_element_store_order() {
        let (mut c, mut m) = fixture(
            vec![
                0x1212, 0x2023, 0, 0x0112, 0x4b, 0x0100, 0x2070, 0, 0x0003, 0x000e,
            ],
            4,
            1,
            vec![],
            "V",
        );
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec!["[I".into(), "Lsample/Parent;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["[I".into()])],
            methods: vec![(1, 0, 0)],
            ..Default::default()
        });
        // Store v2 (one) in the array at index v1 (zero).
        m.code.as_mut().unwrap().instructions[4] = 0x024b;
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(
            body.text.find("new int[1]").unwrap() < body.text.find("[0] = 1").unwrap(),
            "{}",
            body.text
        );
        assert!(
            body.text.find("[0] = 1").unwrap() < body.text.find("super(").unwrap(),
            "{}",
            body.text
        );
    }

    #[test]
    fn constructor_prologue_filled_array_rejects_this_escape() {
        let (mut c, mut m) = fixture(
            vec![0x1024, 0, 2, 0x000c, 0x2070, 0, 0x0001, 0x000e],
            3,
            2,
            vec!["Ljava/lang/Object;".into()],
            "V",
        );
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec!["[Ljava/lang/Object;".into(), "Lsample/Parent;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["[Ljava/lang/Object;".into()])],
            methods: vec![(1, 0, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(
            body.text.contains("new java.lang.Object[]"),
            "{}",
            body.text
        );
        m.code.as_mut().unwrap().instructions[2] = 1;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
        m.code.as_mut().unwrap().instructions[0] = 0x0125;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
        m.code.as_mut().unwrap().instructions[2] = 2;
        assert!(reconstruct("sample.Example", &c, &m).is_ok());
    }

    #[test]
    fn constructor_prologue_nested_allocation_preserves_receiver_and_rejects_escape() {
        let (mut c, mut m) = fixture(
            vec![
                0x0022, 0, 0x0122, 1, 0x1070, 0, 1, 0x2070, 1, 0x0010, 0x2070, 2, 0x0002, 0x000e,
            ],
            3,
            1,
            vec![],
            "V",
        );
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec![
                "Lsample/Box;".into(),
                "Ljava/lang/Object;".into(),
                "Lsample/Parent;".into(),
            ],
            strings: vec!["<init>".into()],
            protos: vec![
                ("V".into(), vec![]),
                ("V".into(), vec!["Ljava/lang/Object;".into()]),
                ("V".into(), vec!["Lsample/Box;".into()]),
            ],
            methods: vec![(1, 0, 0), (0, 1, 0), (2, 2, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("new sample.Box("), "{}", body.text);
        assert!(
            body.text.contains("new java.lang.Object()"),
            "{}",
            body.text
        );
        assert_eq!(body.text.matches("super(").count(), 1, "{}", body.text);
        // Feeding this to the inner allocation must fail even when lowering
        // examines the complete nested allocation window.
        Arc::get_mut(&mut c.symbols).unwrap().methods[0].1 = 1;
        m.code.as_mut().unwrap().instructions[4] = 0x2070;
        m.code.as_mut().unwrap().instructions[6] = 0x0021;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }

    #[test]
    fn constructor_prologue_branch_merges_arguments_without_exposing_this() {
        let (mut c, mut m) = fixture(
            vec![0x0238, 4, 0x7012, 0x0228, 0x3012, 0x2070, 0, 0x0001, 0x000e],
            3,
            2,
            vec!["I".into()],
            "V",
        );
        c.superclass = Some("Lsample/Parent;".into());
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Parent;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(
            body.text.find("if (").unwrap() < body.text.find("super(").unwrap(),
            "{}",
            body.text
        );
        assert!(
            body.text.contains("= 3;") && body.text.contains("= 7;"),
            "{}",
            body.text
        );
        assert_eq!(body.text.matches("super(").count(), 1);
        // Even a comparison must not inspect the uninitialized receiver.
        m.code.as_mut().unwrap().instructions[0] = 0x0138;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
        m.code.as_mut().unwrap().instructions[0] = 0x0238;
        m.code.as_mut().unwrap().instructions[2] = 0x7112;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
        // Delegation inside an arm is not equivalent to a prologue merge.
        m.code.as_mut().unwrap().instructions =
            vec![0x0238, 5, 0x2070, 0, 0x0021, 0x2070, 0, 0x0021, 0x000e];
        assert!(reconstruct("sample.Example", &c, &m).is_err());
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.strings.push("observe".into());
        symbols
            .protos
            .push(("V".into(), vec!["Ljava/lang/Object;".into()]));
        symbols.methods.push((0, 1, 1));
        m.code.as_mut().unwrap().instructions =
            vec![0x0238, 5, 0x1071, 1, 1, 0x2070, 0, 0x0021, 0x000e];
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }

    #[test]
    fn constructor_rejects_uninitialized_receiver_as_argument() {
        let (mut c, mut m) = fixture(vec![0x2070, 0, 0x0000, 0x000e], 1, 1, vec![], "V");
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Ljava/lang/Object;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec!["Ljava/lang/Object;".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        m.name = "<init>".into();
        m.access_flags = 1;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn switch_shared_field_effect_and_navigation_are_emitted_once() {
        let (mut c, m) = fixture(
            vec![
                0x012b, 12, 0, 0x0012, 0x0528, 0x1012, 0x0328, 0x2012, 0x0128, 0x0060, 0, 0x000f,
                0x0100, 2, 0, 0, 5, 0, 7, 0,
            ],
            2,
            1,
            vec!["I".into()],
            "I",
        );
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into(), "I".into()],
            strings: vec!["value".into()],
            fields: vec![(0, 1, 0)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("sample.Example.value").count(), 1);
        let links: Vec<_> = body
            .links
            .iter()
            .filter(|link| link.label == "sample.Example.value:I")
            .collect();
        assert_eq!(links.len(), 1);
        let link = links[0];
        assert_eq!(
            body.text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "value"
        );
    }
    #[test]
    fn switch_failed_tests_can_share_a_backward_default_selector() {
        let (c, m) = fixture(
            vec![
                0x012b, 14, 0, 0x0928, 0x0138, 0xffff, 0x1012, 0x0628, 0x0138, 0xfffb, 0x2012,
                0x0228, 0xf012, 0x000f, 0x0100, 2, 0, 0, 4, 0, 8, 0,
            ],
            2,
            1,
            vec!["I".into()],
            "I",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("switch (").count(), 1, "{}", body.text);
        assert!(body.text.contains("case 0:") && body.text.contains("case 1:"));
        assert_eq!(body.text.matches("return ").count(), 1, "{}", body.text);
        assert!(body.text.contains("= -1;"));
    }

    #[test]
    fn switch_payload_is_never_an_executable_target() {
        // A valid packed payload exists, but normal switch fallthrough enters it.
        assert!(Graph::new(&[0x012b, 4, 0, 0, 0x0100, 0, 0, 0]).is_err());
        // Orphaned payloads are rejected as well.
        assert!(Graph::new(&[0x000e, 0, 0x0100, 0, 0, 0]).is_err());
    }
    #[test]
    fn backward_shared_tail_is_acyclic_and_preserves_each_branch_value() {
        // Both arms add 3; the second arm jumps backwards to the shared add.
        // That edge cannot reach its source and must not become a while loop.
        let words = vec![
            0x0138, 6, 0x1012, 0x00d8, 0x0300, 0x0328, 0x2012, 0xfc28, 0x000f,
        ];
        let graph = Graph::new(&words).unwrap();
        assert!(graph.loops.is_empty());
        assert!(graph.acyclic_backwards.contains(&7));
        let (c, m) = fixture(words, 2, 1, vec!["I".into()], "I");
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(!body.text.contains("while"), "{}", body.text);
        assert!(body.text.contains("1 + (3)"), "{}", body.text);
        assert!(body.text.contains("2 + (3)"), "{}", body.text);
        assert_eq!(body.text.matches("return ").count(), 1);
    }
    #[test]
    fn guarded_conditional_loop_keeps_exit_values_separate_from_backedge_values() {
        // if p0<=0 exit; p0--; if p0>0 repeat; p1=99; p1++; exit:return p1.
        let (c, m) = fixture(
            vec![
                0x003d, 10, 0x00d8, 0xff00, 0x003c, 0xfffc, 0x0113, 99, 0x01d8, 0x0101, 0x010f,
            ],
            2,
            2,
            vec!["I".into(), "I".into()],
            "I",
        );
        let graph = Graph::new(&m.code.as_ref().unwrap().instructions).unwrap();
        assert_eq!(graph.loops.len(), 1);
        assert_eq!(graph.loops[0].tail, Some(6));
        assert_eq!(graph.loops[0].exit, 10);
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(body.text.matches("break;").count(), 2, "{}", body.text);
        let tail = body.text.find("99 + (1)").unwrap();
        assert!(body.text[..tail].contains("continue;"), "{}", body.text);
        assert!(body.text.ends_with("        return v2;\n"), "{}", body.text);
        assert_eq!(
            body.text.matches("v2 = ").count(),
            3,
            "entry, guard exit and tail exit: {}",
            body.text
        );
    }

    #[test]
    fn exit_tail_may_change_a_dead_loop_register_type_but_not_a_live_exit_type() {
        // p0 is integral on the backedge and a String only on the exit tail.
        let (mut c, mut m) = fixture(
            vec![0x003d, 8, 0x00d8, 0xff00, 0x003c, 0xfffc, 0x001a, 0, 0x000e],
            1,
            1,
            vec!["I".into()],
            "V",
        );
        Arc::get_mut(&mut c.symbols)
            .unwrap()
            .strings
            .push("exit".into());
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("while (true) {"));
        m.return_type = "I".into();
        m.code.as_mut().unwrap().instructions[8] = 0x000f;
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }

    #[test]
    fn guarded_loop_separates_shared_tail_but_rejects_body_entry_and_tail_backedge() {
        // A branch outside the loop enters its one-time exit tail.
        let entry = [
            0x0138, 8, 0x003d, 10, 0x00d8, 0xff00, 0x003c, 0xfffc, 0x0113, 99, 0x01d8, 0x0101,
            0x010f,
        ];
        let graph = Graph::new(&entry).unwrap();
        assert_eq!(graph.loops[0].tail, None);
        assert_eq!(graph.loops[0].exit, 8);
        let mut interior = entry;
        interior[1] = 4;
        let error = Graph::new(&interior).err().unwrap().to_string();
        assert!(error.contains("interior entry"), "{error}");
        let backedge = [
            0x003d, 10, 0x00d8, 0xff00, 0x003c, 0xfffc, 0x0113, 99, 0x0139, 0xfffa, 0x010f,
        ];
        assert!(Graph::new(&backedge).is_err());
    }

    #[test]
    fn branches_within_owned_exit_tail_do_not_make_it_externally_shared() {
        let words = vec![
            0x003d, 13, 0x00d8, 0xff00, 0x003c, 0xfffc, 0x0138, 4, 0x1112, 0x0228, 0x2112, 0x01d8,
            0x0101, 0x010f,
        ];
        let graph = Graph::new(&words).unwrap();
        assert_eq!(graph.loops[0].tail, Some(6));
        assert_eq!(graph.loops[0].exit, 13);
        let (class, method) = fixture(words, 2, 2, vec!["I".into(), "I".into()], "I");
        reconstruct("sample.Example", &class, &method).unwrap();
    }

    #[test]
    fn while_loop_materializes_carried_values_and_keeps_exit_in_scope() {
        // v0 = 0; while (v0 < p0) { v0++; } return v0.
        let (c, m) = fixture(
            vec![0x0012, 0x1035, 5, 0x00d8, 0x0100, 0xfc28, 0x000f],
            2,
            1,
            vec!["I".into()],
            "I",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("while (true) {"));
        assert!(body.text.contains("if (v0 >= v1) {"));
        assert!(body.text.contains("break;"));
        assert!(body.text.ends_with("        return v0;\n"));
    }
    #[test]
    fn do_loop_tests_before_parallel_carried_assignments() {
        // do { p0--; } while (p0 > 0); return p0.
        let (c, m) = fixture(
            vec![0x00d8, 0xff00, 0x003c, 0xfffe, 0x000f],
            1,
            1,
            vec!["I".into()],
            "I",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        let loop_start = body.text.find("while (true) {").unwrap();
        let slots: Vec<_> = body.text[..loop_start]
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("int ")?
                    .split_once(" = ")
                    .map(|(name, _)| name)
            })
            .collect();
        assert_eq!(slots.len(), 2, "separate header and exit slots");
        let test = body.text.find("if (").unwrap();
        let assignment = |slot: &str| {
            let pattern = format!("{slot} = ");
            let line = body
                .text
                .lines()
                .find(|line| line.trim().starts_with(&pattern))
                .unwrap();
            body.text.find(line).unwrap()
        };
        // Cleanup may inline the one-use boolean snapshot. Its condition still
        // reads the decremented temporary before either slot receives a copy.
        let decremented = body.text[loop_start..test]
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("int ")?
                    .split_once(" = ")
                    .map(|(name, _)| name)
            })
            .unwrap();
        assert!(body.text[test..].starts_with(&format!("if ({decremented} <= 0)")));
        assert!(test < assignment(slots[0]));
        assert!(test < assignment(slots[1]));
        assert!(
            body.text
                .ends_with(&format!("        return {};\n", slots[1]))
        );
    }
    #[test]
    fn loops_reject_interior_entries_and_overlapping_backedges() {
        assert!(Graph::new(&[0x0228, 0, 0x003c, 0xffff, 0x000e]).is_err());
        assert!(Graph::new(&[0, 0, 0x003c, 0xfffe, 0x003c, 0xfffd, 0x000e]).is_err());
        // Same-header backedges are now one loop, not overlapping loops.
        let shared = Graph::new(&[0, 0x003c, 0xffff, 0x003c, 0xfffd, 0x000e]).unwrap();
        assert_eq!(shared.loops.len(), 1);
        assert_eq!(shared.loops[0].latch, 3);
    }
    #[test]
    fn register_overwrites_do_not_rewrite_previous_values() {
        // v0=p0+p1; v1=v0; v0=5; return v1.
        let (c, m) = fixture(
            vec![0x0090, 0x0302, 0x0101, 0x5012, 0x010f],
            4,
            2,
            vec!["I".into(), "I".into()],
            "I",
        );
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(
            body.text,
            "        int v0 = p0 + (p1);\n        return v0;\n"
        );
    }
    #[test]
    fn boolean_and_null_constants_have_contextual_types() {
        let (c, m) = fixture(vec![0x1012, 0x000f], 1, 0, vec![], "Z");
        assert!(
            reconstruct("sample.Example", &c, &m)
                .unwrap()
                .text
                .contains("return true;")
        );
        let (c, m) = fixture(vec![0x0012, 0x0011], 1, 0, vec![], "Ljava/lang/Object;");
        assert!(
            reconstruct("sample.Example", &c, &m)
                .unwrap()
                .text
                .contains("return null;")
        );
    }
    #[test]
    fn invalid_return_kind_and_branch_are_rejected() {
        let (c, m) = fixture(vec![0x1012, 0x0011], 1, 0, vec![], "I");
        assert!(reconstruct("sample.Example", &c, &m).is_err());
        let (c, m) = fixture(vec![0x0028], 1, 0, vec![], "V");
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn adjacent_allocation_constructor_preserves_effects_and_reference() {
        let (mut c, m) = fixture(
            vec![0x0022, 0, 0x1070, 0, 0, 0x0011],
            1,
            0,
            vec![],
            "Lsample/Example;",
        );
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into()],
            strings: vec!["<init>".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert_eq!(
            body.text,
            "        sample.Example v0 = new sample.Example();\n        return v0;\n"
        );
        assert_eq!(body.links.len(), 2);
        assert_eq!(body.links[0].label, "sample.Example.<init>()V");
        let link = &body.links[0];
        assert_eq!(
            body.text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "sample.Example"
        );
        let mut m = m;
        m.code.as_mut().unwrap().instructions.insert(2, 0x001a);
        assert!(reconstruct("sample.Example", &c, &m).is_err());
    }
    #[test]
    fn null_receiver_is_explicitly_typed_before_field_access() {
        let (mut c, m) = fixture(vec![0x0012, 0x0052, 0, 0x000f], 1, 0, vec![], "I");
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Example;".into(), "I".into()],
            strings: vec!["value".into()],
            fields: vec![(0, 1, 0)],
            ..Default::default()
        });
        let body = reconstruct("sample.Example", &c, &m).unwrap();
        assert!(body.text.contains("((sample.Example) null).value"));
        let link = &body.links[0];
        assert_eq!(
            body.text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "value"
        );
    }
    #[test]
    fn expression_references_have_explicit_unicode_offsets() {
        let mut out = Output::default();
        out.local("I", "é.a(\".a\")", &[(2, 1, "example.Owner.a()I".into())])
            .unwrap();
        assert_eq!(out.links.len(), 1);
        let link = &out.links[0];
        assert_eq!(
            out.text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "a"
        );
    }
}

#[cfg(test)]
mod readability_conditions {
    use super::*;
    #[test]
    fn boolean_zero_tests_simplify_but_integer_tests_do_not() {
        let boolean = Value {
            text: "flag".into(),
            ty: "Z".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
        let regs = [Some(boolean)];
        assert_eq!(condition(0x39, 0, &regs).unwrap(), "flag");
        assert_eq!(condition(0x38, 0, &regs).unwrap(), "!(flag)");
        let integer = Value {
            text: "number".into(),
            ty: "I".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
        assert_eq!(condition(0x39, 0, &[Some(integer)]).unwrap(), "number != 0");
    }
    #[test]
    fn null_checks_need_no_object_cast_but_unrelated_reference_equality_does() {
        let value = |text: &str, ty: &str| {
            Some(Value {
                text: text.into(),
                ty: ty.into(),
                literal: None,
                wide_literal: None,
                raw_bits32: false,
            })
        };
        let regs = [
            value("left", "Lsample/Left;"),
            value("right", "Lsample/Right;"),
        ];
        assert_eq!(condition(0x38, 0, &regs).unwrap(), "left == null");
        assert_eq!(
            condition(0x32, 0x10, &regs).unwrap(),
            "((java.lang.Object) left) == ((java.lang.Object) right)"
        );
    }
}

#[cfg(test)]
mod boolean_numeric_arguments {
    use super::*;
    #[test]
    fn boolean_zero_one_values_reify_for_all_narrow_integral_uses() {
        let value = Value {
            text: "flag".into(),
            ty: "Z".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
        for (descriptor, expected) in [
            ("I", "(flag ? 1 : 0)"),
            ("B", "(byte) (flag ? 1 : 0)"),
            ("S", "(short) (flag ? 1 : 0)"),
            ("C", "(char) (flag ? 1 : 0)"),
        ] {
            assert_eq!(argument(&value, descriptor).unwrap(), expected);
        }
        assert_eq!(argument(&value, "Z").unwrap(), "flag");
        for target in ["F", "J", "D", "Ljava/lang/Object;"] {
            assert!(argument(&value, target).is_err());
        }
    }
    #[test]
    fn arbitrary_integer_values_are_not_narrowed_implicitly() {
        let value = Value {
            text: "number".into(),
            ty: "I".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        };
        for target in ["B", "S", "C", "Z"] {
            assert!(argument(&value, target).is_err());
        }
    }
}

#[cfg(test)]
mod boolean_register_arguments {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use std::sync::Arc;

    fn code(words: Vec<u16>, registers: u16, ins: u16) -> DexCode {
        DexCode {
            registers,
            ins,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }
    }

    fn integer_value() -> Value {
        Value {
            text: "slot".into(),
            ty: "I".into(),
            literal: None,
            wide_literal: None,
            raw_bits32: false,
        }
    }

    #[test]
    fn all_narrow_constant_encodings_prove_boolean_register() {
        for words in [
            vec![0x1012, 0x000e],
            vec![0x0013, 1, 0x000e],
            vec![0x0014, 1, 0, 0x000e],
            vec![0x0015, 0, 0x000e],
        ] {
            let code = code(words, 1, 0);
            let graph = Graph::new(&code.instructions).unwrap();
            assert_eq!(
                argument_from_register(&integer_value(), "Z", 0, &code, &graph).unwrap(),
                "(slot != 0)"
            );
        }
    }

    #[test]
    fn arbitrary_parameter_and_later_backward_write_are_not_boolean() {
        let parameter = code(vec![0x000e], 1, 1);
        let graph = Graph::new(&parameter.instructions).unwrap();
        assert!(argument_from_register(&integer_value(), "Z", 0, &parameter, &graph).is_err());

        // The write lies after the sink in code order but can run before its
        // next execution through the backward goto.
        let looped = code(
            vec![0x0012, 0x0138, 7, 0x1071, 0, 0, 0x2012, 0xfa28, 0x000e],
            2,
            0,
        );
        let graph = Graph::new(&looped.instructions).unwrap();
        assert_eq!(graph.targets[7], Some(1));
        assert!(argument_from_register(&integer_value(), "Z", 0, &looped, &graph).is_err());
    }

    #[test]
    fn selected_constant_proof_uses_decoded_identity_and_rejects_poison() {
        let class = DexClass {
            symbols: Arc::new(DexSymbols::default()),
            descriptor: "Lsample/ConstProof;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        };
        let method = DexMethod {
            declaring_type: class.descriptor.clone(),
            name: "test".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(code(vec![0x1012, 0x000e], 1, 0)),
        };
        let source = method.code.as_ref().unwrap();
        let mut graph = Graph::straight_line(&class, &method).unwrap().unwrap();
        let selected = &mut graph.front_end.as_mut().unwrap().ir.instructions[0];
        selected.literal = Some(2);
        assert!(!constant_register_domain(source, &graph, 0, ConstantDomain::Boolean).unwrap());
        graph.front_end.as_mut().unwrap().ir.instructions[0].literal = None;
        assert!(constant_register_domain(source, &graph, 0, ConstantDomain::Boolean).is_err());
    }

    #[test]
    fn boolean_return_proof_rejects_selected_call_and_cfg_poison() {
        let class = DexClass {
            symbols: Arc::new(DexSymbols {
                strings: vec!["tick".into()],
                types: vec!["Lsample/ReturnProof;".into()],
                protos: vec![("Z".into(), vec![])],
                methods: vec![(0, 0, 0)],
                ..Default::default()
            }),
            descriptor: "Lsample/ReturnProof;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        };
        let method = DexMethod {
            declaring_type: class.descriptor.clone(),
            name: "test".into(),
            return_type: "Z".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(code(vec![0x0012, 0x0071, 0, 0, 0x000a, 0x000f], 1, 0)),
        };
        let clean = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert!(proven_boolean_return(&class, &method, &clean, 5, 0).unwrap());

        let mut poisoned_call = Graph::straight_line(&class, &method).unwrap().unwrap();
        poisoned_call.front_end.as_mut().unwrap().bound.calls[0].return_type = "I".into();
        assert!(proven_boolean_return(&class, &method, &poisoned_call, 5, 0).is_err());

        let mut poisoned_cfg = Graph::straight_line(&class, &method).unwrap().unwrap();
        poisoned_cfg.shared_cfg = Some(
            crate::native_cfg::ControlFlowGraph::from_decoded(
                &poisoned_cfg.front_end.as_ref().unwrap().ir,
                method.code.as_ref().unwrap().instructions.len(),
            )
            .unwrap(),
        );
        poisoned_cfg.shared_cfg.as_mut().unwrap().blocks[0].start += 1;
        assert!(proven_boolean_return(&class, &method, &poisoned_cfg, 5, 0).is_err());

        let literal_method = DexMethod {
            code: Some(code(vec![0x2012, 0x000f], 1, 0)),
            ..method
        };
        let mut poisoned_literal = Graph::straight_line(&class, &literal_method)
            .unwrap()
            .unwrap();
        poisoned_literal.front_end.as_mut().unwrap().ir.instructions[0].literal = Some(1);
        assert!(proven_boolean_return(&class, &literal_method, &poisoned_literal, 1, 0).is_err());

        let moved_method = DexMethod {
            code: Some(code(vec![0x0012, 0x0101, 0x010f], 2, 0)),
            ..literal_method
        };
        let mut poisoned_move = Graph::straight_line(&class, &moved_method)
            .unwrap()
            .unwrap();
        poisoned_move.front_end.as_mut().unwrap().ir.instructions[1].reads[0].register = 1;
        assert!(proven_boolean_return(&class, &moved_method, &poisoned_move, 2, 1).is_err());
    }

    #[test]
    fn boolean_or_loop_rejects_selected_poison_and_unanchored_dependency_cycle() {
        let class = DexClass {
            symbols: Arc::new(DexSymbols {
                strings: vec!["tick".into()],
                types: vec!["Lsample/ReturnOrProof;".into()],
                protos: vec![("Z".into(), vec![])],
                methods: vec![(0, 0, 0)],
                ..Default::default()
            }),
            descriptor: "Lsample/ReturnOrProof;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        };
        let method = DexMethod {
            declaring_type: class.descriptor.clone(),
            name: "test".into(),
            return_type: "Z".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(code(
                vec![
                    0x0012, 0x0238, 10, 0x0071, 0, 0, 0x010a, 0x10b6, 0x02d8, 0xff02, 0xf728,
                    0x000f,
                ],
                3,
                1,
            )),
        };
        let mut selected = Graph::straight_line(&class, &method).unwrap().unwrap();
        assert!(proven_boolean_return(&class, &method, &selected, 11, 0).unwrap());
        selected.front_end.as_mut().unwrap().ir.instructions[0].literal = Some(2);
        assert!(proven_boolean_return(&class, &method, &selected, 11, 0).is_err());

        let code = method.code.as_ref().unwrap();
        let ir = crate::native_ir::DecodedMethod::decode(code).unwrap();
        let cfg = crate::native_cfg::ControlFlowGraph::build(code).unwrap();
        let mut ssa = crate::native_ssa::SsaMethod::build(code, &ir, &cfg).unwrap();
        let bound = crate::native_calls::BoundCalls::bind(code, &ir, &class.symbols).unwrap();
        let calls = crate::native_call_values::SsaCalls::bind(&bound, &ssa).unwrap();
        assert!(boolean_return_or_proven(&method, &ir, &ssa, &calls, 11, 0));

        // The return phi still has a valid zero anchor. Its loop-back value
        // now depends on a separate closed phi cycle with no finite value.
        // Joining Bottom away at the return phi would accept it unsoundly.
        let header = ssa
            .phis
            .iter()
            .find(|phi| phi.register == 0)
            .unwrap()
            .clone();
        let phantom = ssa.definitions.len();
        ssa.definitions.push(crate::native_ssa::Definition {
            register: 1,
            kind: crate::native_ssa::DefinitionKind::Phi {
                block: header.block,
            },
        });
        ssa.phis.push(crate::native_ssa::Phi {
            block: header.block,
            register: 1,
            result: phantom,
            incoming: header
                .incoming
                .iter()
                .map(|(pred, _)| (*pred, phantom))
                .collect(),
        });
        ssa.instructions
            .iter_mut()
            .find(|instruction| instruction.pc == 7)
            .unwrap()
            .reads[1]
            .words[0] = phantom;
        assert!(!boolean_return_or_proven(&method, &ir, &ssa, &calls, 11, 0));
    }
}

#[cfg(test)]
mod terminal_local_tests {
    use super::*;

    #[test]
    fn adjacent_return_uses_recorded_expression_and_unicode_spans() {
        let mut out = Output::default();
        out.line("// λ", &[]);
        let value = out
            .local("I", "Source.read()", &[(7, 4, "Source.read()I".into())])
            .unwrap();
        out.return_value(&value, &value.text, "I");
        assert!(out.text.contains("return Source.read();"));
        assert!(!out.text.contains("int v0"));
        let link = &out.links[0];
        assert_eq!(
            out.text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "read"
        );
    }

    #[test]
    fn statements_blocks_conversions_and_appends_prevent_terminal_shrinking() {
        for barrier in 0..4 {
            let mut out = Output::default();
            let value = out.local("I", "Source.read()", &[]).unwrap();
            match barrier {
                0 => out.line("Source.effect();", &[]),
                1 => out.indent += 1,
                2 => out.append(Output::default()),
                _ => (),
            }
            let converted = if barrier == 3 {
                "((long) v0)"
            } else {
                &value.text
            };
            out.return_value(&value, converted, if barrier == 3 { "J" } else { "I" });
            assert!(out.text.contains("int v0 = Source.read();"), "{}", out.text);
        }
    }
}

#[cfg(test)]
mod call_shrink_proof_tests {
    use super::*;
    use crate::native_dex::{self, DexSymbols};
    use std::sync::Arc;

    #[test]
    fn selected_call_result_rejects_protection_and_branch_entry() {
        let mut class = native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.descriptor = "Lsample/Proof;".into();
        class
            .methods
            .retain(|method| method.name.as_ref() == "answer");
        class.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Source;".into(), "Lsample/Sink;".into()],
            strings: vec!["make".into(), "accept".into()],
            protos: vec![
                ("Ljava/lang/String;".into(), vec![]),
                (
                    "V".into(),
                    vec!["Ljava/lang/Object;".into(), "Ljava/lang/String;".into()],
                ),
            ],
            methods: vec![(0, 0, 0), (1, 1, 1)],
            ..Default::default()
        });
        let method = &mut class.methods[0];
        method.declaring_type = class.descriptor.clone();
        method.name = "run".into();
        method.return_type = "V".into();
        method.parameters = vec!["Ljava/lang/Object;".into()];
        method.access_flags = 9;
        let code = method.code.as_mut().unwrap();
        code.registers = 2;
        code.ins = 1;
        code.outs = 2;
        code.tries = 0;
        code.try_regions.clear();
        code.instructions = vec![0x0071, 0, 0, 0x000c, 0x2071, 1, 0x0001, 0x000e];
        let mut graph = Graph::empty(code.instructions.len());
        graph.front_end =
            Some(crate::native_method::MethodFrontEnd::build(&class, &class.methods[0]).unwrap());
        assert!(graph.single_use_call_result(&class.methods[0], 4, 0, "Ljava/lang/String;"));
        class.methods[0].code.as_mut().unwrap().tries = 1;
        assert!(!graph.single_use_call_result(&class.methods[0], 4, 0, "Ljava/lang/String;"));
        class.methods[0].code.as_mut().unwrap().tries = 0;
        class.methods[0].code.as_mut().unwrap().instructions[7] = 0xfd28;
        class.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .push(0x000e);
        let mut branch = Graph::empty(class.methods[0].code.as_ref().unwrap().instructions.len());
        branch.front_end =
            Some(crate::native_method::MethodFrontEnd::build(&class, &class.methods[0]).unwrap());
        assert!(!branch.single_use_call_result(&class.methods[0], 4, 0, "Ljava/lang/String;"));
    }
}
