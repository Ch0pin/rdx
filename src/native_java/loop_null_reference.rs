//! Independent reference lifetimes for a shared DEX zero at a loop phi.
//! Original check-casts remain explicit and start their own typed value family.
use crate::{
    native_call_values::SsaCalls,
    native_dex::{DexClass, DexMethod},
    native_ir::DecodedMethod,
    native_ssa::SsaMethod,
    native_types::{AssignmentBound, InferredTypes, TypeResolution},
};
use std::collections::{HashMap, HashSet};
#[allow(clippy::too_many_arguments)]
pub(crate) fn select(
    class: &DexClass,
    method: &DexMethod,
    ir: &DecodedMethod,
    ssa: &SsaMethod,
    calls: &SsaCalls,
    types: &InferredTypes,
    header: usize,
    register: usize,
) -> Option<String> {
    let hierarchy = class.symbols.hierarchy.get().map(AsRef::as_ref);
    let reference = |ty: &str| ty.starts_with('L') || ty.starts_with('[');
    let assignable = |from: &str, to: &str| {
        reference(from)
            && reference(to)
            && (from == to
                || to == "Ljava/lang/Object;"
                || hierarchy.is_some_and(|h: &crate::native_hierarchy::TypeHierarchy| {
                    h.assignable(from, to) == crate::native_hierarchy::Relation::Proven
                }))
    };
    let mut heads = ssa.phis.iter().filter(|p| {
        usize::from(p.register) == register
            && ssa
                .graph
                .blocks
                .get(p.block)
                .is_some_and(|b| b.start == header)
    });
    let phi = heads.next()?;
    if heads.next().is_some() || phi.incoming.is_empty() {
        return None;
    }
    let inferred = types.values.get(phi.result)?;
    if !inferred.assignment.contains(&AssignmentBound::Literal {
        bits: 0,
        wide: false,
    }) {
        return None;
    }
    let candidates: Vec<_> = inferred
        .assignment
        .iter()
        .filter_map(|b| match b {
            AssignmentBound::Type(ty) if reference(ty) => Some(ty.as_ref()),
            _ => None,
        })
        .collect();
    let first = *candidates.first()?;
    let ty = match &inferred.resolution {
        TypeResolution::Resolved(ty)
            if reference(ty) && candidates.iter().all(|actual| assignable(actual, ty)) =>
        {
            ty.to_string()
        }
        _ if candidates.iter().all(|other| *other == first) => first.to_string(),
        _ => "Ljava/lang/Object;".to_string(),
    };
    super::handler_null_type::reference_value_as(ssa, types, phi.result, &ty, hierarchy)?;
    let at: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    if at.len() != ir.instructions.len() {
        return None;
    }
    let mut pending = vec![(phi.result, ty.clone())];
    let mut seen = HashSet::new();
    let mut work = 100_000usize;
    while let Some((id, current)) = pending.pop() {
        if !seen.insert((id, current.clone())) {
            continue;
        }
        super::handler_null_type::reference_value_as(ssa, types, id, &current, hierarchy)?;
        for other in &ssa.phis {
            work = work.checked_sub(1)?;
            if other.incoming.iter().any(|(_, value)| *value == id) {
                let next =
                    super::handler_null_type::reference_value(ssa, types, other.result, hierarchy)
                        .or_else(|| {
                            super::handler_null_type::reference_value_as(
                                ssa,
                                types,
                                other.result,
                                &current,
                                hierarchy,
                            )
                        })?;
                if !assignable(&current, &next) {
                    return None;
                }
                pending.push((other.result, next));
            }
        }
        for use_site in &ssa.instructions {
            work = work.checked_sub(1)?;
            if !use_site.reads.iter().any(|r| r.words.contains(&id)) {
                continue;
            }
            let ins = at.get(&use_site.pc)?;
            match ins.opcode {
                0x07..=0x09 => {
                    for value in use_site.writes.iter().flat_map(|w| &w.words) {
                        pending.push((*value, current.clone()));
                    }
                }
                0x1f => {
                    let target = class.symbols.types.get(ins.reference?.index as usize)?;
                    if !reference(target) {
                        return None;
                    }
                    // This is the original DEX runtime check, emitted at its original use.
                    // Its successful result has the target type; no join cast is invented.
                    for value in use_site.writes.iter().flat_map(|w| &w.words) {
                        super::handler_null_type::reference_value_as(
                            ssa, types, *value, target, hierarchy,
                        )?;
                        pending.push((*value, target.to_string()));
                    }
                }
                0x20 | 0x38 | 0x39 => {}
                0x32 | 0x33 => {
                    if !use_site.reads.iter().flat_map(|r| &r.words).all(|other| {
                        types.values.get(*other).is_some_and(|v| {
                            !v.assignment_bounds_truncated
                                && !v.assignment.is_empty()
                                && v.assignment.iter().all(|b| {
                                    matches!(
                                        b,
                                        AssignmentBound::Literal {
                                            bits: 0,
                                            wide: false
                                        }
                                    ) || matches!(b,AssignmentBound::Type(t)if reference(t))
                                })
                        })
                    }) {
                        return None;
                    }
                }
                0x52..=0x5f => {
                    use crate::native_ir::{PoolKind, ValueKind};
                    let put = ins.opcode >= 0x59;
                    let family = if put {
                        ins.opcode - 0x59
                    } else {
                        ins.opcode - 0x52
                    };
                    let field = ins.reference?;
                    if field.kind != PoolKind::Field || ins.width != 2 || !ins.may_throw {
                        return None;
                    }
                    let &(owner, descriptor, _) = class.symbols.fields.get(field.index as usize)?;
                    let owner = class.symbols.types.get(owner as usize)?;
                    let descriptor = class.symbols.types.get(descriptor as usize)?;
                    if !owner.starts_with('L')
                        || !owner.ends_with(';')
                        || !match family {
                            0 => matches!(descriptor.as_ref(), "I" | "F"),
                            1 => matches!(descriptor.as_ref(), "J" | "D"),
                            2 => reference(descriptor),
                            3 => descriptor.as_ref() == "Z",
                            4 => descriptor.as_ref() == "B",
                            5 => descriptor.as_ref() == "C",
                            6 => descriptor.as_ref() == "S",
                            _ => false,
                        }
                    {
                        return None;
                    }
                    if ins.reads.len() != 1 + usize::from(put)
                        || use_site.reads.len() != ins.reads.len()
                        || ins.writes.len() != usize::from(!put)
                    {
                        return None;
                    }
                    // The original receiver and stored value are independent
                    // roles. A reference may satisfy both only if both declared
                    // types accept its current SSA family without a new cast.
                    for (index, (raw, read)) in ins.reads.iter().zip(&use_site.reads).enumerate() {
                        let expected_kind = if index == 0 || family == 2 {
                            ValueKind::Reference
                        } else if family == 1 {
                            ValueKind::Wide64
                        } else {
                            ValueKind::Bits32
                        };
                        if raw.kind != expected_kind
                            || read.register != raw.register
                            || read.words.len() != expected_kind.word_count()
                        {
                            return None;
                        }
                        if read.words.contains(&id)
                            && (read.words.len() != 1
                                || !(if index == 0 {
                                    assignable(&current, owner)
                                } else {
                                    family == 2 && assignable(&current, descriptor)
                                }))
                        {
                            return None;
                        }
                    }
                    // Field loads start their own value family. They do not
                    // alias the receiver merely because a DEX register is reused.
                }
                0x11 if assignable(&current, &method.return_type) => {}
                0x24 | 0x25 => {
                    let array = class.symbols.types.get(ins.reference?.index as usize)?;
                    if !array
                        .strip_prefix('[')
                        .is_some_and(|component| assignable(&current, component))
                    {
                        return None;
                    }
                }
                0x6e..=0x72 | 0x74..=0x78 | 0xfa..=0xfd => {
                    let call = calls.calls.iter().find(|c| c.pc == ins.pc)?;
                    let mut found = false;
                    for argument in call.receiver.iter().chain(&call.arguments) {
                        if argument.words.contains(&id) {
                            found = true;
                            if argument.words.len() != 1
                                || !assignable(&current, &argument.descriptor)
                            {
                                return None;
                            }
                        }
                    }
                    if !found {
                        return None;
                    }
                }
                _ => return None,
            }
        }
    }
    Some(ty)
}
