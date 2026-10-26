//! Compose one observed erased leaf with an existing exact direct SDK forwarder.
//! No reachability-only permission, recursive new dependency, or parent mutation.
use super::*;
use crate::{
    native_calls::{BoundCalls, CallKind, CallTarget},
    native_dex::DexCode,
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::collections::BTreeSet;

type ObservedTarget = (Arc<str>, Vec<Arc<str>>);
type AllocationObservation = (Arc<str>, Arc<str>, Vec<Arc<str>>);
type ObservationKey = (Arc<str>, String);

fn package(owner: &str) -> &str {
    owner.rsplit_once('/').map_or("", |(p, _)| p)
}
fn shape(class: &DexClass, leaf: bool) -> bool {
    class.fields.len() <= 256
        && class.methods.len() <= 4096
        && class.access_flags & !0x11 == 0
        && (!leaf || class.access_flags & 1 != 0)
        && (leaf || class.access_flags & 0x10 == 0)
        && independent_constructor_owner(class)
        && !class.methods.iter().any(|m| m.name.as_ref() == "<init>")
        && {
            let mut names = HashSet::new();
            class.fields.iter().all(|f| {
                f.declaring_type == class.descriptor
                    && names.insert(f.name.as_ref())
                    && f.is_static == (f.access_flags & 8 != 0)
                    && f.access_flags & !0x10df == 0
                    && f.access_flags & 0x50 != 0x50
                    && matches!(f.access_flags & 7, 0 | 1 | 2 | 4)
                    && (f.is_static
                        || leaf
                            && f.access_flags & (0x10 | 0x40) == 0
                            && matches!(
                                f.field_type.as_ref(),
                                "Z" | "B" | "S" | "C" | "I" | "J" | "F" | "D"
                            ))
            })
        }
}
fn nonempty_sdk_parent(owner: &str) -> bool {
    static OWNERS: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    OWNERS
        .get_or_init(|| {
            platform_constructor_facts()
                .constructors
                .iter()
                .filter_map(|(signature, (access, throws))| {
                    let (owner, tail) = signature.split_once("-><init>(")?;
                    (tail != ")V"
                        && access & 5 != 0
                        && access & (8 | 0x80 | 0x100 | 0x400) == 0
                        && throws.is_empty())
                    .then(|| owner.to_owned())
                })
                .collect()
        })
        .contains(owner)
}
pub(super) fn candidate(
    leaf: &DexClass,
    owners: &HashMap<&str, &DexClass>,
    duplicates: &HashSet<&str>,
) -> bool {
    if !shape(leaf, true) || duplicates.contains(leaf.descriptor.as_ref()) {
        return false;
    }
    let Some(parent) = leaf.superclass.as_deref().and_then(|p| owners.get(p)) else {
        return false;
    };
    shape(parent, false)
        && parent.descriptor != leaf.descriptor
        && !duplicates.contains(parent.descriptor.as_ref())
        && package(&leaf.descriptor) == package(&parent.descriptor)
        && parent.superclass.as_deref().is_some_and(|sdk| {
            !owners.contains_key(sdk) && !duplicates.contains(sdk) && nonempty_sdk_parent(sdk)
        })
}
fn sdk_owner_unshadowed(
    owner: &str,
    owners: &HashMap<&str, &DexClass>,
    duplicates: &HashSet<&str>,
) -> bool {
    let mut current = owner;
    let mut seen = HashSet::new();
    loop {
        if seen.len() == 32
            || !seen.insert(current)
            || owners.contains_key(current)
            || duplicates.contains(current)
        {
            return false;
        }
        let Some(binary) = current
            .strip_prefix('L')
            .and_then(|x| x.strip_suffix(';'))
            .map(|x| x.replace('/', "."))
        else {
            return false;
        };
        if binary.contains('$') && platform_member_type(&binary).is_none() {
            return false;
        }
        if let Some((_, _, chain)) = platform_member_type(&binary)
            && chain
                .iter()
                .any(|p| owners.contains_key(p.as_str()) || duplicates.contains(p.as_str()))
        {
            return false;
        }
        if current == "Ljava/lang/Object;" {
            return true;
        }
        let Some(next) = super::super::unloaded_platform_superclass(current) else {
            return false;
        };
        current = next;
    }
}
fn valid_inputs(method: &DexMethod, code: &DexCode) -> bool {
    let words =
        method
            .parameters
            .iter()
            .try_fold(usize::from(method.access_flags & 8 == 0), |n, p| {
                n.checked_add(if matches!(p.as_ref(), "J" | "D") {
                    2
                } else {
                    1
                })
            });
    words == Some(usize::from(code.ins))
        && code.ins <= code.registers
        && code.instructions.len() <= 4096
        && code.registers <= 256
        && code.outs <= 256
        && usize::from(code.tries) == code.try_regions.len()
        && code.try_regions.len() <= 16
}
enum Origin {
    Allocation { pc: usize, owner: Arc<str> },
    This,
}

fn charge(budget: &mut usize, amount: usize) -> Option<()> {
    *budget = budget.checked_sub(amount)?;
    Some(())
}

fn origin(
    class: &DexClass,
    method: &DexMethod,
    ir: &HashMap<usize, &crate::native_ir::Instruction>,
    ssa: &SsaMethod,
    words: &HashMap<usize, &crate::native_ssa::SsaInstruction>,
    mut value: usize,
    budget: &mut usize,
) -> Option<Origin> {
    let mut seen = HashSet::new();
    for _ in 0..32 {
        if !seen.insert(value) {
            return None;
        }
        charge(budget, 1)?;
        let definition = ssa.definitions.get(value)?;
        match definition.kind {
            DefinitionKind::Parameter => {
                let code = method.code.as_ref()?;
                return (method.name.as_ref() == "<init>"
                    && method.access_flags & 8 == 0
                    && definition.register == code.registers - code.ins)
                    .then_some(Origin::This);
            }
            DefinitionKind::Instruction { pc, word: 0, .. } => {
                let instruction = ir.get(&pc)?;
                if instruction.opcode == 0x22 {
                    let reference = instruction.reference?;
                    if reference.kind != crate::native_ir::PoolKind::Type {
                        return None;
                    }
                    return Some(Origin::Allocation {
                        pc,
                        owner: class.symbols.types.get(reference.index as usize)?.clone(),
                    });
                }
                if !matches!(instruction.opcode, 0x07..=0x09) {
                    return None;
                }
                let read = words.get(&pc)?.reads.first()?;
                if read.kind != ValueKind::Reference || read.words.len() != 1 {
                    return None;
                }
                value = read.words[0];
            }
            _ => return None,
        }
    }
    None
}

/// Establish one linear fresh lifetime for the selected allocation value.
/// Every SSA operand in the allocation/initialization interval is examined:
/// the identity may flow only through a one-word move-object and may be read
/// only once as the exact constructor receiver at the endpoint.
#[allow(clippy::too_many_arguments)]
fn exact_initialization_interval(
    decoded: &HashMap<usize, &crate::native_ir::Instruction>,
    ssa: &SsaMethod,
    words: &HashMap<usize, &crate::native_ssa::SsaInstruction>,
    allocation_pc: usize,
    constructor_pc: usize,
    block: usize,
    fresh: usize,
    receiver: usize,
    budget: &mut usize,
) -> Option<()> {
    let instructions = &ssa.graph.blocks.get(block)?.instructions;
    let allocation_index = instructions.iter().position(|pc| *pc == allocation_pc)?;
    let constructor_index = instructions.iter().position(|pc| *pc == constructor_pc)?;
    if allocation_index >= constructor_index {
        return None;
    }
    let mut aliases = BTreeSet::new();
    for &pc in instructions.get(allocation_index..=constructor_index)? {
        charge(budget, 1)?;
        let instruction = words.get(&pc)?;
        let mut alias_reads = 0usize;
        for operand in &instruction.reads {
            charge(budget, 1)?;
            for value in &operand.words {
                charge(budget, 1)?;
                alias_reads = alias_reads.checked_add(usize::from(aliases.contains(value)))?;
            }
        }
        for operand in &instruction.writes {
            charge(budget, 1usize.checked_add(operand.words.len())?)?;
        }
        if pc == allocation_pc {
            if decoded.get(&pc)?.opcode != 0x22
                || !instruction.reads.is_empty()
                || instruction.writes.len() != 1
                || instruction.writes[0].kind != ValueKind::Reference
                || instruction.writes[0].words.as_slice() != [fresh]
            {
                return None;
            }
            aliases.insert(fresh);
            continue;
        }
        if pc == constructor_pc {
            // Exactly one alias read means the receiver consumes the identity
            // and no ordinary or wide constructor argument reuses any alias.
            if !aliases.contains(&receiver) || alias_reads != 1 {
                return None;
            }
            continue;
        }
        if alias_reads == 0 {
            continue;
        }
        if !matches!(decoded.get(&pc)?.opcode, 0x07..=0x09)
            || instruction.reads.len() != 1
            || instruction.writes.len() != 1
            || instruction.reads[0].kind != ValueKind::Reference
            || instruction.writes[0].kind != ValueKind::Reference
            || instruction.reads[0].words.len() != 1
            || instruction.writes[0].words.len() != 1
            || alias_reads != 1
        {
            return None;
        }
        aliases.insert(instruction.writes[0].words[0]);
    }
    Some(())
}

pub(super) struct Observations {
    // None is irrevocable conflict/unknown for this exact owner/prototype.
    entries: BTreeMap<ObservationKey, Option<ObservedTarget>>,
    invalid_owners: HashSet<Arc<str>>,
    remaining_work: usize,
    methods: usize,
    words: usize,
    exhausted: bool,
}
impl Default for Observations {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            invalid_owners: HashSet::new(),
            remaining_work: 20_000_000,
            methods: 0,
            words: 0,
            exhausted: false,
        }
    }
}
impl Observations {
    pub(super) fn invalidate(&mut self, owner: Arc<str>) {
        self.invalid_owners.insert(owner);
    }
    fn observe(&mut self, allocated: Arc<str>, invoked: Arc<str>, args: Vec<Arc<str>>) {
        let key = (allocated, args.join(""));
        let entry = Some((invoked, args));
        self.entries
            .entry(key)
            .and_modify(|old| {
                if old.as_ref() != entry.as_ref() {
                    *old = None;
                }
            })
            .or_insert(entry);
    }
    pub(super) fn collect(
        &mut self,
        class: &DexClass,
        method: &DexMethod,
        decoded: &DecodedMethod,
        audit: &HashSet<&str>,
    ) {
        let allocations: BTreeMap<_, _> = decoded
            .instructions
            .iter()
            .filter(|i| i.opcode == 0x22)
            .filter_map(|i| {
                i.reference
                    .and_then(|r| class.symbols.types.get(r.index as usize))
                    .filter(|owner| audit.contains(owner.as_ref()))
                    .map(|owner| (i.pc, owner.clone()))
            })
            .collect();
        if allocations.is_empty() || self.exhausted {
            return;
        }
        let Some(code) = method.code.as_ref() else {
            return;
        };
        self.methods += 1;
        self.words = self.words.saturating_add(code.instructions.len());
        let limit = code
            .instructions
            .len()
            .saturating_mul(usize::from(code.registers) + 1)
            .saturating_mul(16)
            .clamp(32_768, 2_000_000);
        let reserve = limit * 4;
        if self.methods > 256 || self.words > 65_536 || self.remaining_work < reserve {
            self.exhausted = true;
            return;
        }
        self.remaining_work -= reserve;
        let prove = || -> Option<Vec<AllocationObservation>> {
            valid_inputs(method, code).then_some(())?;
            if code
                .try_regions
                .iter()
                .try_fold(0usize, |n, r| n.checked_add(r.catches.len()))?
                > 64
            {
                return None;
            }
            let cfg = crate::native_cfg::ControlFlowGraph::build(code).ok()?;
            let calls = BoundCalls::bind(code, decoded, &class.symbols).ok()?;
            let ssa =
                SsaMethod::build_with_local_and_dominator_limits(code, decoded, &cfg, limit, limit)
                    .ok()?;
            let values = crate::native_call_values::SsaCalls::bind(&calls, &ssa).ok()?;
            let dom =
                crate::native_dominators::DominatorTree::compute_with_work_limit(&ssa.graph, limit)
                    .ok()?;
            let ir: HashMap<_, _> = decoded.instructions.iter().map(|i| (i.pc, i)).collect();
            let words: HashMap<_, _> = ssa.instructions.iter().map(|i| (i.pc, i)).collect();
            let value_calls: HashMap<_, _> = values.calls.iter().map(|i| (i.pc, i)).collect();
            let mut budget = limit;
            let mut initialized = BTreeSet::new();
            let mut observed = Vec::new();
            for call in &calls.calls {
                let CallTarget::Method {
                    declaring_type,
                    name,
                    ..
                } = &call.target
                else {
                    continue;
                };
                if name.as_ref() != "<init>" {
                    continue;
                }
                if call.kind != CallKind::Direct || call.return_type.as_ref() != "V" {
                    return None;
                }
                let argument_word_count = call
                    .arguments
                    .iter()
                    .map(|a| {
                        if matches!(a.descriptor.as_ref(), "J" | "D") {
                            2
                        } else {
                            1
                        }
                    })
                    .sum::<usize>();
                if argument_word_count + 1 > usize::from(code.outs) {
                    return None;
                }
                let call_values = value_calls.get(&call.pc)?;
                let receiver = call_values.receiver.as_ref()?;
                if receiver.words.len() != 1 {
                    return None;
                }
                match origin(
                    class,
                    method,
                    &ir,
                    &ssa,
                    &words,
                    receiver.words[0],
                    &mut budget,
                )? {
                    Origin::This => continue,
                    Origin::Allocation { pc, owner } => {
                        if !audit.contains(owner.as_ref()) {
                            continue;
                        }
                        if allocations.get(&pc) != Some(&owner) || !initialized.insert(pc) {
                            return None;
                        }
                        // SSA copy definitions may be later than allocation;
                        // use the original allocation instruction's SSA block.
                        let fresh = words.get(&pc)?.writes.first()?.words.first()?;
                        let DefinitionKind::Instruction {
                            block: allocation_block,
                            ..
                        } = ssa.definitions.get(*fresh)?.kind
                        else {
                            return None;
                        };
                        let call_block = *ssa.graph.block_at.get(&call.pc)?;
                        if allocation_block != call_block
                            || !dom.dominates(allocation_block, call_block)
                            || pc >= call.pc
                        {
                            return None;
                        }
                        exact_initialization_interval(
                            &ir,
                            &ssa,
                            &words,
                            pc,
                            call.pc,
                            allocation_block,
                            *fresh,
                            receiver.words[0],
                            &mut budget,
                        )?;
                        observed.push((
                            owner,
                            declaring_type.clone(),
                            call.arguments
                                .iter()
                                .map(|a| a.descriptor.clone())
                                .collect(),
                        ));
                    }
                }
            }
            if initialized.len() != allocations.len() {
                return None;
            }
            Some(observed)
        };
        match prove() {
            Some(observed) => {
                for (owner, invoked, args) in observed {
                    self.observe(owner, invoked, args);
                }
            }
            None => {
                for owner in allocations.values() {
                    self.invalidate(owner.clone());
                }
            }
        }
    }
    pub(super) fn commit(
        self,
        owners: &HashMap<&str, &DexClass>,
        duplicates: &HashSet<&str>,
        result: &mut HashMap<Arc<str>, BTreeMap<String, RecoveredConstructor>>,
    ) {
        if self.exhausted {
            return;
        }
        let mut proposed = Vec::new();
        // Validate against the immutable completed old result. New records are
        // leaves only; none can authorize another new record or mutate parents.
        for ((owner, key), observation) in &self.entries {
            if self.invalid_owners.contains(owner) {
                continue;
            }
            let Some((invoked, args)) = observation else {
                continue;
            };
            if args.is_empty() || args.len() > 16 {
                continue;
            }
            let Some(leaf) = owners
                .get(owner.as_ref())
                .filter(|c| candidate(c, owners, duplicates))
            else {
                continue;
            };
            let Some(parent) = leaf.superclass.as_deref().and_then(|p| owners.get(p)) else {
                continue;
            };
            if parent.superclass.as_deref() != Some(invoked.as_ref())
                || !sdk_owner_unshadowed(invoked, owners, duplicates)
            {
                continue;
            }
            let Some((access, throws)) = platform_constructor_facts()
                .constructors
                .get(&format!("{invoked}-><init>({})V", args.join("")))
            else {
                continue;
            };
            if access & 5 == 0 || access & (8 | 0x80 | 0x100 | 0x400) != 0 || !throws.is_empty() {
                continue;
            }
            if self.invalid_owners.contains(&parent.descriptor)
                || self.entries.get(&(parent.descriptor.clone(), key.clone()))
                    != Some(&Some((invoked.clone(), args.clone())))
            {
                continue;
            }
            let Some(existing) = result
                .get(&parent.descriptor)
                .and_then(|entries| entries.get(key))
            else {
                continue;
            };
            if existing.parent != *invoked
                || existing.invoked_owner != *invoked
                || existing.parameters != *args
                || !existing.thrown_types.is_empty()
                || !existing.strict_allocation_order
                || !existing.observed_allocation
            {
                continue;
            }
            if result
                .get(owner)
                .is_some_and(|entries| entries.contains_key(key))
            {
                continue;
            }
            proposed.push((
                owner.clone(),
                key.clone(),
                RecoveredConstructor {
                    source_bridge: false,
                    access_flags: 1,
                    parent: parent.descriptor.clone(),
                    invoked_owner: invoked.clone(),
                    parameters: args.clone(),
                    thrown_types: vec![],
                    strict_allocation_order: true,
                    observed_allocation: true,
                    observed_super: false,
                },
            ));
        }
        for (owner, key, record) in proposed {
            result.entry(owner).or_default().insert(key, record);
        }
    }
}
