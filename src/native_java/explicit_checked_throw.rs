//! Certified checked throws retain their object and the original source header.
use super::{method, names};
use crate::native_dex::{DexClass, DexMethod};

// A checked field value guarded against null can be thrown without altering
// its identity. The original CFG establishes that the guard cannot be bypassed.
fn nonnull_field_sites(class: &DexClass, method: &DexMethod) -> Vec<(usize, String)> {
    let prove = || -> Option<Vec<(usize, String)>> {
        let code = method.code.as_ref()?;
        if method.name.as_ref() == "<clinit>"
            || method.declaring_type != class.descriptor
            || code.instructions.len() > 4096
            || !code.try_regions.is_empty()
            || code.tries != 0
        {
            return None;
        }
        // This raw scan grants no authority; avoid decoding ordinary methods
        // that cannot contain the exact adjacent field/guard/throw shape.
        let candidate = code.instructions.windows(4).enumerate().any(|(pc, words)| {
            let field = words[0];
            let guard = words[2];
            let register = match field as u8 {
                0x54 => (field >> 8) & 15,
                0x62 => field >> 8,
                _ => return false,
            };
            if !matches!(guard as u8, 0x38 | 0x39) || guard >> 8 != register {
                return false;
            }
            let throw_pc = if guard as u8 == 0x38 {
                Some(pc + 4)
            } else {
                (pc + 2).checked_add_signed(isize::from(words[3] as i16))
            };
            throw_pc
                .and_then(|at| code.instructions.get(at))
                .is_some_and(|raw| *raw as u8 == 0x27 && *raw >> 8 == register)
        });
        if !candidate {
            return None;
        }
        let ir = crate::native_ir::DecodedMethod::decode(code).ok()?;
        let cfg = crate::native_cfg::ControlFlowGraph::build(code).ok()?;
        if cfg.blocks.len() > 1024 {
            return None;
        }
        let mut result = Vec::new();
        for pair in ir.instructions.windows(2) {
            let field = &pair[0];
            let guard = &pair[1];
            if !matches!(field.opcode, 0x54 | 0x62)
                || field.width != 2
                || !matches!(guard.opcode, 0x38 | 0x39)
                || guard.width != 2
                || guard.pc != field.pc + 2
            {
                continue;
            }
            let raw_field = *code.instructions.get(field.pc)?;
            let register = if field.opcode == 0x54 {
                (raw_field >> 8) & 15
            } else {
                raw_field >> 8
            };
            if *code.instructions.get(guard.pc)? >> 8 != register {
                continue;
            }
            let throw_pc = if guard.opcode == 0x39 {
                guard.branch_target?
            } else {
                guard.pc + 2
            };
            // Both guard arms must differ; otherwise null reaches the throw.
            if throw_pc == 0 || guard.branch_target? == guard.pc + 2 {
                continue;
            }
            let Some(throw) = ir.instructions.iter().find(|ins| ins.pc == throw_pc) else {
                continue;
            };
            if throw.opcode != 0x27 || *code.instructions.get(throw_pc)? >> 8 != register {
                continue;
            }
            let field_block = *cfg.block_at.get(&field.pc)?;
            let guard_block = *cfg.block_at.get(&guard.pc)?;
            let throw_block = *cfg.block_at.get(&throw_pc)?;
            if field_block != guard_block
                || guard_block == throw_block
                || cfg.blocks[throw_block].start != throw_pc
                || cfg.blocks[guard_block].instructions.last() != Some(&guard.pc)
            {
                continue;
            }
            let predecessors: Vec<_> = cfg
                .blocks
                .iter()
                .enumerate()
                .flat_map(|(from, block)| {
                    block
                        .successors
                        .iter()
                        .filter(move |edge| edge.target == throw_block)
                        .map(move |edge| (from, edge.kind))
                })
                .collect();
            if predecessors != [(guard_block, crate::native_cfg::EdgeKind::Normal)] {
                continue;
            }
            let (owner, ty, name) = *class
                .symbols
                .fields
                .get(*code.instructions.get(field.pc + 1)? as usize)?;
            let owner = class.symbols.types.get(owner as usize)?;
            let ty = class.symbols.types.get(ty as usize)?;
            let name = class.symbols.strings.get(name as usize)?;
            if !method::explicit_checked_throw_type(class, ty) {
                continue;
            }
            if owner == &class.descriptor {
                let mut declarations = class
                    .fields
                    .iter()
                    .filter(|field| field.name.as_ref() == name.as_str());
                let declaration = declarations.next()?;
                let is_static = field.opcode == 0x62;
                if declarations.next().is_some()
                    || declaration.declaring_type != class.descriptor
                    || declaration.field_type != *ty
                    || declaration.is_static != is_static
                    || (declaration.access_flags & 8 != 0) != is_static
                    || declaration.access_flags & !0x10df != 0
                    || declaration.access_flags & 0x50 == 0x50
                    || (declaration.access_flags & 7).count_ones() > 1
                {
                    continue;
                }
            }
            result.push((throw_pc, ty.to_string()));
        }
        Some(result)
    };
    prove().unwrap_or_default()
}

/// A throw of a freshly constructed checked throwable: `new-instance r, T`,
/// any computation of the constructor arguments that leaves `r` untouched,
/// the `T.<init>` call on `r`, and `throw r` immediately after it. Rendering
/// still evaluates the original constructor exactly once, so argument
/// expressions may be arbitrary straight-line code.
fn site(class: &DexClass, method: &DexMethod, pc: usize) -> Option<String> {
    if method.name.as_ref() == "<clinit>" {
        return None;
    }
    let code = method.code.as_ref()?;
    let words = &code.instructions;
    let throw = *words.get(pc)?;
    // Cheap raw-word prefilter before decoding: a constructor call must end
    // right before the throw and name the thrown register as its receiver.
    let invoke_pc = pc.checked_sub(3)?;
    let invoke = *words.get(invoke_pc)?;
    let receiver = throw >> 8;
    if throw as u8 != 0x27
        || !((invoke as u8 == 0x70 && *words.get(invoke_pc + 2)? & 15 == receiver)
            || (invoke as u8 == 0x76 && *words.get(invoke_pc + 2)? == receiver))
    {
        return None;
    }
    let (owner, proto, name) = *class
        .symbols
        .methods
        .get(*words.get(invoke_pc + 1)? as usize)?;
    let ty = class.symbols.types.get(owner as usize)?;
    let (result, _) = class.symbols.protos.get(proto as usize)?;
    if class.symbols.strings.get(name as usize)? != "<init>"
        || result.as_ref() != "V"
        || !method::explicit_checked_throw_type(class, ty)
    {
        return None;
    }
    // Raw words alone cannot establish instruction boundaries or ownership.
    let ir = crate::native_ir::DecodedMethod::decode(code).ok()?;
    let throw_index = ir.instructions.iter().position(|i| i.pc == pc)?;
    let throw = &ir.instructions[throw_index];
    let init = ir.instructions.get(throw_index.checked_sub(1)?)?;
    let touches = |operands: &[crate::native_ir::RegisterOperand]| {
        operands.iter().any(|operand| {
            operand.register == receiver
                || (operand.kind == crate::native_ir::ValueKind::Wide64
                    && operand.register + 1 == receiver)
        })
    };
    if throw.opcode != 0x27
        || init.pc != invoke_pc
        || init.pc + init.width != pc
        || !matches!(init.opcode, 0x70 | 0x76)
        || init.reads.first()?.register != receiver
        || touches(&init.reads[1..])
    {
        return None;
    }
    let mut new_pc = None;
    for instruction in ir.instructions[..throw_index - 1].iter().rev() {
        if instruction.opcode == 0x22 && touches(&instruction.writes) {
            let allocated = instruction
                .reference
                .filter(|reference| reference.kind == crate::native_ir::PoolKind::Type)
                .and_then(|reference| class.symbols.types.get(reference.index as usize))?;
            if allocated != ty {
                return None;
            }
            new_pc = Some(instruction.pc);
            break;
        }
        // The allocation and its constructor must sit on one straight path
        // that neither observes nor redefines the uninitialized reference.
        if matches!(
            instruction.opcode,
            0x0d | 0x0e..=0x11 | 0x27..=0x2c | 0x32..=0x3d
        ) || touches(&instruction.reads)
            || touches(&instruction.writes)
        {
            return None;
        }
    }
    let new_pc = new_pc?;
    // No other path may join the sequence between the allocation and the
    // throw: every block inside it has exactly its fallthrough predecessor.
    let cfg = crate::native_cfg::ControlFlowGraph::build(code).ok()?;
    for (index, block) in cfg.blocks.iter().enumerate() {
        if block.start <= new_pc || block.start > pc {
            continue;
        }
        let mut predecessors = cfg.blocks.iter().enumerate().flat_map(|(from, candidate)| {
            candidate
                .successors
                .iter()
                .filter(move |edge| edge.target == index)
                .map(move |edge| (from, edge.kind))
        });
        let fallthrough = predecessors.next()?;
        if predecessors.next().is_some()
            || fallthrough.1 != crate::native_cfg::EdgeKind::Normal
            || cfg.blocks[fallthrough.0].end != block.start
        {
            return None;
        }
    }
    Some(ty.to_string())
}

fn helper_name(class: &DexClass) -> Option<String> {
    if class.access_flags & (0x200 | 0x2000 | 0x4000) != 0
        || class.methods.len() > 8192
        || !super::source_header_authority(class)
    {
        return None;
    }
    if class
        .methods
        .iter()
        .filter_map(|m| m.code.as_ref())
        .try_fold(0usize, |n, c| n.checked_add(c.instructions.len()))
        .is_none_or(|n| n > 65536)
    {
        return None;
    }
    (0..64)
        .map(|n| format!("_rdx_checked_throw_{n}"))
        .find(|candidate| {
            !class
                .methods
                .iter()
                .any(|m| names::member(&m.name).ok().as_deref() == Some(candidate))
                && !super::helper_type_names::raw_contains_helper(&class.symbols, candidate)
        })
}
pub(super) fn for_site(
    class: &DexClass,
    method: &DexMethod,
    pc: usize,
    ty: &str,
) -> Option<String> {
    if method.code.as_ref()?.instructions.len() > 4096 || site(class, method, pc)? != ty {
        return None;
    }
    helper_name(class)
}
pub(super) fn for_nonnull_field_site(
    class: &DexClass,
    method: &DexMethod,
    pc: usize,
    register: usize,
    ty: &str,
) -> Option<String> {
    let raw = *method.code.as_ref()?.instructions.get(pc)?;
    if raw as u8 != 0x27 || usize::from(raw >> 8) != register {
        return None;
    }
    nonnull_field_sites(class, method)
        .iter()
        .any(|(site, established)| *site == pc && established == ty)
        .then(|| helper_name(class))
        .flatten()
}
pub(super) fn definitions(class: &DexClass) -> Vec<String> {
    let Some(name) = helper_name(class) else {
        return vec![];
    };
    // Bound the declaration scan independently of the per-site renderer.
    let mut words = 0usize;
    for method in &class.methods {
        let Some(code) = &method.code else { continue };
        words = words.saturating_add(code.instructions.len());
        if words > 65536 {
            return vec![];
        }
    }
    if !class.methods.iter().any(|method| {
        if !nonnull_field_sites(class, method).is_empty() {
            return true;
        }
        method.code.as_ref().is_some_and(|code| {
            code.instructions.len() <= 4096
                && code
                    .instructions
                    .iter()
                    .enumerate()
                    .any(|(pc, word)| *word as u8 == 0x27 && site(class, method, pc).is_some())
        })
    }) {
        return vec![];
    }
    vec![format!(
        "\n    // Preserve the original explicit checked throwable and caller header.\n    @java.lang.SuppressWarnings(\"unchecked\")\n    private static <E extends java.lang.Throwable> java.lang.RuntimeException {name}(java.lang.Throwable failure) throws E {{\n        throw (E) failure;\n    }}\n"
    )]
}

#[cfg(test)]
mod tests {
    use super::site;
    use crate::native_dex::{DexClass, DexCode, DexMethod, DexSymbols};
    use std::sync::Arc;

    const CHECKED: &str = "Ljava/io/IOException;";

    // Registers: v0 scratch, v1 the throwable, v2 `this`.
    fn fixture(words: &[u16]) -> DexClass {
        let symbols = Arc::new(DexSymbols {
            types: vec![
                CHECKED.into(),
                "Ljava/lang/Object;".into(),
                "Ljava/lang/String;".into(),
                "Lsample/Caller;".into(),
            ],
            strings: vec!["<init>".into(), "toString".into(), "message".into()],
            protos: vec![
                ("V".into(), vec!["Ljava/lang/String;".into()]),
                ("Ljava/lang/String;".into(), vec![]),
                ("V".into(), vec![]),
            ],
            methods: vec![(0, 0, 0), (1, 1, 1), (0, 2, 0), (1, 2, 0)],
            ..Default::default()
        });
        DexClass {
            descriptor: "Lsample/Caller;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![DexMethod {
                declaring_type: "Lsample/Caller;".into(),
                name: "run".into(),
                return_type: "V".into(),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 1,
                code: Some(DexCode {
                    registers: 3,
                    ins: 1,
                    outs: 2,
                    tries: 0,
                    try_regions: vec![],
                    instructions: words.to_vec(),
                    offset: 0,
                }),
            }],
            symbols,
        }
    }

    fn prove(words: &[u16], throw_pc: usize) -> Option<String> {
        let class = fixture(words);
        site(&class, &class.methods[0], throw_pc)
    }

    #[test]
    fn computed_constructor_arguments_prove_the_fresh_throw() {
        // new-instance v1; v0 = this.toString(); new <init>(v0); throw v1
        let computed = [
            0x0122, 0, 0x106e, 1, 0x0002, 0x000c, 0x2070, 0, 0x0001, 0x0127,
        ];
        assert_eq!(prove(&computed, 9).as_deref(), Some(CHECKED));
        // The range form with the argument staged next to the receiver.
        let range = [
            0x0122, 0, 0x106e, 1, 0x0002, 0x020c, 0x0276, 0, 0x0001, 0x0127,
        ];
        assert_eq!(prove(&range, 9).as_deref(), Some(CHECKED));
        // A nested allocation for the argument keeps the outer proof.
        let nested = [
            0x0122, 0, 0x0022, 1, 0x1070, 3, 0x0000, 0x106e, 1, 0x0000, 0x000c, 0x2070, 0, 0x0001,
            0x0127,
        ];
        assert_eq!(prove(&nested, 14).as_deref(), Some(CHECKED));
    }

    #[test]
    fn sequence_rejects_receiver_use_control_transfer_and_joins() {
        // The receiver is redefined by the argument computation.
        let redefined = [
            0x0122, 0, 0x106e, 1, 0x0002, 0x010c, 0x2070, 0, 0x0001, 0x0127,
        ];
        assert_eq!(prove(&redefined, 9), None);
        // The uninitialized receiver is observed before its constructor.
        let observed = [0x0122, 0, 0x1007, 0x2070, 0, 0x0001, 0x0127];
        assert_eq!(prove(&observed, 6), None);
        // A return separates the allocation from its constructor.
        let returned = [0x0122, 0, 0x000e, 0x001a, 0x0002, 0x2070, 0, 0x0001, 0x0127];
        assert_eq!(prove(&returned, 8), None);
        // The throw does not follow the constructor directly.
        let detached = [0x0122, 0, 0x001a, 0x0002, 0x2070, 0, 0x0001, 0x0000, 0x0127];
        assert_eq!(prove(&detached, 8), None);
        // Two allocations join before one shared constructor call.
        let joined = [
            0x0238, 0x0005, 0x0122, 0, 0x0328, 0x0122, 0, 0x001a, 0x0002, 0x2070, 0, 0x0001, 0x0127,
        ];
        assert_eq!(prove(&joined, 12), None);
        // Without the join, the same tail proves.
        let straight = [0x0122, 0, 0x001a, 0x0002, 0x2070, 0, 0x0001, 0x0127];
        assert_eq!(prove(&straight, 7).as_deref(), Some(CHECKED));
        // The allocation names another type than the constructor.
        let retyped = [0x0122, 1, 0x001a, 0x0002, 0x2070, 0, 0x0001, 0x0127];
        assert_eq!(prove(&retyped, 7), None);
        // The constructor owner is no checked throwable.
        let mut class = fixture(&straight);
        Arc::get_mut(&mut class.symbols).unwrap().methods[0].0 = 1;
        assert_eq!(site(&class, &class.methods[0], 7), None);
    }
}
