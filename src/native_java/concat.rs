//! A single-use platform builder can be represented as Java string conversion.
//! This is a DEX/SSA proof, not a rewrite of emitted Java text.
use crate::{
    native_calls::{BoundCall, BoundCalls, CallKind, CallTarget},
    native_cfg::{ControlFlowGraph, instruction_width},
    native_dex::{DexClass, DexMethod},
    native_ir::{Instruction, PoolKind},
    native_method::MethodFrontEnd,
    native_ssa::{DefinitionKind, SsaMethod},
};

const BUILDER: &str = "Ljava/lang/StringBuilder;";
const STRING: &str = "Ljava/lang/String;";
const OBJECT: &str = "Ljava/lang/Object;";
const MAX_WORDS: usize = 4096;
const MAX_REGISTERS: u16 = 128;

pub(super) struct Plan {
    pub end: usize,
    pub argument: usize,
    pub result: usize,
    pub prefix: String,
}

fn call(calls: &BoundCalls, pc: usize) -> Option<&BoundCall> {
    calls.calls.iter().find(|call| call.pc == pc)
}

fn exact_call(
    class: &DexClass,
    call: &BoundCall,
    kind: CallKind,
    name: &str,
    args: &[&str],
    ret: &str,
    receiver: usize,
) -> bool {
    let CallTarget::Method {
        declaring_type,
        name: actual_name,
        prototype_index,
        ..
    } = &call.target
    else {
        return false;
    };
    call.kind == kind
        && declaring_type.as_ref() == BUILDER
        && actual_name.as_ref() == name
        && call.return_type.as_ref() == ret
        && call
            .receiver
            .as_ref()
            .is_some_and(|r| usize::from(r.register) == receiver)
        && class
            .symbols
            .protos
            .get(usize::from(*prototype_index))
            .is_some_and(|(actual_ret, actual_args)| {
                actual_ret.as_ref() == ret
                    && actual_args.len() == args.len()
                    && actual_args
                        .iter()
                        .zip(args)
                        .all(|(actual, wanted)| actual.as_ref() == *wanted)
            })
        && call.arguments.len() == args.len()
        && call
            .arguments
            .iter()
            .zip(args)
            .all(|(actual, wanted)| actual.descriptor.as_ref() == *wanted)
}

fn single_register(operands: &[crate::native_ir::RegisterOperand]) -> Option<usize> {
    (operands.len() == 1).then(|| usize::from(operands[0].register))
}

fn value_id(ssa: &SsaMethod, pc: usize, register: usize, read: bool) -> Option<usize> {
    let instruction = ssa
        .instructions
        .iter()
        .find(|instruction| instruction.pc == pc)?;
    let operands = if read {
        &instruction.reads
    } else {
        &instruction.writes
    };
    let value = operands
        .iter()
        .find(|operand| usize::from(operand.register) == register)?;
    (value.words.len() == 1).then_some(value.words[0])
}

fn only_uses(ssa: &SsaMethod, id: usize, expected: &[(usize, usize)]) -> bool {
    let mut uses = Vec::new();
    for instruction in &ssa.instructions {
        for operand in &instruction.reads {
            if operand.words.contains(&id) {
                uses.push((instruction.pc, usize::from(operand.register)));
            }
        }
    }
    uses == expected
}

fn six_instruction_shape(class: &DexClass, method: &DexMethod, start: usize) -> Option<[usize; 6]> {
    let code = method.code.as_ref()?;
    if code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() > MAX_WORDS
        || code.registers > MAX_REGISTERS
    {
        return None;
    }
    let words = &code.instructions;
    if words.get(start).copied()? as u8 != 0x22
        || class
            .symbols
            .types
            .get(usize::from(*words.get(start + 1)?))?
            .as_ref()
            != BUILDER
    {
        return None;
    }
    let mut pcs = [0; 6];
    let mut pc = start;
    for slot in &mut pcs {
        *slot = pc;
        pc += instruction_width(words, pc).ok()?.0;
    }
    let ops = pcs.map(|pc| words[pc] as u8);
    (ops[0] == 0x22
        && matches!(ops[1], 0x1a | 0x1b)
        && matches!(ops[2], 0x70 | 0x76)
        && matches!(ops[3], 0x6e | 0x74)
        && matches!(ops[4], 0x6e | 0x74)
        && ops[5] == 0x0c)
        .then_some(pcs)
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

pub(super) fn plan_at(class: &DexClass, method: &DexMethod, start: usize) -> Option<Plan> {
    // The cheap opcode/pool filter prevents CFG and SSA work for ordinary
    // allocations. The selected chain is then proved by canonical identities.
    let pcs = six_instruction_shape(class, method, start)?;
    let code = method.code.as_ref()?;
    // Most builder chains use another append overload. Reject those before
    // decoding and binding every instruction in the surrounding method.
    for (pc, name, parameters, result) in [
        (pcs[2], "<init>", &[STRING][..], "V"),
        (pcs[3], "append", &[OBJECT][..], BUILDER),
        (pcs[4], "toString", &[][..], STRING),
    ] {
        if !pool_signature_matches(class, code.instructions[pc + 1], name, parameters, result) {
            return None;
        }
    }
    let hierarchy = class.symbols.hierarchy.get()?;
    if ![BUILDER, STRING, OBJECT]
        .into_iter()
        .all(|ty| hierarchy.unshadowed_sdk_type(ty))
    {
        return None;
    }
    let front = MethodFrontEnd::build(class, method).ok()?;
    let ir = &front.ir;
    let index = ir
        .instructions
        .binary_search_by_key(&start, |i| i.pc)
        .ok()?;
    let ins: &[Instruction] = ir.instructions.get(index..index + 6)?;
    if !ins.iter().zip(pcs).all(|(i, pc)| i.pc == pc)
        || ir.instructions.iter().any(|i| {
            i.branch_target
                .is_some_and(|target| start < target && target < ins[5].pc + ins[5].width)
                || i.payload_target
                    .is_some_and(|target| start < target && target < ins[5].pc + ins[5].width)
        })
    {
        return None;
    }
    let builder = single_register(&ins[0].writes)?;
    let prefix_reg = single_register(&ins[1].writes)?;
    let result = single_register(&ins[5].writes)?;
    if builder == prefix_reg || builder == result || prefix_reg == result {
        return None;
    }
    let prefix_ref = ins[1].reference?;
    if prefix_ref.kind != PoolKind::String {
        return None;
    }
    let prefix = class
        .symbols
        .strings
        .get(usize::try_from(prefix_ref.index).ok()?)?;
    let init = call(&front.bound, pcs[2])?;
    let append = call(&front.bound, pcs[3])?;
    let to_string = call(&front.bound, pcs[4])?;
    if !exact_call(
        class,
        init,
        CallKind::Direct,
        "<init>",
        &[STRING],
        "V",
        builder,
    ) || init.arguments[0].register != prefix_reg as u16
        || init.result.is_some()
        || !exact_call(
            class,
            append,
            CallKind::Virtual,
            "append",
            &[OBJECT],
            BUILDER,
            builder,
        )
        || append.result.is_some()
        || !exact_call(
            class,
            to_string,
            CallKind::Virtual,
            "toString",
            &[],
            STRING,
            builder,
        )
        || !to_string.result.as_ref().is_some_and(|value| {
            value.move_pc == pcs[5] && usize::from(value.register.register) == result
        })
    {
        return None;
    }
    let argument = usize::from(append.arguments[0].register);
    if [builder, prefix_reg, result].contains(&argument) {
        return None;
    }
    // Only a formal reference parameter has the stable, emitter-owned pN
    // expression used by this first slice. The implicit receiver is excluded.
    let mut parameter_register = usize::from(code.registers) - usize::from(code.ins)
        + usize::from(method.access_flags & 0x0008 == 0);
    let mut formal_reference = false;
    for ty in &method.parameters {
        if parameter_register == argument {
            formal_reference = ty.starts_with('L') || ty.starts_with('[');
            break;
        }
        parameter_register += usize::from(matches!(ty.as_ref(), "J" | "D")) + 1;
    }
    if !formal_reference {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    let block = cfg.block_at.get(&start)?;
    if !pcs.iter().all(|pc| cfg.block_at.get(pc) == Some(block)) {
        return None;
    }
    let ssa = SsaMethod::build(code, ir, &cfg).ok()?;
    let builder_id = value_id(&ssa, pcs[0], builder, false)?;
    let prefix_id = value_id(&ssa, pcs[1], prefix_reg, false)?;
    let argument_id = value_id(&ssa, pcs[3], argument, true)?;
    if !matches!(
        ssa.definitions.get(argument_id)?.kind,
        DefinitionKind::Parameter
    ) || value_id(&ssa, pcs[2], builder, true)? != builder_id
        || value_id(&ssa, pcs[3], builder, true)? != builder_id
        || value_id(&ssa, pcs[4], builder, true)? != builder_id
        || value_id(&ssa, pcs[2], prefix_reg, true)? != prefix_id
        || !only_uses(
            &ssa,
            builder_id,
            &[(pcs[2], builder), (pcs[3], builder), (pcs[4], builder)],
        )
        || !only_uses(&ssa, prefix_id, &[(pcs[2], prefix_reg)])
        || ssa.phis.iter().any(|phi| {
            phi.incoming
                .iter()
                .any(|(_, id)| *id == builder_id || *id == prefix_id)
        })
    {
        return None;
    }
    Some(Plan {
        end: pcs[5] + ins[5].width,
        argument,
        result,
        prefix: prefix.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use std::sync::Arc;

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
        assert!(six_instruction_shape(&class, &class.methods[0], 3).is_some());
        assert!(plan_at(&class, &class.methods[0], 3).is_none());
    }
}
