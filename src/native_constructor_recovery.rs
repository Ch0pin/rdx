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
    pub strict_allocation_order: bool,
    pub observed_allocation: bool,
    pub observed_super: bool,
}

#[derive(serde::Deserialize)]
struct PlatformConstructors {
    constructors: HashMap<String, (u32, Vec<String>)>,
}

fn platform_constructor_facts() -> &'static PlatformConstructors {
    static FACTS: std::sync::OnceLock<PlatformConstructors> = std::sync::OnceLock::new();
    FACTS.get_or_init(|| {
        serde_json::from_str(include_str!("../data/android-35-constructors.json"))
            .expect("embedded Android constructor metadata is valid")
    })
}

// Candidate enumeration needs an eligible parent, not an unobserved noarg
// overload. The selected delegation still proves its exact access/prototype.
fn platform_parent_has_constructor(owner: &str) -> bool {
    static OWNERS: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    OWNERS
        .get_or_init(|| {
            platform_constructor_facts()
                .constructors
                .iter()
                .filter_map(|(signature, (access, _))| {
                    if access & 5 == 0 || access & (8 | 0x100 | 0x400) != 0 {
                        return None;
                    }
                    signature
                        .split_once("-><init>(")
                        .map(|(parent, _)| parent.to_owned())
                })
                .collect()
        })
        .contains(owner)
}

fn platform_constructor(owner: &str, args: &[Arc<str>]) -> Option<Vec<Arc<str>>> {
    let facts = platform_constructor_facts();
    let key = format!("{owner}-><init>({})V", args.join(""));
    let (access, thrown) = facts.constructors.get(&key)?;
    (access & 5 != 0 && access & (8 | 0x100 | 0x400) == 0)
        .then(|| thrown.iter().map(|ty| Arc::from(ty.as_str())).collect())
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
                && !class
                    .fields
                    .iter()
                    .any(|f| !f.is_static && f.access_flags & 0x10 != 0)
                && class.superclass.as_deref().is_some_and(|parent| {
                    owners.get(parent).map_or_else(
                        || platform_parent_has_constructor(parent),
                        |parent| {
                            parent
                                .methods
                                .iter()
                                .any(|method| method.name.as_ref() == "<init>")
                        },
                    )
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
                        && let Some(leaf) = owners.get(allocated.as_ref())
                        && !leaf
                            .methods
                            .iter()
                            .any(|m| m.name.as_ref() == "<init>" && m.parameters == *args)
                        && !duplicates.contains(parent.as_ref())
                        && let Some(immediate) = leaf.superclass.as_ref()
                        && !duplicates.contains(immediate.as_ref())
                        && let Some(thrown_types) = recovered_forwarding_target(
                            leaf,
                            immediate,
                            parent,
                            args,
                            &owners,
                            &duplicates,
                        )
                    {
                        result.entry(allocated.clone()).or_default().insert(
                            args.join(""),
                            RecoveredConstructor {
                                parent: immediate.clone(),
                                invoked_owner: parent.clone(),
                                parameters: args.clone(),
                                thrown_types,
                                observed_allocation: true,
                                observed_super: false,
                                strict_allocation_order: args.is_empty()
                                    || leaf.methods.iter().any(|m| m.name.as_ref() == "<init>")
                                    || !owners.contains_key(immediate.as_ref()),
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
    recover_observed_super_forwarders(classes, &owners, &duplicates, &mut result);
    result
        .into_iter()
        .map(|(owner, values)| (owner, values.into_values().collect()))
        .collect()
}

fn recovered_forwarding_target(
    leaf: &DexClass,
    immediate: &str,
    invoked: &str,
    args: &[Arc<str>],
    owners: &HashMap<&str, &DexClass>,
    duplicates: &HashSet<&str>,
) -> Option<Vec<Arc<str>>> {
    if let Some(parent) = owners.get(immediate) {
        let target = forwarding_target(leaf, parent, args)?;
        (immediate == invoked
            || transparent_forwarding_path(parent, target, invoked, args, owners, duplicates))
        .then(|| target.thrown_types.clone())
    } else if immediate == invoked {
        platform_constructor(immediate, args)
    } else {
        None
    }
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

// Insert before result.into_iter() in constructor_recovery::recover. This is
// separate from allocation candidates: an abstract parent may have a Java ctor.
fn recover_observed_super_forwarders(
    classes: &[&DexClass],
    owners: &HashMap<&str, &DexClass>,
    duplicates: &HashSet<&str>,
    result: &mut HashMap<Arc<str>, BTreeMap<String, RecoveredConstructor>>,
) {
    use crate::{
        native_calls::{BoundCalls, CallKind, CallTarget},
        native_cfg::ControlFlowGraph,
        native_ssa::SsaMethod,
    };
    let package = |name: &str| name.rsplit_once('/').map_or("", |(p, _)| p).to_string();
    let mut remaining = 2_000_000usize;
    let mut observed: HashMap<Arc<str>, BTreeMap<String, RecoveredConstructor>> = HashMap::new();
    let mut conflicts = HashSet::new();
    for &child in classes {
        if duplicates.contains(child.descriptor.as_ref())
            || child.access_flags & (0x200 | 0x4000) != 0
        {
            continue;
        }
        let Some(parent_name) = child.superclass.as_deref() else {
            continue;
        };
        let Some(parent) = owners.get(parent_name).copied() else {
            continue;
        };
        if duplicates.contains(parent_name)
            || parent.access_flags & (0x200 | 0x4000 | 0x10) != 0
            || !matches!(parent.access_flags & 7, 0 | 1)
            || parent.descriptor.contains('$')
            || parent.access_flags & 1 == 0
                && package(&parent.descriptor) != package(&child.descriptor)
            || parent
                .fields
                .iter()
                .any(|f| !f.is_static && f.access_flags & 0x10 != 0)
        {
            continue;
        }
        let Some(immediate) = parent.superclass.as_ref() else {
            continue;
        };
        if duplicates.contains(immediate.as_ref())
            || immediate.as_ref() == "Ljava/lang/Enum;"
            || owners
                .get(immediate.as_ref())
                .is_some_and(|base| base.access_flags & (0x200 | 0x4000) != 0)
        {
            continue;
        }
        for method in &child.methods {
            if method.name.as_ref() != "<init>"
                || method.declaring_type != child.descriptor
                || method.return_type.as_ref() != "V"
                || method.access_flags & (8 | 0x100 | 0x400) != 0
            {
                continue;
            }
            let Some(code) = method.code.as_ref() else {
                continue;
            };
            if code.instructions.len() > 8192 || code.instructions.len() > remaining {
                continue;
            }
            // Decode only constructors which mention a potentially erased ancestor
            // call, not every constructor in the APK.
            if !code
                .instructions
                .iter()
                .any(|w| matches!(*w as u8, 0x70 | 0x76))
            {
                continue;
            }
            let Ok(ir) = DecodedMethod::decode(code) else {
                continue;
            };
            let Ok(bound) = BoundCalls::bind(code, &ir, &child.symbols) else {
                continue;
            };
            if !bound.calls.iter().any(|call| matches!(&call.target,CallTarget::Method{declaring_type,name,..} if name.as_ref()=="<init>" && declaring_type.as_ref()!=parent_name && declaring_type.as_ref()!=child.descriptor.as_ref())) { continue; }
            remaining -= code.instructions.len();
            let Ok(cfg) = ControlFlowGraph::build(code) else {
                continue;
            };
            let Ok(ssa) = SsaMethod::build_with_work_limit(code, &ir, &cfg, 2_000_000) else {
                continue;
            };
            let this_calls: Vec<_> = bound
                .calls
                .iter()
                .filter(|call| {
                    matches!(&call.target,CallTarget::Method{name,..} if name.as_ref()=="<init>")
                        && call.receiver.as_ref().is_some_and(|receiver| {
                            super::this_receiver_identity::proves_read(
                                method,
                                &ir,
                                &ssa,
                                call.pc,
                                usize::from(receiver.register),
                            )
                        })
                })
                .collect();
            // First tranche: exactly one complete entry-this delegation domain.
            // Allocation constructors are independent; alternate this delegations
            // are declined rather than merged or inferred.
            if this_calls.len() != 1 {
                continue;
            }
            let call = this_calls[0];
            if !valid_delegation_path(child, method, &ir, &ssa, call.pc) {
                continue;
            }
            let CallTarget::Method {
                declaring_type: invoked,
                ..
            } = &call.target
            else {
                continue;
            };
            if call.kind != CallKind::Direct
                || call.return_type.as_ref() != "V"
                || invoked.as_ref() == parent_name
                || invoked == &child.descriptor
            {
                continue;
            }
            let args: Vec<_> = call
                .arguments
                .iter()
                .map(|arg| arg.descriptor.clone())
                .collect();
            if parent
                .methods
                .iter()
                .any(|m| m.name.as_ref() == "<init>" && m.parameters == args)
            {
                continue;
            }
            let Some(thrown_types) =
                recovered_forwarding_target(parent, immediate, invoked, &args, owners, duplicates)
            else {
                continue;
            };
            let key = args.join("");
            let identity = (parent.descriptor.clone(), key.clone());
            if conflicts.contains(&identity) {
                continue;
            }
            let record = RecoveredConstructor {
                parent: immediate.clone(),
                invoked_owner: invoked.clone(),
                parameters: args,
                thrown_types,
                strict_allocation_order: true,
                observed_allocation: false,
                observed_super: true,
            };
            let entries = observed.entry(parent.descriptor.clone()).or_default();
            if entries.get(&key).is_some_and(|old| {
                old.parent != record.parent
                    || old.invoked_owner != record.invoked_owner
                    || old.thrown_types != record.thrown_types
            }) {
                entries.remove(&key);
                conflicts.insert(identity);
            } else {
                entries.insert(key, record);
            }
        }
    }
    for (owner, entries) in observed {
        for (key, record) in entries {
            let target = result.entry(owner.clone()).or_default();
            if let Some(old) = target.get_mut(&key) {
                // Preserve existing allocation permissions. Inconsistent new
                // observations never overwrite historical recovery metadata.
                if old.parent == record.parent
                    && old.invoked_owner == record.invoked_owner
                    && old.thrown_types == record.thrown_types
                {
                    old.observed_super = true;
                }
            } else {
                target.insert(key, record);
            }
        }
    }
}

fn valid_delegation_path(
    class: &DexClass,
    method: &DexMethod,
    ir: &DecodedMethod,
    ssa: &crate::native_ssa::SsaMethod,
    pc: usize,
) -> bool {
    let Some(code) = method.code.as_ref() else {
        return false;
    };
    let Some(&block) = ssa.graph.block_at.get(&pc) else {
        return false;
    };
    if !ssa.reachable.get(block).copied().unwrap_or(false)
        || code
            .try_regions
            .iter()
            .any(|r| r.start as usize <= pc && pc < r.end as usize)
    {
        return false;
    }
    let Ok(dom) =
        crate::native_dominators::DominatorTree::compute_with_work_limit(&ssa.graph, 2_000_000)
    else {
        return false;
    };
    // A static instruction appearing once can still execute repeatedly. Reject
    // any normal/exceptional successor path returning to its invocation block.
    let mut pending: Vec<_> = ssa.graph.blocks[block]
        .successors
        .iter()
        .map(|edge| edge.target)
        .collect();
    let mut seen = HashSet::new();
    let mut cycle_budget = 65536;
    while let Some(next) = pending.pop() {
        if cycle_budget == 0 || next >= ssa.graph.blocks.len() {
            return false;
        }
        cycle_budget -= 1;
        if !ssa.reachable[next] {
            continue;
        }
        if next == block {
            return false;
        }
        if !seen.insert(next) {
            continue;
        }
        pending.extend(
            ssa.graph.blocks[next]
                .successors
                .iter()
                .map(|edge| edge.target),
        );
    }
    let raw: HashMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let Some(possible_this) = super::this_receiver_identity::possible_values(method, ir, ssa)
    else {
        return false;
    };
    let mut returns = 0;
    let mut budget = 65536;
    for instruction in &ssa.instructions {
        let Some(&at) = ssa.graph.block_at.get(&instruction.pc) else {
            return false;
        };
        let Some(op) = raw.get(&instruction.pc) else {
            return false;
        };
        let after = dom.dominates(block, at) && (at != block || instruction.pc > pc);
        if matches!(op.opcode, 0x0e..=0x11) {
            if op.opcode != 0x0e || !after {
                return false;
            }
            returns += 1;
        }
        if after {
            continue;
        }
        // Receiver is the first narrow word; no constructor argument may carry
        // uninitialized entry-this, including mixed this/other phis.
        let skip = usize::from(instruction.pc == pc);
        let early_field =
            instruction.pc != pc && own_early_field_write(class, method, ir, ssa, op, instruction);
        let words: Vec<_> = instruction
            .reads
            .iter()
            .flat_map(|r| &r.words)
            .copied()
            .collect();
        for (index, &value) in words.iter().enumerate().skip(skip) {
            if budget == 0 || value >= ssa.definitions.len() {
                return false;
            }
            budget -= 1;
            if possible_this.contains(&value)
                && !(instruction.pc != pc && matches!(op.opcode, 0x07..=0x09))
                && !(early_field && index == 0)
            {
                return false;
            }
        }
    }
    returns > 0
}

fn own_early_field_write(
    class: &DexClass,
    method: &DexMethod,
    ir: &DecodedMethod,
    ssa: &crate::native_ssa::SsaMethod,
    raw: &crate::native_ir::Instruction,
    values: &crate::native_ssa::SsaInstruction,
) -> bool {
    if !matches!(raw.opcode, 0x59..=0x5f)
        || values.reads.len() != 2
        || values.reads[0].words.len() != 1
    {
        return false;
    }
    if !super::this_receiver_identity::proves_value(method, ir, ssa, values.reads[0].words[0]) {
        return false;
    }
    let Some(reference) = raw.reference else {
        return false;
    };
    if reference.kind != crate::native_ir::PoolKind::Field {
        return false;
    }
    let Some(&(owner, ty, name)) = class.symbols.fields.get(reference.index as usize) else {
        return false;
    };
    let (Some(owner), Some(ty), Some(name)) = (
        class.symbols.types.get(owner as usize),
        class.symbols.types.get(ty as usize),
        class.symbols.strings.get(name as usize),
    ) else {
        return false;
    };
    owner == &class.descriptor
        && class
            .fields
            .iter()
            .filter(|field| {
                !field.is_static
                    && field.declaring_type == class.descriptor
                    && field.name.as_ref() == name
                    && field.field_type == *ty
            })
            .count()
            == 1
}
