//! Exact observed forwarding constructors removed by DEX optimization.
use crate::native_dex::{DexClass, DexMethod};
use crate::native_ir::{DecodedMethod, ValueKind};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RecoveredConstructor {
    pub parent: Arc<str>,
    pub invoked_owner: Arc<str>,
    pub parameters: Vec<Arc<str>>,
    pub thrown_types: Vec<Arc<str>>,
}

pub(super) fn recover(classes: &[&DexClass]) -> HashMap<Arc<str>, Vec<RecoveredConstructor>> {
    let mut owners = HashMap::new();
    let mut duplicates = HashSet::new();
    for &class in classes {
        if owners.insert(class.descriptor.as_ref(), class).is_some() {
            duplicates.insert(class.descriptor.as_ref());
        }
    }
    let candidates: HashSet<_> = classes
        .iter()
        .filter(|class| {
            !duplicates.contains(class.descriptor.as_ref())
                && class.access_flags & (0x200 | 0x400 | 0x4000) == 0
                && !class.descriptor.contains('$')
                && !class.methods.iter().any(|m| m.name.as_ref() == "<init>")
                && !class
                    .fields
                    .iter()
                    .any(|f| !f.is_static && f.access_flags & 0x10 != 0)
                && class
                    .superclass
                    .as_deref()
                    .and_then(|parent| owners.get(parent))
                    .is_some_and(|parent| {
                        parent.methods.iter().any(|method| {
                            method.name.as_ref() == "<init>" && !method.parameters.is_empty()
                        })
                    })
        })
        .map(|class| class.descriptor.as_ref())
        .collect();
    let mut result: HashMap<Arc<str>, BTreeMap<String, RecoveredConstructor>> = HashMap::new();
    let mut pool_relevance = HashMap::new();
    for &class in classes {
        // Cheap prefilter before decoding: no candidate type in this DEX pool.
        let relevant: &HashSet<usize> = pool_relevance
            .entry(Arc::as_ptr(&class.symbols) as usize)
            .or_insert_with(|| {
                class
                    .symbols
                    .types
                    .iter()
                    .enumerate()
                    .filter(|(_, ty)| candidates.contains(ty.as_ref()))
                    .map(|(i, _)| i)
                    .collect()
            });
        for method in &class.methods {
            let Some(code) = &method.code else {
                continue;
            };
            if !code
                .instructions
                .windows(2)
                .any(|w| w[0] as u8 == 0x22 && relevant.contains(&(w[1] as usize)))
            {
                continue;
            }
            let Ok(ir) = DecodedMethod::decode(code) else {
                continue;
            };
            // Switch targets require payload decoding; decline those methods.
            if ir
                .instructions
                .iter()
                .any(|i| matches!(i.opcode, 0x2b | 0x2c))
            {
                continue;
            }
            let mut targets: HashSet<usize> = ir
                .instructions
                .iter()
                .filter_map(|i| i.branch_target)
                .collect();
            targets.extend(
                code.try_regions
                    .iter()
                    .flat_map(|r| r.catches.iter().map(|(_, pc)| *pc as usize)),
            );
            let mut allocations: Vec<Option<Arc<str>>> = vec![None; code.registers as usize];
            for instruction in &ir.instructions {
                if targets.contains(&instruction.pc) {
                    allocations.fill(None);
                }
                let copy = if matches!(instruction.opcode, 0x07..=0x09) {
                    instruction
                        .reads
                        .first()
                        .and_then(|r| allocations[r.register as usize].clone())
                } else {
                    None
                };
                if matches!(instruction.opcode, 0x70 | 0x76) {
                    if let Some(receiver) = instruction.reads.first()
                        && let Some(allocated) = allocations[receiver.register as usize].clone()
                        && let Some(reference) = instruction.reference
                        && let Some(&(owner, proto, name)) =
                            class.symbols.methods.get(reference.index as usize)
                        && class
                            .symbols
                            .strings
                            .get(name as usize)
                            .is_some_and(|s| s == "<init>")
                        && let (Some(parent), Some((ret, args))) = (
                            class.symbols.types.get(owner as usize),
                            class.symbols.protos.get(proto as usize),
                        )
                        && ret.as_ref() == "V"
                        && !args.is_empty()
                        && let Some(leaf) = owners.get(allocated.as_ref())
                        && !duplicates.contains(parent.as_ref())
                        && let Some(immediate) = leaf.superclass.as_ref()
                        && !duplicates.contains(immediate.as_ref())
                        && let Some(parent_class) = owners.get(immediate.as_ref())
                        && let Some(target) = forwarding_target(leaf, parent_class, args)
                        && (immediate == parent
                            || transparent_forwarding_path(
                                parent_class,
                                target,
                                parent,
                                args,
                                &owners,
                                &duplicates,
                            ))
                    {
                        result.entry(allocated.clone()).or_default().insert(
                            args.join(""),
                            RecoveredConstructor {
                                parent: immediate.clone(),
                                invoked_owner: parent.clone(),
                                parameters: args.clone(),
                                thrown_types: target.thrown_types.clone(),
                            },
                        );
                    }
                    if let Some(receiver) = instruction.reads.first()
                        && let Some(allocated) = allocations[receiver.register as usize].clone()
                    {
                        for slot in &mut allocations {
                            if slot.as_ref() == Some(&allocated) {
                                *slot = None;
                            }
                        }
                    }
                }
                for write in &instruction.writes {
                    allocations[write.register as usize] = None;
                    if write.kind == ValueKind::Wide64 {
                        allocations[write.register as usize + 1] = None;
                    }
                }
                if instruction.opcode == 0x22 {
                    if let Some(reference) = instruction.reference
                        && relevant.contains(&(reference.index as usize))
                        && let Some(write) = instruction.writes.first()
                    {
                        allocations[write.register as usize] =
                            class.symbols.types.get(reference.index as usize).cloned();
                    }
                } else if let Some(value) = copy
                    && let Some(write) = instruction.writes.first()
                {
                    allocations[write.register as usize] = Some(value);
                }
                if instruction.branch_target.is_some()
                    || matches!(instruction.opcode, 0x0e..=0x11 | 0x27)
                {
                    allocations.fill(None);
                }
            }
        }
    }
    result
        .into_iter()
        .map(|(owner, values)| (owner, values.into_values().collect()))
        .collect()
}

fn forwarding_target<'a>(
    leaf: &DexClass,
    parent: &'a DexClass,
    args: &[Arc<str>],
) -> Option<&'a DexMethod> {
    let package = |name: &str| {
        name.rsplit_once('/')
            .map_or("", |(package, _)| package)
            .to_string()
    };
    let same_package = package(&leaf.descriptor) == package(&parent.descriptor);
    if parent.access_flags & 1 == 0 && !same_package {
        return None;
    }
    let mut matches = parent
        .methods
        .iter()
        .filter(|m| m.name.as_ref() == "<init>" && m.parameters == args);
    let target = matches.next()?;
    if matches.next().is_some()
        || target.return_type.as_ref() != "V"
        || target.declaring_type != parent.descriptor
        || target.code.is_none()
        || target.access_flags & (0x8 | 0x100 | 0x400) != 0
        || !match target.access_flags & 7 {
            1 | 4 => true,
            0 => same_package,
            _ => false,
        }
    {
        return None;
    }
    Some(target)
}

// A constructor erased through an intermediate class may be restored only when
// every retained constructor on that path is an exact argument-forwarding call.
fn transparent_forwarding_path<'a>(
    mut class: &'a DexClass,
    mut method: &'a DexMethod,
    invoked: &str,
    args: &[Arc<str>],
    owners: &HashMap<&str, &'a DexClass>,
    duplicates: &HashSet<&str>,
) -> bool {
    let mut visited = HashSet::new();
    for _ in 0..32 {
        if !visited.insert(class.descriptor.as_ref()) {
            return false;
        }
        let Some(parent) = class.superclass.as_deref() else {
            return false;
        };
        let Some(code) = &method.code else {
            return false;
        };
        if !code.try_regions.is_empty()
            || code.ins > code.registers
            || usize::from(code.ins)
                != 1 + args
                    .iter()
                    .map(|ty| {
                        if matches!(ty.as_ref(), "J" | "D") {
                            2
                        } else {
                            1
                        }
                    })
                    .sum::<usize>()
        {
            return false;
        }
        let Ok(ir) = DecodedMethod::decode(code) else {
            return false;
        };
        let mut identity: Vec<Option<usize>> = vec![None; code.registers as usize];
        let base = (code.registers - code.ins) as usize;
        for (n, slot) in identity.iter_mut().skip(base).enumerate() {
            *slot = Some(n);
        }
        let mut delegated = false;
        for (i, insn) in ir.instructions.iter().enumerate() {
            match insn.opcode {
                0x01..=0x09 if !delegated => {
                    let Some(read) = insn.reads.first() else {
                        return false;
                    };
                    let Some(write) = insn.writes.first() else {
                        return false;
                    };
                    identity[write.register as usize] = identity[read.register as usize];
                    if write.kind == ValueKind::Wide64 {
                        identity[write.register as usize + 1] =
                            identity[read.register as usize + 1];
                    }
                }
                0x70 | 0x76 if !delegated => {
                    let w = &code.instructions;
                    let pc = insn.pc;
                    let a = (w[pc] >> 8) as usize;
                    let Some(&(owner, proto, name)) = class.symbols.methods.get(w[pc + 1] as usize)
                    else {
                        return false;
                    };
                    if class.symbols.types[owner as usize].as_ref() != parent
                        || class.symbols.strings[name as usize] != "<init>"
                        || class.symbols.protos[proto as usize].0.as_ref() != "V"
                        || class.symbols.protos[proto as usize].1 != args
                    {
                        return false;
                    }
                    let rr: Vec<usize> = if insn.opcode == 0x76 {
                        (w[pc + 2] as usize..w[pc + 2] as usize + a).collect()
                    } else {
                        let p = w[pc + 2];
                        let r = [
                            (p & 15) as usize,
                            ((p >> 4) & 15) as usize,
                            ((p >> 8) & 15) as usize,
                            ((p >> 12) & 15) as usize,
                            a & 15,
                        ];
                        if a >> 4 > 5 {
                            return false;
                        }
                        r[..a >> 4].to_vec()
                    };
                    if rr.len() != code.ins as usize
                        || rr
                            .iter()
                            .enumerate()
                            .any(|(n, r)| identity.get(*r) != Some(&Some(n)))
                    {
                        return false;
                    }
                    delegated = true;
                }
                0x0e if delegated && i + 1 == ir.instructions.len() => {}
                _ => return false,
            }
        }
        if !delegated || ir.instructions.last().is_none_or(|i| i.opcode != 0x0e) {
            return false;
        }
        if parent == invoked {
            return true;
        }
        if duplicates.contains(parent) {
            return false;
        }
        let Some(next) = owners.get(parent) else {
            return false;
        };
        let Some(next_method) = forwarding_target(class, next, args) else {
            return false;
        };
        class = next;
        method = next_method;
    }
    false
}

/// Classes for which an unreachable Java `super()` is still statically legal.
/// This does not claim the parent's constructor is effect free; the caller must
/// prove that no execution reaches the delegation.
pub(super) fn accessible_noarg_superclasses(classes: &[&DexClass]) -> HashSet<Arc<str>> {
    let mut owners = HashMap::new();
    let mut duplicates = HashSet::new();
    for &class in classes {
        if owners.insert(class.descriptor.as_ref(), class).is_some() {
            duplicates.insert(class.descriptor.as_ref());
        }
    }
    classes
        .iter()
        .filter_map(|class| {
            if duplicates.contains(class.descriptor.as_ref()) {
                return None;
            }
            let parent = class.superclass.as_deref()?;
            if duplicates.contains(parent) {
                return None;
            }
            let parent_class = owners.get(parent)?;
            let target = forwarding_target(class, parent_class, &[])?;
            target
                .thrown_types
                .is_empty()
                .then(|| class.descriptor.clone())
        })
        .collect()
}
