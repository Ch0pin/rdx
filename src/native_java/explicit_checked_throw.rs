//! Exact fresh checked throws retain their object and the original source header.
use super::{method, names};
use crate::native_dex::{DexClass, DexMethod};

fn site(class: &DexClass, method: &DexMethod, pc: usize) -> Option<String> {
    if method.name.as_ref() == "<clinit>" {
        return None;
    }
    let code = method.code.as_ref()?;
    let words = &code.instructions;
    let new_pc = pc.checked_sub(5)?;
    let new = *words.get(new_pc)?;
    let invoke = *words.get(new_pc + 2)?;
    let throw = *words.get(pc)?;
    if new as u8 != 0x22 || throw as u8 != 0x27 || new >> 8 != throw >> 8 {
        return None;
    }
    let receiver = new >> 8;
    let operands = *words.get(new_pc + 4)?;
    if !((invoke as u8 == 0x70 && invoke >> 12 == 1 && invoke & 0xf00 == 0 && operands == receiver)
        || (invoke as u8 == 0x76 && invoke >> 8 == 1 && operands == receiver))
    {
        return None;
    }
    let ty = class.symbols.types.get(*words.get(new_pc + 1)? as usize)?;
    let (owner, proto, name) = *class
        .symbols
        .methods
        .get(*words.get(new_pc + 3)? as usize)?;
    let (result, args) = class.symbols.protos.get(proto as usize)?;
    if class.symbols.types.get(owner as usize)? != ty
        || class.symbols.strings.get(name as usize)? != "<init>"
        || result.as_ref() != "V"
        || !args.is_empty()
        || !method::explicit_checked_throw_type(class, ty)
    {
        return None;
    }
    // Raw words alone cannot establish instruction boundaries or ownership.
    let ir = crate::native_ir::DecodedMethod::decode(code).ok()?;
    if ![new_pc, new_pc + 2, pc]
        .into_iter()
        .all(|at| ir.instructions.iter().any(|i| i.pc == at))
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
    vec![format!("\n    // Preserve the original explicit checked throwable and caller header.\n    @java.lang.SuppressWarnings(\"unchecked\")\n    private static <E extends java.lang.Throwable> java.lang.RuntimeException {name}(java.lang.Throwable failure) throws E {{\n        throw (E) failure;\n    }}\n")]
}
