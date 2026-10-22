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

fn site(class: &DexClass, method: &DexMethod, pc: usize) -> Option<String> {
    if method.name.as_ref() == "<clinit>" {
        return None;
    }
    // Constructor arguments remain deliberately bounded to an adjacent constant
    // String. Rendering still evaluates the original constructor exactly once.
    [0usize, 2, 3]
        .into_iter()
        .find_map(|string_words| site_shape(class, method, pc, string_words))
}

fn site_shape(
    class: &DexClass,
    method: &DexMethod,
    pc: usize,
    string_words: usize,
) -> Option<String> {
    let code = method.code.as_ref()?;
    let words = &code.instructions;
    let new_pc = pc.checked_sub(5 + string_words)?;
    let invoke_pc = new_pc + 2 + string_words;
    let new = *words.get(new_pc)?;
    let invoke = *words.get(invoke_pc)?;
    let throw = *words.get(pc)?;
    if new as u8 != 0x22 || throw as u8 != 0x27 || new >> 8 != throw >> 8 {
        return None;
    }
    let receiver = new >> 8;
    let message = if string_words == 0 {
        None
    } else {
        let constant = *words.get(new_pc + 2)?;
        if constant as u8 != if string_words == 2 { 0x1a } else { 0x1b } {
            return None;
        }
        let index = usize::from(*words.get(new_pc + 3)?)
            | if string_words == 3 {
                usize::from(*words.get(new_pc + 4)?) << 16
            } else {
                0
            };
        class.symbols.strings.get(index)?;
        let register = constant >> 8;
        if register == receiver {
            return None;
        }
        Some(register)
    };
    let count = if message.is_some() { 2 } else { 1 };
    let operands = *words.get(invoke_pc + 2)?;
    if !((invoke as u8 == 0x70
        && invoke >> 12 == count
        && invoke & 0xf00 == 0
        && operands == (receiver | (message.unwrap_or(0) << 4))
        && receiver < 16
        && message.is_none_or(|register| register < 16))
        || (invoke as u8 == 0x76
            && invoke >> 8 == count
            && operands == receiver
            && message.is_none_or(|register| register == receiver + 1)))
    {
        return None;
    }
    let ty = class.symbols.types.get(*words.get(new_pc + 1)? as usize)?;
    let (owner, proto, name) = *class
        .symbols
        .methods
        .get(*words.get(invoke_pc + 1)? as usize)?;
    let (result, args) = class.symbols.protos.get(proto as usize)?;
    if class.symbols.types.get(owner as usize)? != ty
        || class.symbols.strings.get(name as usize)? != "<init>"
        || result.as_ref() != "V"
        || (if message.is_some() {
            args.len() != 1 || args[0].as_ref() != "Ljava/lang/String;"
        } else {
            !args.is_empty()
        })
        || !method::explicit_checked_throw_type(class, ty)
    {
        return None;
    }
    // Raw words alone cannot establish instruction boundaries or ownership.
    let ir = crate::native_ir::DecodedMethod::decode(code).ok()?;
    if ![new_pc, invoke_pc, pc]
        .into_iter()
        .all(|at| ir.instructions.iter().any(|i| i.pc == at))
        || (message.is_some() && !ir.instructions.iter().any(|i| i.pc == new_pc + 2))
    {
        return None;
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
