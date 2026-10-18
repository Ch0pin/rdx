//! Exact observed forwarding constructors removed by DEX optimization.
#[path = "native_constructor_flat_noarg.rs"]
mod flat_noarg;
#[path = "native_constructor_platform_composition.rs"]
mod platform_composition;
use crate::native_dex::{DexClass, DexMethod, DexValue};
use crate::native_ir::{DecodedMethod, ValueKind};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RecoveredConstructor {
    /// Exact Java constructor visibility. Existing recovery categories remain
    /// public; ctorless flat classes use the class's implicit-constructor
    /// visibility and never widen a package-private class API.
    pub access_flags: u32,
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
    member_types: HashMap<String, (String, String, Vec<String>)>,
}

/// Canonical source spelling is distinct from binary/navigation identity.
/// Entries require a complete public static named member chain in the SDK.
pub(crate) fn platform_member_type(
    binary: &str,
) -> Option<(&'static str, &'static str, &'static [String])> {
    let (owner, member, chain) = platform_constructor_facts().member_types.get(binary)?;
    Some((owner.as_str(), member.as_str(), chain.as_slice()))
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

/// A public SDK parent may end a chain of erased implicit no-arg constructors.
/// Any loaded definition of the parent or its enclosing chain takes precedence.
pub(super) fn accessible_platform_noarg_constructor(
    owner: &str,
    owners: &HashMap<&str, &DexClass>,
    duplicates: &HashSet<&str>,
) -> bool {
    if owners.contains_key(owner) || duplicates.contains(owner) {
        return false;
    }
    let mut current = owner;
    let mut seen = HashSet::new();
    loop {
        if seen.len() >= 32
            || !seen.insert(current)
            || owners.contains_key(current)
            || duplicates.contains(current)
        {
            return false;
        }
        if current == "Ljava/lang/Object;" {
            break;
        }
        let Some(parent) = super::unloaded_platform_superclass(current) else {
            return false;
        };
        current = parent;
    }
    let binary = owner
        .strip_prefix('L')
        .and_then(|name| name.strip_suffix(';'))
        .map(|name| name.replace('/', "."));
    if let Some(binary) = binary
        && let Some((_, _, chain)) = platform_member_type(&binary)
        && chain
            .iter()
            .any(|name| owners.contains_key(name.as_str()) || duplicates.contains(name.as_str()))
    {
        return false;
    }
    platform_constructor(owner, &[]).is_some_and(|thrown| thrown.is_empty())
}

/// A named static member has no implicit enclosing-instance constructor input.
/// A dollar in a descriptor alone establishes neither membership nor staticness.
pub(super) fn independent_constructor_owner(class: &DexClass) -> bool {
    if !class.descriptor.contains('$') {
        return true;
    }
    let Some(annotations) = class
        .symbols
        .annotations
        .get(&class.annotations_offset)
        .and_then(|directory| directory.class.as_deref())
    else {
        return false;
    };
    if annotations.iter().any(|annotation| {
        class
            .symbols
            .types
            .get(annotation.type_idx as usize)
            .is_some_and(|ty| ty.as_ref() == "Ldalvik/annotation/EnclosingMethod;")
    }) {
        return false;
    }
    let exact = |name: &str| {
        let mut found = annotations.iter().filter(|annotation| {
            class
                .symbols
                .types
                .get(annotation.type_idx as usize)
                .is_some_and(|ty| ty.as_ref() == name)
        });
        let first = found.next()?;
        (first.visibility == 2 && found.next().is_none()).then_some(first)
    };
    let Some(inner) = exact("Ldalvik/annotation/InnerClass;") else {
        return false;
    };
    let Some(enclosing) = exact("Ldalvik/annotation/EnclosingClass;") else {
        return false;
    };
    let element = |annotation: &crate::native_dex::DexAnnotation, name: &str| {
        let mut values = annotation.elements.iter().filter(|(key, _)| {
            class
                .symbols
                .strings
                .get(*key as usize)
                .is_some_and(|key| key == name)
        });
        let value = values.next()?.1.clone();
        values.next().is_none().then_some(value)
    };
    let (Some(DexValue::Int(flags)), Some(DexValue::String(name)), Some(DexValue::Type(owner))) = (
        element(inner, "accessFlags"),
        element(inner, "name"),
        element(enclosing, "value"),
    ) else {
        return false;
    };
    let flags = flags as u32;
    if flags & 8 == 0
        || !matches!(flags & 7, 0 | 1 | 2 | 4)
        || flags & !(7 | 8 | 0x10 | 0x400 | 0x1000) != 0
        || flags & (7 | 0x10 | 0x400) != class.access_flags & (7 | 0x10 | 0x400)
    {
        return false;
    }
    let (Some(name), Some(owner)) = (
        class.symbols.strings.get(name as usize),
        class.symbols.types.get(owner as usize),
    ) else {
        return false;
    };
    !name.is_empty()
        && owner
            .strip_suffix(';')
            .is_some_and(|owner| class.descriptor.as_ref() == format!("{owner}${name};"))
}

const MAX_LAYERED_NOARG_KEYS: usize = 16_384;
const MAX_LAYERED_NOARG_OBSERVATIONS: usize = 100_000;
const MAX_LAYERED_NOARG_PREFILTER_WORDS: usize = 32_000_000;
const MAX_LAYERED_NOARG_WORK: usize = 8_000_000;

struct LayeredNoargCandidate {
    immediate: Arc<str>,
    skipped: Arc<str>,
    invoked: Arc<str>,
    retained_parameters: HashSet<Vec<Arc<str>>>,
}

struct LayeredNoargObservations {
    invoked: BTreeMap<Arc<str>, Option<Arc<str>>>,
    invalid: HashSet<Arc<str>>,
    keys: usize,
    observations: usize,
    remaining_prefilter_words: usize,
    remaining_work: usize,
    exhausted: bool,
}

fn reserve_layered_work(remaining: &mut usize, instructions: usize, registers: usize) -> bool {
    let Some(work) = instructions.checked_mul(registers.max(1)) else {
        return false;
    };
    let Some(next) = remaining.checked_sub(work) else {
        return false;
    };
    *remaining = next;
    true
}

fn layered_exception_work(code: &crate::native_dex::DexCode) -> Option<usize> {
    code.try_regions.iter().try_fold(0usize, |total, region| {
        let width = usize::try_from(region.end.checked_sub(region.start)?).ok()?;
        let edges = width.checked_mul(region.catches.len().max(1))?;
        total.checked_add(edges)
    })
}

impl LayeredNoargObservations {
    fn new(remaining_prefilter_words: usize, remaining_work: usize, exhausted: bool) -> Self {
        Self {
            invoked: BTreeMap::new(),
            invalid: HashSet::new(),
            keys: 0,
            observations: 0,
            remaining_prefilter_words,
            remaining_work,
            exhausted,
        }
    }

    fn reserve_prefilter(&mut self, words: usize) -> bool {
        if self.exhausted {
            return false;
        }
        let Some(next) = self.remaining_prefilter_words.checked_sub(words) else {
            self.exhausted = true;
            return false;
        };
        self.remaining_prefilter_words = next;
        true
    }

    fn reserve_method(&mut self, code: &crate::native_dex::DexCode) -> bool {
        if self.exhausted {
            return false;
        }
        let Some(register_work) = code
            .instructions
            .len()
            .checked_mul(usize::from(code.registers).max(1))
        else {
            self.exhausted = true;
            return false;
        };
        let Some(work) = layered_exception_work(code)
            .and_then(|exception_work| register_work.checked_add(exception_work))
        else {
            self.exhausted = true;
            return false;
        };
        let Some(next) = self.remaining_work.checked_sub(work) else {
            self.exhausted = true;
            return false;
        };
        self.remaining_work = next;
        true
    }

    fn start_allocation(&mut self) -> Option<usize> {
        if self.exhausted {
            return None;
        }
        let Some(observations) = self.observations.checked_add(1) else {
            self.exhausted = true;
            return None;
        };
        self.observations = observations;
        if observations > MAX_LAYERED_NOARG_OBSERVATIONS {
            self.exhausted = true;
            return None;
        }
        Some(observations)
    }

    fn observe(&mut self, leaf: Arc<str>, invoked: Arc<str>) {
        if self.exhausted {
            return;
        }
        let known = self.invoked.contains_key(&leaf) || self.invalid.contains(&leaf);
        if !known {
            let Some(keys) = self.keys.checked_add(1) else {
                self.exhausted = true;
                return;
            };
            if keys > MAX_LAYERED_NOARG_KEYS {
                self.exhausted = true;
                return;
            }
            self.keys = keys;
        }
        match self.invoked.entry(leaf) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(invoked));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry.get().as_ref() != Some(&invoked) {
                    entry.insert(None);
                }
            }
        }
    }

    fn invalidate(&mut self, leaf: Arc<str>) {
        if self.exhausted {
            return;
        }
        let known = self.invalid.contains(&leaf) || self.invoked.contains_key(&leaf);
        if !known {
            let Some(keys) = self.keys.checked_add(1) else {
                self.exhausted = true;
                return;
            };
            if keys > MAX_LAYERED_NOARG_KEYS {
                self.exhausted = true;
                return;
            }
            self.keys = keys;
        }
        self.invalid.insert(leaf);
    }
}

fn legal_nonfinal_parent(class: &DexClass) -> bool {
    class.access_flags & (0x10 | 0x200 | 0x4000) == 0
        && matches!(class.access_flags & 7, 0 | 1)
        && independent_constructor_owner(class)
}

fn documented_retained_overloads(class: &DexClass) -> Option<HashSet<Vec<Arc<str>>>> {
    let mut seen = HashSet::new();
    let mut public = HashSet::new();
    for method in class
        .methods
        .iter()
        .filter(|method| method.name.as_ref() == "<init>")
    {
        if !seen.insert(method.parameters.clone())
            || method.declaring_type != class.descriptor
            || method.return_type.as_ref() != "V"
            || method.access_flags & 0x10000 == 0
            || method.access_flags & (0x8 | 0x100 | 0x400) != 0
            || !matches!(method.access_flags & 7, 0 | 1 | 2 | 4)
            || method.code.is_none()
        {
            return None;
        }
        if method.parameters.is_empty() {
            return None;
        }
        if method.access_flags & 7 == 1 {
            public.insert(method.parameters.clone());
        }
    }
    (!seen.is_empty()).then_some(public)
}

fn layered_noarg_candidates_with_budget<'a>(
    classes: &[&'a DexClass],
    verified_implicit_constructors: &HashMap<Arc<str>, Arc<str>>,
    owners: &HashMap<&str, &'a DexClass>,
    duplicates: &HashSet<&str>,
    max_work: usize,
) -> (BTreeMap<Arc<str>, LayeredNoargCandidate>, usize, bool) {
    if owners.contains_key("Ljava/lang/Object;") || duplicates.contains("Ljava/lang/Object;") {
        return (BTreeMap::new(), max_work, false);
    }
    let mut ordered = classes.to_vec();
    ordered.sort_by(|left, right| left.descriptor.cmp(&right.descriptor));
    let mut result = BTreeMap::new();
    let mut remaining_work = max_work;
    for leaf in ordered {
        if result.len() >= MAX_LAYERED_NOARG_KEYS
            || duplicates.contains(leaf.descriptor.as_ref())
            || leaf.access_flags & (0x200 | 0x400 | 0x4000) != 0
            || !independent_constructor_owner(leaf)
        {
            continue;
        }
        let Some(immediate_name) = leaf.superclass.as_deref() else {
            continue;
        };
        if duplicates.contains(immediate_name) {
            continue;
        }
        let Some(immediate) = owners.get(immediate_name).copied() else {
            continue;
        };
        if !legal_nonfinal_parent(immediate) || !class_accessible_from(leaf, immediate) {
            continue;
        }
        let Some(skipped_name) = immediate.superclass.as_deref() else {
            continue;
        };
        if duplicates.contains(skipped_name) {
            continue;
        }
        let Some(skipped) = owners.get(skipped_name).copied() else {
            continue;
        };
        if skipped.access_flags & (0x10 | 0x200 | 0x4000) != 0
            || !matches!(skipped.access_flags & 7, 0 | 1)
            || !independent_constructor_owner(skipped)
            || !class_accessible_from(immediate, skipped)
            || verified_implicit_constructors
                .get(skipped_name)
                .map(|owner| owner.as_ref())
                != Some("Ljava/lang/Object;")
        {
            continue;
        }
        let Some(declaration_work) = leaf
            .fields
            .len()
            .checked_add(leaf.methods.len())
            .and_then(|work| work.checked_add(immediate.fields.len()))
            // The immediate method table is traversed once to select the
            // forwarder and once by the exact raw-body check.
            .and_then(|work| {
                immediate
                    .methods
                    .len()
                    .checked_mul(2)
                    .and_then(|methods| work.checked_add(methods))
            })
            .and_then(|work| work.checked_add(skipped.methods.len()))
        else {
            return (BTreeMap::new(), 0, true);
        };
        if !reserve_layered_work(&mut remaining_work, declaration_work, 1) {
            return (BTreeMap::new(), 0, true);
        }
        if leaf
            .fields
            .iter()
            .any(|field| !field.is_static && field.access_flags & 0x10 != 0)
            || immediate
                .fields
                .iter()
                .any(|field| !field.is_static && field.access_flags & 0x10 != 0)
            || skipped
                .methods
                .iter()
                .any(|method| method.name.as_ref() == "<init>")
        {
            continue;
        }
        let Some(retained_parameters) = documented_retained_overloads(leaf) else {
            continue;
        };
        let Some(target) = forwarding_target(leaf, immediate, &[]) else {
            continue;
        };
        let Some(code) = target.code.as_ref() else {
            continue;
        };
        if !reserve_layered_work(
            &mut remaining_work,
            code.instructions.len(),
            usize::from(code.registers),
        ) {
            return (BTreeMap::new(), 0, true);
        }
        let invoked: Arc<str> = Arc::from("Ljava/lang/Object;");
        if code.instructions.len() != 4
            || !target.thrown_types.is_empty()
            || !exact_trivial_noarg_forwarder(immediate, target, &invoked)
        {
            continue;
        }
        result.insert(
            leaf.descriptor.clone(),
            LayeredNoargCandidate {
                immediate: immediate.descriptor.clone(),
                skipped: skipped.descriptor.clone(),
                invoked,
                retained_parameters,
            },
        );
    }
    (result, remaining_work, false)
}

fn complete_layered_method_validation(
    class: &DexClass,
    method: &DexMethod,
    ir: &DecodedMethod,
) -> Option<(
    crate::native_cfg::ControlFlowGraph,
    crate::native_calls::BoundCalls,
)> {
    let code = method.code.as_ref()?;
    let parameter_words = method
        .parameters
        .iter()
        .try_fold(0usize, |words, parameter| {
            words.checked_add(if matches!(parameter.as_ref(), "J" | "D") {
                2
            } else {
                1
            })
        })?;
    let receiver_words = if method.access_flags & 0x8 == 0 { 1 } else { 0 };
    let expected_ins = parameter_words.checked_add(receiver_words)?;
    if usize::from(code.ins) != expected_ins || code.ins > code.registers {
        return None;
    }
    let cfg = crate::native_cfg::ControlFlowGraph::build(code).ok()?;
    let bound = crate::native_calls::BoundCalls::bind(code, ir, &class.symbols).ok()?;
    for call in &bound.calls {
        let receiver_words: usize = if call.receiver.is_some() { 1 } else { 0 };
        let argument_words = call.arguments.iter().try_fold(0usize, |words, argument| {
            words.checked_add(if matches!(argument.descriptor.as_ref(), "J" | "D") {
                2
            } else {
                1
            })
        })?;
        if receiver_words.checked_add(argument_words)? > usize::from(code.outs) {
            return None;
        }
    }
    Some((cfg, bound))
}

pub(super) fn recover(
    classes: &[&DexClass],
    verified_implicit_constructors: &HashMap<Arc<str>, Arc<str>>,
) -> HashMap<Arc<str>, Vec<RecoveredConstructor>> {
    let mut owners = HashMap::new();
    let mut duplicates = HashSet::new();
    for &class in classes {
        if owners.insert(class.descriptor.as_ref(), class).is_some() {
            duplicates.insert(class.descriptor.as_ref());
        }
    }
    let (layered_candidates, layered_remaining_work, layered_exhausted) =
        layered_noarg_candidates_with_budget(
            classes,
            verified_implicit_constructors,
            &owners,
            &duplicates,
            MAX_LAYERED_NOARG_WORK,
        );
    let layered_candidate_names: HashSet<_> = layered_candidates
        .keys()
        .map(|owner| owner.as_ref())
        .collect();
    let mut candidates: HashSet<_> = classes
        .iter()
        .filter(|class| {
            !duplicates.contains(class.descriptor.as_ref())
                && class.access_flags & (0x200 | 0x400 | 0x4000) == 0
                && independent_constructor_owner(class)
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
    let platform_leaves: HashSet<_> = classes
        .iter()
        .filter(|class| platform_composition::candidate(class, &owners, &duplicates))
        .map(|class| class.descriptor.as_ref())
        .collect();
    candidates.extend(platform_leaves.iter().copied());
    let mut platform_audit = platform_leaves.clone();
    platform_audit.extend(platform_leaves.iter().filter_map(|owner| {
        owners
            .get(owner)
            .and_then(|leaf| leaf.superclass.as_deref())
    }));
    let mut platform_pool_relevance = HashMap::new();
    let mut platform_observations = platform_composition::Observations::default();
    let mut layered_pool_relevance = HashMap::new();
    let mut layered_noarg = LayeredNoargObservations::new(
        MAX_LAYERED_NOARG_PREFILTER_WORDS,
        layered_remaining_work,
        layered_exhausted,
    );
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
            let mut layered_raw_mentions = HashSet::new();
            let mut layered_active = false;
            if !layered_noarg.exhausted && !layered_candidate_names.is_empty() {
                let layered_relevant: &HashSet<usize> = layered_pool_relevance
                    .entry(Arc::as_ptr(&class.symbols) as usize)
                    .or_insert_with(|| {
                        class
                            .symbols
                            .types
                            .iter()
                            .enumerate()
                            .filter(|(_, owner)| layered_candidate_names.contains(owner.as_ref()))
                            .map(|(index, _)| index)
                            .collect()
                    });
                // The cheap raw-word scan has its own aggregate budget. Only a
                // method with an actual candidate new-instance reserves the
                // more expensive instruction-by-register proof budget.
                if !layered_relevant.is_empty()
                    && layered_noarg.reserve_prefilter(code.instructions.len())
                {
                    for words in code.instructions.windows(2) {
                        if words[0] as u8 == 0x22
                            && layered_relevant.contains(&(words[1] as usize))
                            && let Some(owner) = class.symbols.types.get(words[1] as usize)
                        {
                            layered_raw_mentions.insert(owner.clone());
                        }
                    }
                    layered_active =
                        !layered_raw_mentions.is_empty() && layered_noarg.reserve_method(code);
                }
            }
            let Ok(ir) = DecodedMethod::decode(code) else {
                for pair in code.instructions.windows(2) {
                    if pair[0] as u8 == 0x22
                        && let Some(owner) = class.symbols.types.get(pair[1] as usize)
                    {
                        if platform_audit.contains(owner.as_ref()) {
                            platform_observations.invalidate(owner.clone());
                        }
                        if layered_active && layered_candidate_names.contains(owner.as_ref()) {
                            layered_noarg.invalidate(owner.clone());
                        }
                    }
                }
                continue;
            };
            let (layered_cfg, layered_bound, layered_call_at) = if layered_active {
                match complete_layered_method_validation(class, method, &ir) {
                    Some((cfg, bound)) => {
                        let call_at = bound
                            .calls
                            .iter()
                            .enumerate()
                            .map(|(index, call)| (call.pc, index))
                            .collect::<HashMap<_, _>>();
                        (Some(cfg), Some(bound), call_at)
                    }
                    None => {
                        for owner in &layered_raw_mentions {
                            layered_noarg.invalidate(owner.clone());
                        }
                        layered_active = false;
                        (None, None, HashMap::new())
                    }
                }
            } else {
                (None, None, HashMap::new())
            };
            let audited: &HashSet<usize> = platform_pool_relevance
                .entry(Arc::as_ptr(&class.symbols) as usize)
                .or_insert_with(|| {
                    class
                        .symbols
                        .types
                        .iter()
                        .enumerate()
                        .filter(|(_, owner)| platform_audit.contains(owner.as_ref()))
                        .map(|(index, _)| index)
                        .collect()
                });
            if ir.instructions.iter().any(|i| {
                i.opcode == 0x22
                    && i.reference
                        .is_some_and(|r| audited.contains(&(r.index as usize)))
            }) {
                platform_observations.collect(class, method, &ir, &platform_audit);
            }
            // A switch elsewhere in a method does not invalidate a local
            // allocation/initialization pair. Decode every case and default
            // boundary before observing pairs; never carry an uninitialized
            // identity into a block with an alternate entry.
            let switch_boundaries = if ir
                .instructions
                .iter()
                .any(|i| matches!(i.opcode, 0x2b | 0x2c))
            {
                if let Some(cfg) = layered_cfg.as_ref() {
                    cfg.blocks.iter().map(|b| b.start).collect()
                } else {
                    let Ok(cfg) = crate::native_cfg::ControlFlowGraph::build(code) else {
                        continue;
                    };
                    cfg.blocks.iter().map(|b| b.start).collect()
                }
            } else {
                HashSet::new()
            };
            let mut targets: HashSet<usize> = ir
                .instructions
                .iter()
                .filter_map(|i| i.branch_target)
                .collect();
            targets.extend(switch_boundaries);
            targets.extend(
                code.try_regions
                    .iter()
                    .flat_map(|r| r.catches.iter().map(|(_, pc)| *pc as usize)),
            );
            let layered_boundaries: HashSet<_> = layered_cfg
                .as_ref()
                .map(|cfg| {
                    cfg.blocks
                        .iter()
                        .filter(|block| block.start != 0)
                        .map(|block| block.start)
                        .collect()
                })
                .unwrap_or_default();
            let mut allocations: Vec<Option<Arc<str>>> = vec![None; code.registers as usize];
            // This identity table is separate from the original recovery
            // registers above. It cannot change an existing recovery result.
            let mut layered_registers = if layered_active {
                vec![None; code.registers as usize]
            } else {
                vec![]
            };
            let mut layered_pending: HashMap<usize, Arc<str>> = HashMap::new();
            for instruction in &ir.instructions {
                if layered_active
                    && !layered_noarg.exhausted
                    && layered_boundaries.contains(&instruction.pc)
                {
                    for (_, owner) in layered_pending.drain() {
                        layered_noarg.invalidate(owner);
                    }
                    layered_registers.fill(None);
                }
                if targets.contains(&instruction.pc) {
                    if layered_active && !layered_noarg.exhausted {
                        for (_, owner) in layered_pending.drain() {
                            layered_noarg.invalidate(owner);
                        }
                        layered_registers.fill(None);
                    }
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
                let layered_tracking = layered_active && !layered_noarg.exhausted;
                let layered_copy = if layered_tracking && matches!(instruction.opcode, 0x07..=0x09)
                {
                    instruction
                        .reads
                        .first()
                        .and_then(|read| layered_registers[read.register as usize])
                } else {
                    None
                };
                if layered_tracking {
                    let read_ids: Vec<_> = instruction
                        .reads
                        .iter()
                        .flat_map(|read| {
                            (0..read.kind.word_count()).filter_map(|offset| {
                                layered_registers
                                    .get(usize::from(read.register).checked_add(offset)?)
                                    .copied()
                                    .flatten()
                            })
                        })
                        .collect();
                    if !read_ids.is_empty() && !matches!(instruction.opcode, 0x07..=0x09) {
                        let valid_completion = if matches!(instruction.opcode, 0x70 | 0x76)
                            && read_ids.len() == 1
                        {
                            let allocation_id = read_ids[0];
                            let owner = layered_pending.get(&allocation_id);
                            let candidate = owner.and_then(|owner| layered_candidates.get(owner));
                            let call = layered_call_at.get(&instruction.pc).and_then(|index| {
                                layered_bound
                                    .as_ref()
                                    .and_then(|bound| bound.calls.get(*index))
                            });
                            match (owner, candidate, call) {
                                (
                                    Some(owner),
                                    Some(candidate),
                                    Some(crate::native_calls::BoundCall {
                                        kind: crate::native_calls::CallKind::Direct,
                                        target:
                                            crate::native_calls::CallTarget::Method {
                                                declaring_type,
                                                name,
                                                ..
                                            },
                                        receiver: Some(receiver),
                                        arguments,
                                        return_type,
                                        ..
                                    }),
                                ) => {
                                    let receiver_matches = layered_registers
                                        [receiver.register as usize]
                                        == Some(allocation_id);
                                    let argument_types: Vec<_> = arguments
                                        .iter()
                                        .map(|argument| argument.descriptor.clone())
                                        .collect();
                                    let exact_layered = receiver_matches
                                        && declaring_type == &candidate.invoked
                                        && name.as_ref() == "<init>"
                                        && return_type.as_ref() == "V"
                                        && argument_types.is_empty();
                                    if exact_layered {
                                        layered_noarg
                                            .observe(owner.clone(), declaring_type.clone());
                                    }
                                    let ordinary_retained = receiver_matches
                                        && declaring_type == owner
                                        && name.as_ref() == "<init>"
                                        && return_type.as_ref() == "V"
                                        && candidate.retained_parameters.contains(&argument_types);
                                    exact_layered || ordinary_retained
                                }
                                _ => false,
                            }
                        } else {
                            false
                        };
                        let retired: HashSet<_> = if valid_completion {
                            read_ids.iter().copied().collect()
                        } else {
                            let read_ids: HashSet<_> = read_ids.into_iter().collect();
                            for allocation_id in &read_ids {
                                if let Some(owner) = layered_pending.get(allocation_id) {
                                    layered_noarg.invalidate(owner.clone());
                                }
                            }
                            read_ids
                        };
                        for allocation_id in &retired {
                            layered_pending.remove(allocation_id);
                        }
                        for slot in &mut layered_registers {
                            if slot.is_some_and(|allocation_id| retired.contains(&allocation_id)) {
                                *slot = None;
                            }
                        }
                    }
                }
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
                                access_flags: 1,
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
                    if layered_tracking {
                        layered_registers[write.register as usize] = None;
                    }
                    if write.kind == ValueKind::Wide64 {
                        allocations[write.register as usize + 1] = None;
                        if layered_tracking {
                            layered_registers[write.register as usize + 1] = None;
                        }
                    }
                }
                if instruction.opcode == 0x22 {
                    if let Some(reference) = instruction.reference
                        && relevant.contains(&(reference.index as usize))
                        && let Some(write) = instruction.writes.first()
                    {
                        let owner = class.symbols.types.get(reference.index as usize).cloned();
                        if layered_tracking
                            && let Some(owner) = &owner
                            && layered_candidate_names.contains(owner.as_ref())
                            && let Some(allocation_id) = layered_noarg.start_allocation()
                        {
                            layered_pending.insert(allocation_id, owner.clone());
                            layered_registers[write.register as usize] = Some(allocation_id);
                        }
                        allocations[write.register as usize] = owner;
                    }
                } else if let Some(value) = copy
                    && let Some(write) = instruction.writes.first()
                {
                    allocations[write.register as usize] = Some(value);
                    if layered_tracking {
                        layered_registers[write.register as usize] = layered_copy;
                    }
                }
                if instruction.branch_target.is_some()
                    || matches!(instruction.opcode, 0x0e..=0x11 | 0x27 | 0x2b | 0x2c)
                {
                    allocations.fill(None);
                    if layered_tracking {
                        layered_registers.fill(None);
                    }
                }
                if layered_tracking && !layered_pending.is_empty() {
                    // At the end of every prior instruction, each pending ID
                    // occupied at least one register. Thus both this set build
                    // and the pending walk are linear in the register count.
                    let present: HashSet<_> =
                        layered_registers.iter().filter_map(|slot| *slot).collect();
                    let lost: Vec<_> = layered_pending
                        .keys()
                        .filter(|allocation_id| !present.contains(allocation_id))
                        .copied()
                        .collect();
                    for allocation_id in lost {
                        if let Some(owner) = layered_pending.remove(&allocation_id) {
                            layered_noarg.invalidate(owner);
                        }
                    }
                }
            }
            if layered_active && !layered_noarg.exhausted {
                for (_, owner) in layered_pending {
                    layered_noarg.invalidate(owner);
                }
            }
        }
    }
    recover_observed_super_forwarders(classes, &owners, &duplicates, &mut result);
    platform_observations.commit(&owners, &duplicates, &mut result);
    commit_layered_noarg_observations(layered_noarg, &layered_candidates, &mut result);
    flat_noarg::commit(classes, &owners, &duplicates, &mut result);
    result
        .into_iter()
        .map(|(owner, values)| (owner, values.into_values().collect()))
        .collect()
}

// Restore one erased no-arg allocation layer only from three independent facts:
// an observed exact allocation call, a retained trivial immediate constructor,
// and the hierarchy's already-frozen implicit-constructor certificate. Pending
// allocation records are deliberately absent from every input to this proof.
fn commit_layered_noarg_observations(
    observations: LayeredNoargObservations,
    candidates: &BTreeMap<Arc<str>, LayeredNoargCandidate>,
    result: &mut HashMap<Arc<str>, BTreeMap<String, RecoveredConstructor>>,
) {
    if observations.exhausted {
        return;
    }
    // Freeze compatibility before inserting any record from this category.
    // A pending layered record can therefore never satisfy a later check.
    let mut skipped_compatibility = BTreeMap::new();
    for candidate in candidates.values() {
        skipped_compatibility
            .entry(candidate.skipped.clone())
            .or_insert_with(|| {
                skipped_noarg_entries_compatible(result.get(&candidate.skipped), &candidate.invoked)
            });
    }
    for (leaf_name, invoked) in observations.invoked {
        let Some(invoked) = invoked else {
            continue;
        };
        let Some(candidate) = candidates.get(&leaf_name) else {
            continue;
        };
        if observations.invalid.contains(&leaf_name)
            || invoked != candidate.invoked
            || result
                .get(&leaf_name)
                .is_some_and(|entries| entries.contains_key(""))
            || !skipped_compatibility
                .get(&candidate.skipped)
                .copied()
                .unwrap_or(false)
        {
            continue;
        }
        result.entry(leaf_name).or_default().insert(
            String::new(),
            RecoveredConstructor {
                access_flags: 1,
                parent: candidate.immediate.clone(),
                invoked_owner: invoked,
                parameters: vec![],
                thrown_types: vec![],
                strict_allocation_order: true,
                observed_allocation: true,
                observed_super: false,
            },
        );
    }
}

fn skipped_noarg_entries_compatible(
    entries: Option<&BTreeMap<String, RecoveredConstructor>>,
    invoked: &Arc<str>,
) -> bool {
    let Some(entries) = entries else {
        return true;
    };
    let mut noarg = entries
        .iter()
        .filter(|(key, record)| key.is_empty() || record.parameters.is_empty());
    let Some((key, record)) = noarg.next() else {
        return true;
    };
    key.is_empty()
        && noarg.next().is_none()
        && record.parent == *invoked
        && record.invoked_owner == *invoked
        && record.parameters.is_empty()
        && record.thrown_types.is_empty()
}

fn class_accessible_from(child: &DexClass, parent: &DexClass) -> bool {
    parent.access_flags & 1 != 0
        || child.descriptor.rsplit_once('/').map_or("", |(p, _)| p)
            == parent.descriptor.rsplit_once('/').map_or("", |(p, _)| p)
}

fn exact_trivial_noarg_forwarder(class: &DexClass, method: &DexMethod, invoked: &Arc<str>) -> bool {
    use crate::{
        native_calls::{BoundCalls, CallKind, CallTarget},
        native_cfg::ControlFlowGraph,
        native_ssa::SsaMethod,
    };
    if super::trivial_noarg_constructor(class).as_ref() != Some(invoked)
        || method.declaring_type != class.descriptor
        || !method.parameters.is_empty()
        || method.return_type.as_ref() != "V"
        || method.access_flags & 0x10000 == 0
    {
        return false;
    }
    let Some(code) = method.code.as_ref() else {
        return false;
    };
    if code.outs < 1 {
        return false;
    }
    let Ok(ir) = DecodedMethod::decode(code) else {
        return false;
    };
    let Ok(bound) = BoundCalls::bind(code, &ir, &class.symbols) else {
        return false;
    };
    let Ok(cfg) = ControlFlowGraph::build(code) else {
        return false;
    };
    let Ok(ssa) = SsaMethod::build_with_work_limit(code, &ir, &cfg, 256) else {
        return false;
    };
    let [call] = bound.calls.as_slice() else {
        return false;
    };
    let CallTarget::Method {
        declaring_type,
        name,
        ..
    } = &call.target
    else {
        return false;
    };
    call.kind == CallKind::Direct
        && call.pc == 0
        && declaring_type == invoked
        && name.as_ref() == "<init>"
        && call.return_type.as_ref() == "V"
        && call.arguments.is_empty()
        && call.receiver.as_ref().is_some_and(|receiver| {
            super::this_receiver_identity::proves_read(
                method,
                &ir,
                &ssa,
                call.pc,
                usize::from(receiver.register),
            )
        })
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
        let binary = immediate
            .strip_prefix('L')?
            .strip_suffix(';')?
            .replace('/', ".");
        if let Some((_, _, chain)) = platform_member_type(&binary)
            && chain.iter().any(|owner| {
                owners.contains_key(owner.as_str()) || duplicates.contains(owner.as_str())
            })
        {
            return None;
        }
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

/// Restore a bounded chain of erased overloads only at an observed entry-this
/// call to a retained loaded constructor. Retained overloads are never skipped.
fn erased_super_layers<'a>(
    child: &DexClass,
    first: &'a DexClass,
    invoked: &str,
    args: &[Arc<str>],
    owners: &HashMap<&str, &'a DexClass>,
    duplicates: &HashSet<&str>,
) -> Option<Vec<&'a DexClass>> {
    if args.is_empty() || args.len() > 16 {
        return None;
    }
    let package = |name: &str| name.rsplit_once('/').map_or("", |(p, _)| p).to_owned();
    let mut previous = child;
    let mut current = first;
    let mut seen = HashSet::new();
    let mut layers = Vec::new();
    loop {
        if !seen.insert(current.descriptor.as_ref())
            || duplicates.contains(current.descriptor.as_ref())
            || current.access_flags & (0x200 | 0x4000 | 0x10) != 0
            || !matches!(current.access_flags & 7, 0 | 1)
            || current.access_flags & 1 == 0
                && package(&current.descriptor) != package(&previous.descriptor)
        {
            return None;
        }
        if current.descriptor.as_ref() == invoked {
            let target = forwarding_target(previous, current, args)?;
            let mut budget = 8192;
            return (layers.len() >= 2
                && closed_constructor_without_checked_effects(
                    current,
                    target,
                    owners,
                    duplicates,
                    &mut HashSet::new(),
                    &mut budget,
                ))
            .then_some(layers);
        }
        if layers.len() == 8
            || !independent_constructor_owner(current)
            || current
                .fields
                .iter()
                .any(|f| !f.is_static && f.access_flags & 0x10 != 0)
            || current
                .methods
                .iter()
                .any(|m| m.name.as_ref() == "<init>" && m.parameters == args)
            || !args.iter().all(|arg| {
                let ty = arg.trim_start_matches('[');
                if matches!(ty, "Z" | "B" | "S" | "C" | "I" | "J" | "F" | "D") {
                    return true;
                }
                if duplicates.contains(ty) {
                    return false;
                }
                if let Some(class) = owners.get(ty) {
                    !ty.contains('$')
                        && (class.access_flags & 1 != 0
                            || package(ty) == package(&current.descriptor))
                } else {
                    matches!(
                        ty,
                        "Ljava/lang/Object;" | "Ljava/lang/Class;" | "Ljava/lang/String;"
                    )
                }
            })
        {
            return None;
        }
        layers.push(current);
        previous = current;
        current = owners.get(current.superclass.as_deref()?).copied()?;
    }
}

/// Constructor-only closed effect proof for the first erased-overload tranche.
/// The retained target may write fields and compute arguments, but an unknown
/// call, explicit throw, missing body or exhausted proof cannot supply a header.
fn closed_constructor_without_checked_effects(
    class: &DexClass,
    method: &DexMethod,
    owners: &HashMap<&str, &DexClass>,
    duplicates: &HashSet<&str>,
    active: &mut HashSet<String>,
    budget: &mut usize,
) -> bool {
    use crate::native_calls::{BoundCalls, CallKind, CallTarget};
    let key = format!("{}({})", class.descriptor, method.parameters.join(""));
    if active.len() >= 8
        || !active.insert(key.clone())
        || !method.thrown_types.is_empty()
        || method.declaring_type != class.descriptor
        || method.return_type.as_ref() != "V"
        || method.access_flags & (8 | 0x100 | 0x400) != 0
    {
        return false;
    }
    let mut prove = || {
        let code = method.code.as_ref()?;
        if code.registers > 8192
            || code.ins > code.registers
            || usize::from(code.ins)
                != 1 + method
                    .parameters
                    .iter()
                    .map(|p| {
                        if matches!(p.as_ref(), "J" | "D") {
                            2
                        } else {
                            1
                        }
                    })
                    .sum::<usize>()
            || code.outs > 8192
            || code.instructions.len() > 512
            || code.instructions.len() > *budget
            || code.tries != 0
            || !code.try_regions.is_empty()
        {
            return None;
        }
        *budget -= code.instructions.len();
        let ir = DecodedMethod::decode(code).ok()?;
        if ir.instructions.iter().any(|i| i.opcode == 0x27) {
            return None;
        }
        let calls = BoundCalls::bind(code, &ir, &class.symbols).ok()?;
        for call in &calls.calls {
            let CallTarget::Method {
                declaring_type,
                name,
                ..
            } = &call.target
            else {
                return None;
            };
            if call.kind != CallKind::Direct
                || name.as_ref() != "<init>"
                || call.return_type.as_ref() != "V"
                || duplicates.contains(declaring_type.as_ref())
            {
                return None;
            }
            let args: Vec<_> = call
                .arguments
                .iter()
                .map(|a| a.descriptor.clone())
                .collect();
            if let Some(target_class) = owners.get(declaring_type.as_ref()) {
                let mut methods = target_class
                    .methods
                    .iter()
                    .filter(|m| m.name.as_ref() == "<init>" && m.parameters == args);
                let target = methods.next()?;
                if methods.next().is_some()
                    || !closed_constructor_without_checked_effects(
                        target_class,
                        target,
                        owners,
                        duplicates,
                        active,
                        budget,
                    )
                {
                    return None;
                }
            } else if !platform_constructor(declaring_type, &args).is_some_and(|t| t.is_empty()) {
                return None;
            }
        }
        Some(())
    };
    let result = prove().is_some();
    active.remove(&key);
    result
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
    let mut layered_keys = HashSet::new();
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
            || !(independent_constructor_owner(parent)
                || flat_noarg::independent_source_owner(parent, &mut remaining))
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
            // Flat source ownership supplies no enclosing instance only for
            // this zero-argument erased-constructor tranche.
            if !args.is_empty() && !independent_constructor_owner(parent) {
                continue;
            }
            if parent
                .methods
                .iter()
                .any(|m| m.name.as_ref() == "<init>" && m.parameters == args)
            {
                continue;
            }
            let (thrown_types, layers, layered) = if let Some(thrown_types) =
                recovered_forwarding_target(parent, immediate, invoked, &args, owners, duplicates)
            {
                (thrown_types, vec![parent], false)
            } else if let Some(layers) =
                erased_super_layers(child, parent, invoked, &args, owners, duplicates)
            {
                (vec![], layers, true)
            } else {
                continue;
            };
            let key = args.join("");
            for layer in layers {
                let identity = (layer.descriptor.clone(), key.clone());
                if layered {
                    layered_keys.insert(identity.clone());
                }
                if conflicts.contains(&identity) {
                    continue;
                }
                let record = RecoveredConstructor {
                    access_flags: 1,
                    parent: layer.superclass.clone().expect("proven parent"),
                    invoked_owner: invoked.clone(),
                    parameters: args.clone(),
                    thrown_types: thrown_types.clone(),
                    strict_allocation_order: true,
                    observed_allocation: false,
                    observed_super: true,
                };
                let entries = observed.entry(layer.descriptor.clone()).or_default();
                if entries.get(&key).is_some_and(|old| {
                    old.parent != record.parent
                        || old.invoked_owner != record.invoked_owner
                        || old.thrown_types != record.thrown_types
                }) {
                    entries.remove(&key);
                    conflicts.insert(identity);
                } else {
                    entries.insert(key.clone(), record);
                }
            }
        }
    }
    // Validate only the new layered records against the final proposed table.
    // Each must reach its exact retained target through at most eight compatible
    // wrappers. A conflict, cycle or incompatible historical record rejects every
    // dependent wrapper independently, without changing older recovery policy.
    let invalid: Vec<_> = layered_keys
        .into_iter()
        .filter(|(owner, key)| {
            let Some(record) = observed.get(owner).and_then(|entries| entries.get(key)) else {
                return false;
            };
            let compatible = |candidate: &RecoveredConstructor| {
                candidate.invoked_owner == record.invoked_owner
                    && candidate.parameters == record.parameters
                    && candidate.thrown_types == record.thrown_types
                    && candidate.observed_super
            };
            let mut current = owner.clone();
            let mut seen = HashSet::new();
            for _ in 0..8 {
                if !seen.insert(current.clone()) {
                    return true;
                }
                let Some(candidate) = observed.get(&current).and_then(|entries| entries.get(key))
                else {
                    return true;
                };
                if !compatible(candidate)
                    || result
                        .get(&current)
                        .and_then(|entries| entries.get(key))
                        .is_some_and(|old| {
                            old.parent != candidate.parent
                                || old.invoked_owner != candidate.invoked_owner
                                || old.thrown_types != candidate.thrown_types
                        })
                {
                    return true;
                }
                let Some(class) = owners.get(current.as_ref()) else {
                    return true;
                };
                let Some(parent) = owners.get(candidate.parent.as_ref()) else {
                    return true;
                };
                if parent
                    .methods
                    .iter()
                    .any(|m| m.name.as_ref() == "<init>" && m.parameters == record.parameters)
                {
                    return parent.descriptor != record.invoked_owner
                        || forwarding_target(class, parent, &record.parameters).is_none();
                }
                current = candidate.parent.clone();
            }
            true
        })
        .collect();
    for (owner, key) in invalid {
        if let Some(entries) = observed.get_mut(&owner) {
            entries.remove(&key);
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

#[cfg(test)]
mod layered_noarg_certificate_tests {
    use super::*;

    fn budget_class(owner: &str, parent: &str, flags: u32) -> DexClass {
        DexClass {
            descriptor: Arc::from(owner),
            superclass: Some(Arc::from(parent)),
            interfaces: vec![],
            access_flags: flags,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
            symbols: Arc::new(crate::native_dex::DexSymbols::default()),
        }
    }

    fn budget_fixture() -> (DexClass, DexClass, DexClass) {
        let mut leaf = budget_class("Lsample/Leaf;", "Lsample/Immediate;", 1);
        leaf.methods.push(DexMethod {
            declaring_type: leaf.descriptor.clone(),
            name: Arc::from("<init>"),
            return_type: Arc::from("V"),
            parameters: vec![Arc::from("Ljava/lang/Object;")],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(crate::native_dex::DexCode {
                registers: 2,
                ins: 2,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![0x000e],
            }),
        });
        for _ in 0..8 {
            leaf.methods.push(DexMethod {
                declaring_type: leaf.descriptor.clone(),
                name: Arc::from("neutral"),
                return_type: Arc::from("V"),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 1,
                code: None,
            });
        }
        let mut immediate = budget_class("Lsample/Immediate;", "Lsample/Implicit;", 0x401);
        immediate.symbols = Arc::new(crate::native_dex::DexSymbols {
            types: vec![Arc::from("Ljava/lang/Object;")],
            strings: vec!["<init>".to_owned()],
            protos: vec![(Arc::from("V"), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        immediate.methods.push(DexMethod {
            declaring_type: immediate.descriptor.clone(),
            name: Arc::from("<init>"),
            return_type: Arc::from("V"),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(crate::native_dex::DexCode {
                registers: 1,
                ins: 1,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![0x1070, 0, 0, 0x000e],
            }),
        });
        let implicit = budget_class("Lsample/Implicit;", "Ljava/lang/Object;", 0x401);
        (leaf, immediate, implicit)
    }

    fn record() -> RecoveredConstructor {
        RecoveredConstructor {
            access_flags: 1,
            parent: Arc::from("Ljava/lang/Object;"),
            invoked_owner: Arc::from("Ljava/lang/Object;"),
            parameters: vec![],
            thrown_types: vec![],
            strict_allocation_order: true,
            observed_allocation: true,
            observed_super: false,
        }
    }

    #[test]
    fn skipped_implicit_entry_must_be_absent_or_exactly_compatible() {
        let root: Arc<str> = Arc::from("Ljava/lang/Object;");
        assert!(skipped_noarg_entries_compatible(None, &root));
        let compatible = BTreeMap::from([(String::new(), record())]);
        assert!(skipped_noarg_entries_compatible(Some(&compatible), &root));
        for variant in 0..4 {
            let mut conflict = record();
            match variant {
                0 => conflict.parent = Arc::from("Lsample/Other;"),
                1 => conflict.invoked_owner = Arc::from("Lsample/Other;"),
                2 => conflict.parameters.push(Arc::from("I")),
                3 => conflict
                    .thrown_types
                    .push(Arc::from("Ljava/lang/Exception;")),
                _ => unreachable!(),
            }
            let conflict = BTreeMap::from([(String::new(), conflict)]);
            assert!(!skipped_noarg_entries_compatible(Some(&conflict), &root));
        }
        let malformed = BTreeMap::from([("I".to_owned(), record())]);
        assert!(!skipped_noarg_entries_compatible(Some(&malformed), &root));
    }

    #[test]
    fn method_work_reservation_is_atomic_and_failure_disables_bookkeeping() {
        let mut observations = LayeredNoargObservations::new(8, 6, false);
        assert!(observations.reserve_prefilter(8));
        assert_eq!(observations.remaining_prefilter_words, 0);
        let code = crate::native_dex::DexCode {
            registers: 3,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0, 0],
        };
        assert!(observations.reserve_method(&code));
        assert_eq!(observations.remaining_work, 0);
        assert!(!observations.reserve_method(&crate::native_dex::DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0],
        }));
        assert!(observations.exhausted);
        observations.observe(Arc::from("Lsample/Leaf;"), Arc::from("Ljava/lang/Object;"));
        observations.invalidate(Arc::from("Lsample/Other;"));
        assert!(observations.invoked.is_empty());
        assert!(observations.invalid.is_empty());

        let mut remaining = usize::MAX;
        assert!(!reserve_layered_work(&mut remaining, usize::MAX, 2));
        assert_eq!(remaining, usize::MAX);

        let mut exception_bound = LayeredNoargObservations::new(10, 5, false);
        assert!(
            !exception_bound.reserve_method(&crate::native_dex::DexCode {
                registers: 1,
                ins: 0,
                outs: 0,
                tries: 1,
                try_regions: vec![crate::native_dex::DexTryRegion {
                    start: 0,
                    end: 2,
                    catches: vec![(None, 1), (None, 1)].into(),
                }],
                offset: 0,
                instructions: vec![0, 0],
            })
        );
        assert!(exception_bound.exhausted);
    }

    #[test]
    fn low_work_limit_exhaustion_is_order_independent() {
        for costs in [[(2, 2), (1, 2)], [(1, 2), (2, 2)]] {
            let mut observations = LayeredNoargObservations::new(5, 5, false);
            for (words, _) in costs {
                observations.reserve_prefilter(words);
            }
            for (instructions, registers) in costs {
                observations.reserve_method(&crate::native_dex::DexCode {
                    registers,
                    ins: 0,
                    outs: 0,
                    tries: 0,
                    try_regions: vec![],
                    offset: 0,
                    instructions: vec![0; instructions],
                });
            }
            assert!(observations.exhausted);
            assert!(observations.invoked.is_empty());
            assert!(observations.invalid.is_empty());
        }
        for costs in [[4, 2], [2, 4]] {
            let mut observations = LayeredNoargObservations::new(5, 100, false);
            for words in costs {
                observations.reserve_prefilter(words);
            }
            assert!(observations.exhausted);
            assert!(observations.start_allocation().is_none());
            assert!(observations.invoked.is_empty());
            assert!(observations.invalid.is_empty());
        }
    }

    #[test]
    fn candidate_declaration_budget_is_atomic_and_order_independent() {
        for reverse in [false, true] {
            let (leaf, immediate, implicit) = budget_fixture();
            let mut classes = vec![&leaf, &immediate, &implicit];
            if reverse {
                classes.reverse();
            }
            let owners: HashMap<_, _> = classes
                .iter()
                .map(|class| (class.descriptor.as_ref(), *class))
                .collect();
            let verified = HashMap::from([(
                Arc::from("Lsample/Implicit;"),
                Arc::from("Ljava/lang/Object;"),
            )]);
            let (candidates, remaining, exhausted) = layered_noarg_candidates_with_budget(
                &classes,
                &verified,
                &owners,
                &HashSet::new(),
                5,
            );
            assert!(candidates.is_empty());
            assert_eq!(remaining, 0);
            assert!(exhausted);
        }
    }
}
