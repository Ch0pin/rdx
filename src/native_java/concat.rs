//! A single-use platform StringBuilder chain can be represented as Java string
//! concatenation. This is a DEX/SSA proof, not a rewrite of emitted Java text.
use crate::{
    native_calls::{BoundCall, BoundCalls, CallKind, CallTarget},
    native_cfg::{ControlFlowGraph, EdgeKind, instruction_width},
    native_dex::{DexClass, DexCode, DexMethod},
    native_ir::{Instruction, PoolKind},
    native_method::MethodFrontEnd,
    native_ssa::{DefinitionKind, SsaMethod, ValueId},
};
use std::sync::Arc;

const BUILDER: &str = "Ljava/lang/StringBuilder;";
const STRING: &str = "Ljava/lang/String;";
const OBJECT: &str = "Ljava/lang/Object;";
const MAX_WORDS: usize = 4096;
const MAX_REGISTERS: u16 = 128;
const MAX_APPENDS: usize = 16;
/// Instructions tolerated between the allocation and its constructor call.
const MAX_PREFIX_GAP: usize = 8;
/// `append` overloads whose conversion is Java string conversion of the same
/// operand. The CharSequence and char[] overloads read their operand differently.
const APPEND_OPERANDS: [&str; 8] = [STRING, OBJECT, "I", "J", "Z", "C", "F", "D"];

/// One `append` call of a proved chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Append {
    pub pc: usize,
    /// Register holding the appended operand.
    pub argument: usize,
    /// Parameter descriptor of the selected overload.
    pub descriptor: Arc<str>,
    /// First instruction after the call and any chained move-result.
    pub next: usize,
    /// Chained form: the move-result and the register receiving the builder.
    chained: Option<(usize, u16)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    pub start: usize,
    pub init: usize,
    /// Pool string passed to `StringBuilder(String)`.
    pub prefix: String,
    pub appends: Vec<Append>,
    pub to_string: usize,
    /// Register receiving the `toString` result.
    pub result: usize,
    /// First instruction after the chain.
    pub end: usize,
}

/// Rendering state of a chain whose constructor, appends and `toString` become
/// one concatenation at `end`. Intermediate instructions render normally.
pub(super) struct Progress {
    pub plan: Plan,
    pub parts: Vec<String>,
    pub appended: usize,
}

impl Progress {
    pub fn new(plan: Plan) -> Self {
        Self {
            plan,
            parts: Vec::new(),
            appended: 0,
        }
    }

    /// The next chain instruction the renderer must reach.
    pub fn expected(&self) -> usize {
        if self.parts.is_empty() {
            self.plan.init
        } else {
            self.plan
                .appends
                .get(self.appended)
                .map_or(self.plan.to_string, |append| append.pc)
        }
    }
}

/// Wraps an operand so `+` binds as intended. Literals, names, member chains
/// and one trailing call or index stay bare.
pub(super) fn operand(text: &str) -> String {
    if primary(text) {
        text.to_string()
    } else {
        format!("({text})")
    }
}

fn primary(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some(&first) = bytes.first() else {
        return false;
    };
    match first {
        b'"' | b'\'' => return quoted(text, first),
        b'(' => return balanced(text),
        _ => {}
    }
    let body = text.strip_prefix('-').unwrap_or(text);
    let Some(&lead) = body.as_bytes().first() else {
        return false;
    };
    if lead.is_ascii_digit() {
        return numeric(body);
    }
    if first == b'-' || !(lead.is_ascii_alphabetic() || lead == b'_' || lead == b'$') {
        return false;
    }
    let name_end = bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'.')))
        .unwrap_or(bytes.len());
    let rest = &text[name_end..];
    rest.is_empty() || ((rest.starts_with('(') || rest.starts_with('[')) && balanced(rest))
}

fn quoted(text: &str, quote: u8) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 2 || bytes[bytes.len() - 1] != quote {
        return false;
    }
    let mut escaped = false;
    for &byte in &bytes[1..bytes.len() - 1] {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == quote {
            return false;
        }
    }
    !escaped
}

/// The whole text is one bracket group: depth returns to zero only at the end.
fn balanced(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut literal: Option<u8> = None;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate() {
        if let Some(quote) = literal {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == quote {
                literal = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => literal = Some(byte),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1).unwrap_or(usize::MAX);
                if depth == 0 && index + 1 != bytes.len() {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0 && literal.is_none() && !bytes.is_empty()
}

fn numeric(body: &str) -> bool {
    let hex = body.starts_with("0x") || body.starts_with("0X");
    let bytes = body.as_bytes();
    bytes.iter().enumerate().all(|(index, &byte)| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'.' | b'_')
            || (!hex
                && matches!(byte, b'+' | b'-')
                && index > 0
                && matches!(bytes[index - 1], b'e' | b'E'))
    })
}

fn call(calls: &BoundCalls, pc: usize) -> Option<&BoundCall> {
    calls.calls.iter().find(|call| call.pc == pc)
}

fn single_write(instruction: &Instruction) -> Option<u16> {
    let [write] = instruction.writes.as_slice() else {
        return None;
    };
    (write.kind.word_count() == 1).then_some(write.register)
}

/// The parameter descriptors of a bound StringBuilder call with the exact
/// kind, name, return type and receiver register, verified against the
/// bound argument descriptors.
fn builder_call<'a>(
    class: &'a DexClass,
    call: &BoundCall,
    kind: CallKind,
    name: &str,
    ret: &str,
    receiver: u16,
) -> Option<&'a [Arc<str>]> {
    let CallTarget::Method {
        declaring_type,
        name: actual_name,
        prototype_index,
        ..
    } = &call.target
    else {
        return None;
    };
    if call.kind != kind
        || declaring_type.as_ref() != BUILDER
        || actual_name.as_ref() != name
        || call.return_type.as_ref() != ret
        || call
            .receiver
            .as_ref()
            .is_none_or(|actual| actual.register != receiver)
    {
        return None;
    }
    let (actual_ret, parameters) = class.symbols.protos.get(usize::from(*prototype_index))?;
    (actual_ret.as_ref() == ret
        && parameters.len() == call.arguments.len()
        && parameters
            .iter()
            .zip(&call.arguments)
            .all(|(parameter, argument)| parameter.as_ref() == argument.descriptor.as_ref()))
    .then_some(parameters.as_slice())
}

fn pool_signature_matches(
    class: &DexClass,
    index: u16,
    name: &str,
    parameters: &[&str],
    result: &str,
) -> bool {
    let symbols = &class.symbols;
    let Some(&(owner, prototype, method_name)) = symbols.methods.get(usize::from(index)) else {
        return false;
    };
    symbols
        .types
        .get(usize::from(owner))
        .is_some_and(|ty| ty.as_ref() == BUILDER)
        && symbols
            .strings
            .get(method_name as usize)
            .is_some_and(|value| value.as_str() == name)
        && symbols
            .protos
            .get(usize::from(prototype))
            .is_some_and(|(ret, args)| {
                ret.as_ref() == result
                    && args.len() == parameters.len()
                    && args
                        .iter()
                        .zip(parameters)
                        .all(|(arg, expected)| arg.as_ref() == *expected)
            })
}

/// Cheap raw-word filter: a StringBuilder allocation followed within a few
/// straight-line instructions by a `StringBuilder(String)` constructor call.
/// Returns that constructor's offset.
fn prefilter(class: &DexClass, code: &DexCode, start: usize) -> Option<usize> {
    if code.instructions.len() > MAX_WORDS
        || code.registers > MAX_REGISTERS
        || usize::from(code.tries) != code.try_regions.len()
    {
        return None;
    }
    let words = &code.instructions;
    if *words.get(start)? as u8 != 0x22
        || class
            .symbols
            .types
            .get(usize::from(*words.get(start + 1)?))?
            .as_ref()
            != BUILDER
    {
        return None;
    }
    let mut pc = start + 2;
    for _ in 0..=MAX_PREFIX_GAP {
        let op = *words.get(pc)? as u8;
        if matches!(op, 0x70 | 0x76)
            && pool_signature_matches(class, *words.get(pc + 1)?, "<init>", &[STRING], "V")
        {
            return Some(pc);
        }
        if matches!(
            op,
            0x0d..=0x11 | 0x1d | 0x1e | 0x22 | 0x27..=0x2c | 0x32..=0x3d
        ) {
            return None;
        }
        pc += instruction_width(words, pc).ok()?.0;
    }
    None
}

fn protection(code: &DexCode, pc: usize) -> Vec<bool> {
    code.try_regions
        .iter()
        .map(|region| (region.start as usize..region.end as usize).contains(&pc))
        .collect()
}

pub(super) fn plan_at(class: &DexClass, method: &DexMethod, start: usize) -> Option<Plan> {
    let code = method.code.as_ref()?;
    // The raw filter prevents decoding, CFG and SSA work for ordinary
    // allocations. The selected chain is then proved by canonical identities.
    let init = prefilter(class, code, start)?;
    let hierarchy = class.symbols.hierarchy.get()?;
    if ![BUILDER, STRING, OBJECT]
        .into_iter()
        .all(|ty| hierarchy.unshadowed_sdk_type(ty))
    {
        return None;
    }
    let front = MethodFrontEnd::build(class, method).ok()?;
    let ir = front.ir.instructions.as_slice();
    let at = |pc: usize| ir.binary_search_by_key(&pc, |i| i.pc).ok();
    let first = at(start)?;
    let builder = single_write(&ir[first])?;
    let init_call = call(&front.bound, init)?;
    let init_parameters = builder_call(class, init_call, CallKind::Direct, "<init>", "V", builder)?;
    if init_call.result.is_some()
        || init_parameters.len() != 1
        || init_parameters[0].as_ref() != STRING
    {
        return None;
    }
    let prefix_register = init_call.arguments[0].register;

    // Walk the chain in DEX order. Before the first append any straight-line
    // instruction may intervene; afterwards only moves, constants and field
    // reads, so no effect is reordered relative to the appended operands'
    // conversions.
    let mut appends: Vec<Append> = Vec::new();
    let mut interior = Vec::new();
    let mut open_allocation: Option<u16> = None;
    let mut current = builder;
    let mut index = at(init)? + 1;
    let (to_string, result, end, end_index) = loop {
        let instruction = ir.get(index)?;
        let bound = call(&front.bound, instruction.pc);
        if let Some(bound) = bound
            && let CallTarget::Method {
                declaring_type,
                name,
                ..
            } = &bound.target
            && declaring_type.as_ref() == BUILDER
            && bound
                .receiver
                .as_ref()
                .is_some_and(|receiver| receiver.register == current)
        {
            if open_allocation.is_some() {
                return None;
            }
            let moved = ir.get(index + 1).filter(|next| {
                next.pc == instruction.pc + instruction.width && matches!(next.opcode, 0x0a..=0x0c)
            });
            match name.as_ref() {
                "append" => {
                    let parameters =
                        builder_call(class, bound, CallKind::Virtual, "append", BUILDER, current)?;
                    let [descriptor] = parameters else {
                        return None;
                    };
                    if !APPEND_OPERANDS.contains(&descriptor.as_ref())
                        || appends.len() == MAX_APPENDS
                    {
                        return None;
                    }
                    let (next, chained) = match (&bound.result, moved) {
                        (Some(result), Some(move_result))
                            if result.move_pc == move_result.pc && move_result.opcode == 0x0c =>
                        {
                            current = result.register.register;
                            (
                                move_result.pc + move_result.width,
                                Some((move_result.pc, current)),
                            )
                        }
                        (None, None) => (instruction.pc + instruction.width, None),
                        _ => return None,
                    };
                    appends.push(Append {
                        pc: instruction.pc,
                        argument: usize::from(bound.arguments[0].register),
                        descriptor: descriptor.clone(),
                        next,
                        chained,
                    });
                    index += if chained.is_some() { 2 } else { 1 };
                }
                "toString" => {
                    if appends.is_empty()
                        || !builder_call(
                            class,
                            bound,
                            CallKind::Virtual,
                            "toString",
                            STRING,
                            current,
                        )?
                        .is_empty()
                    {
                        return None;
                    }
                    let (Some(result), Some(move_result)) = (&bound.result, moved) else {
                        return None;
                    };
                    if result.move_pc != move_result.pc || move_result.opcode != 0x0c {
                        return None;
                    }
                    break (
                        instruction.pc,
                        usize::from(result.register.register),
                        move_result.pc + move_result.width,
                        index + 2,
                    );
                }
                _ => return None,
            }
            continue;
        }
        let op = instruction.opcode;
        if appends.is_empty() {
            if matches!(op, 0x0d..=0x11 | 0x1d | 0x1e | 0x27..=0x2c | 0x32..=0x3d) {
                return None;
            }
            match op {
                0x22 => {
                    if open_allocation.is_some() {
                        return None;
                    }
                    open_allocation = Some(single_write(instruction)?);
                }
                0x70 | 0x76 => {
                    if let Some(bound) = bound
                        && let CallTarget::Method { name, .. } = &bound.target
                        && name.as_ref() == "<init>"
                        && bound.receiver.as_ref().map(|receiver| receiver.register)
                            == open_allocation
                    {
                        open_allocation = None;
                    }
                }
                _ => {}
            }
        } else if matches!(op, 0x01..=0x09 | 0x12..=0x1b | 0x52..=0x58 | 0x60..=0x66) {
            interior.push(index);
        } else {
            return None;
        }
        index += 1;
    };

    let chain = &ir[first..end_index];
    let membership = protection(code, start);
    if chain
        .iter()
        .any(|instruction| protection(code, instruction.pc) != membership)
        || ir.iter().any(|instruction| {
            instruction
                .branch_target
                .is_some_and(|target| start < target && target < end)
                || instruction
                    .payload_target
                    .is_some_and(|target| start < target && target < end)
        })
    {
        return None;
    }

    // The chain is one straight line: consecutive instructions share a block
    // or are joined by the only normal edge into the later block.
    let cfg = ControlFlowGraph::build(code).ok()?;
    for pair in chain.windows(2) {
        let (from, to) = (
            *cfg.block_at.get(&pair[0].pc)?,
            *cfg.block_at.get(&pair[1].pc)?,
        );
        if from == to {
            continue;
        }
        let normal: Vec<usize> = cfg.blocks[from]
            .successors
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Normal)
            .map(|edge| edge.target)
            .collect();
        let incoming: Vec<(usize, EdgeKind)> = cfg
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(source, block)| {
                block
                    .successors
                    .iter()
                    .filter_map(move |edge| (edge.target == to).then_some((source, edge.kind)))
            })
            .collect();
        if normal != [to] || incoming != [(from, EdgeKind::Normal)] {
            return None;
        }
    }

    // Every builder value is observed by its chain reads alone, so neither the
    // intermediates nor any handler or merge can see the builder.
    let ssa = SsaMethod::build(code, &front.ir, &cfg).ok()?;
    let exclusive = |value: ValueId, reads: &[(usize, u16)]| {
        ssa.in_no_phi(value)
            && ssa.only_uses(value, reads)
            && reads
                .iter()
                .all(|&(pc, register)| ssa.value_id(pc, register, true) == Some(value))
    };
    let mut segment = ssa.value_id(start, builder, false)?;
    let mut receiver = builder;
    let mut reads = vec![(init, builder)];
    for append in &appends {
        reads.push((append.pc, receiver));
        if let Some((move_pc, register)) = append.chained {
            if !exclusive(segment, &reads) {
                return None;
            }
            segment = ssa.value_id(move_pc, register, false)?;
            receiver = register;
            reads.clear();
        }
    }
    reads.push((to_string, receiver));
    if !exclusive(segment, &reads) {
        return None;
    }

    // The constructor argument is one pool string.
    let prefix_id = ssa.value_id(init, prefix_register, true)?;
    let DefinitionKind::Instruction { pc: prefix_pc, .. } = ssa.definitions.get(prefix_id)?.kind
    else {
        return None;
    };
    let constant = &ir[at(prefix_pc)?];
    let reference = constant.reference.filter(|reference| {
        reference.kind == PoolKind::String && matches!(constant.opcode, 0x1a | 0x1b)
    })?;
    let prefix = class
        .symbols
        .strings
        .get(usize::try_from(reference.index).ok()?)?
        .clone();
    // Empty builder results may reuse the platform's empty String singleton.
    // Keep the original builder calls unless the prefix guarantees a nonempty
    // result, whose fresh identity the emitted String constructor preserves.
    if prefix.is_empty() {
        return None;
    }

    // Appended operands are defined values, and values written between appends
    // feed later appends of this chain only.
    let ssa_instruction = |pc: usize| ssa.instructions.iter().find(|insn| insn.pc == pc);
    for append in &appends {
        let operand = ssa_instruction(append.pc)?
            .reads
            .iter()
            .find(|operand| usize::from(operand.register) == append.argument)?;
        if operand.words.is_empty()
            || operand.words.iter().any(|&word| {
                ssa.definitions
                    .get(word)
                    .is_none_or(|definition| matches!(definition.kind, DefinitionKind::Undefined))
            })
        {
            return None;
        }
    }
    for &index in &interior {
        let instruction = &ir[index];
        for write in &ssa_instruction(instruction.pc)?.writes {
            for &value in &write.words {
                if !ssa.in_no_phi(value)
                    || !ssa.uses(value).iter().all(|&(pc, register)| {
                        pc > instruction.pc
                            && appends.iter().any(|append| {
                                append.pc == pc && append.argument == usize::from(register)
                            })
                    })
                {
                    return None;
                }
            }
        }
    }

    Some(Plan {
        start,
        init,
        prefix,
        appends,
        to_string,
        result,
        end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};

    fn fixture(words: Vec<u16>) -> DexClass {
        DexClass {
            descriptor: "Lsample/Test;".into(),
            superclass: Some(OBJECT.into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            symbols: Arc::new(DexSymbols {
                strings: vec![
                    "tag:".into(),
                    "<init>".into(),
                    "append".into(),
                    "toString".into(),
                ],
                types: vec![BUILDER.into()],
                protos: vec![
                    ("V".into(), vec![STRING.into()]),
                    (BUILDER.into(), vec![OBJECT.into()]),
                    (STRING.into(), vec![]),
                ],
                methods: vec![(0, 0, 1), (0, 1, 2), (0, 2, 3)],
                ..Default::default()
            }),
            methods: vec![DexMethod {
                declaring_type: "Lsample/Test;".into(),
                name: "text".into(),
                return_type: STRING.into(),
                parameters: vec![OBJECT.into()],
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 4,
                    ins: 1,
                    outs: 2,
                    tries: 0,
                    try_regions: vec![],
                    instructions: words,
                    offset: 0,
                }),
            }],
        }
    }

    fn chain() -> Vec<u16> {
        vec![
            0x0022, 0, 0x011a, 0, 0x2070, 0, 0x0010, 0x206e, 1, 0x0030, 0x106e, 2, 0, 0x020c,
            0x0211,
        ]
    }

    fn attach_hierarchy(class: &DexClass, shadow: Option<&DexClass>) {
        let hierarchy = if let Some(shadow) = shadow {
            crate::native_hierarchy::TypeHierarchy::from_classes([class, shadow]).unwrap()
        } else {
            crate::native_hierarchy::TypeHierarchy::from_classes([class]).unwrap()
        };
        class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    }

    #[test]
    fn platform_contract_types_are_present() {
        let hierarchy = crate::native_hierarchy::TypeHierarchy::from_classes([]).unwrap();
        for ty in [BUILDER, STRING, OBJECT] {
            assert!(hierarchy.unshadowed_sdk_type(ty), "missing SDK {ty}");
        }
    }

    #[test]
    fn single_append_chain_is_planned_exactly() {
        let class = fixture(chain());
        attach_hierarchy(&class, None);
        let plan = plan_at(&class, &class.methods[0], 0).unwrap();
        assert_eq!(
            (plan.start, plan.init, plan.to_string, plan.result, plan.end),
            (0, 4, 10, 2, 14)
        );
        assert_eq!(plan.prefix, "tag:");
        assert_eq!(plan.appends.len(), 1);
        assert_eq!(
            (
                plan.appends[0].pc,
                plan.appends[0].argument,
                plan.appends[0].next
            ),
            (7, 3, 10)
        );
        assert_eq!(plan.appends[0].descriptor.as_ref(), OBJECT);
        let mut progress = Progress::new(plan);
        assert_eq!(progress.expected(), 4);
        progress.parts.push("\"tag:\"".into());
        assert_eq!(progress.expected(), 7);
        progress.appended = 1;
        assert_eq!(progress.expected(), 10);
    }

    #[test]
    fn sdk_shadows_missing_hierarchy_and_budget_decline() {
        let class = fixture(chain());
        assert!(plan_at(&class, &class.methods[0], 0).is_none());
        attach_hierarchy(&class, None);
        assert!(plan_at(&class, &class.methods[0], 0).is_some());
        let mut over_budget = fixture(chain());
        over_budget.methods[0].code.as_mut().unwrap().registers = MAX_REGISTERS + 1;
        attach_hierarchy(&over_budget, None);
        assert!(plan_at(&over_budget, &over_budget.methods[0], 0).is_none());
        for descriptor in [BUILDER, STRING, OBJECT] {
            let class = fixture(chain());
            let shadow = DexClass {
                descriptor: descriptor.into(),
                superclass: None,
                interfaces: vec![],
                access_flags: 1,
                annotations_offset: 0,
                static_values_offset: 0,
                static_values: vec![],
                fields: vec![],
                methods: vec![],
                symbols: Arc::new(DexSymbols::default()),
            };
            attach_hierarchy(&class, Some(&shadow));
            assert!(
                plan_at(&class, &class.methods[0], 0).is_none(),
                "loaded shadow of {descriptor} must disable platform rewrite"
            );
        }
    }

    #[test]
    fn switch_entry_into_builder_chain_declines() {
        let mut words = vec![0x032b, 18, 0]; // packed-switch v3; payload at 18
        words.extend(chain());
        words.extend([0x0100, 1, 0, 0, 5, 0]); // case target PC5, the const-string
        let mut class = fixture(words);
        class.methods[0].parameters = vec!["I".into(), OBJECT.into()];
        let code = class.methods[0].code.as_mut().unwrap();
        code.registers = 5;
        code.ins = 2;
        code.instructions[12] = 0x0040; // append receives v4, the Object parameter
        attach_hierarchy(&class, None);
        assert_eq!(
            prefilter(&class, class.methods[0].code.as_ref().unwrap(), 3),
            Some(7)
        );
        assert!(plan_at(&class, &class.methods[0], 3).is_none());
    }

    #[test]
    fn operands_keep_primaries_bare_and_parenthesize_compound_expressions() {
        for bare in [
            "p0",
            "this.a",
            "v3",
            "\"a (\\\" b\"",
            "'c'",
            "-6",
            "1.5e-3f",
            "0x1F",
            "Hook.read()",
            "v0.get(1, \"x)\")",
            "v2[3]",
            "((int) v3)",
            "(v1 ? 1 : 0)",
            "null",
            "true",
        ] {
            assert_eq!(operand(bare), bare, "{bare}");
        }
        for compound in [
            "a + b",
            "(int) v3",
            "v0.a().b()",
            "-v1",
            "(a) + (b)",
            "x.y(z)[0]",
            "\"a\" + \"b\"",
        ] {
            assert_eq!(operand(compound), format!("({compound})"), "{compound}");
        }
    }
}
