//! Bounded recovery of an optimizer-erased no-arg constructor for an ordinary
//! flat Java class presentation.
//!
//! This proves constructor behavior only. Existing flat `$` names remain flat;
//! DEX nesting annotations are retained as capture evidence and never converted
//! into a new source nesting claim here.

use super::{RecoveredConstructor, exact_trivial_noarg_forwarder, forwarding_target};
use crate::{
    native_calls::{BoundCall, CallKind, CallTarget},
    native_cfg::ControlFlowGraph,
    native_dex::{DexAnnotationSet, DexClass, DexMethod, DexValue},
    native_ir::{DecodedMethod, PoolKind, ValueKind},
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

const MAX_CANDIDATES: usize = 16_384;
const MAX_OBSERVATIONS: usize = 100_000;
const MAX_DECLARATION_WORK: usize = 2_000_000;
const MAX_PREFILTER_WORDS: usize = 32_000_000;
const MAX_PROOF_WORK: usize = 8_000_000;
const MAX_OBLIGATION_OWNERS: usize = 256;
const PINNED_PLATFORM_DECLARATIONS: usize = 60_543;

struct Candidate {
    parent: Arc<str>,
    thrown_types: Vec<Arc<str>>,
    access_flags: u32,
}

#[derive(Default)]
struct PoolRelevance {
    allocated: HashMap<u32, Arc<str>>,
    constructors: HashMap<u32, Arc<str>>,
}

struct Observations {
    counts: HashMap<Arc<str>, usize>,
    invalid: HashSet<Arc<str>>,
    remaining_prefilter_words: usize,
    remaining_proof_work: usize,
    total: usize,
    exhausted: bool,
}

impl Observations {
    fn new() -> Self {
        Self::with_limits(MAX_PREFILTER_WORDS, MAX_PROOF_WORK)
    }

    fn with_limits(prefilter_words: usize, proof_work: usize) -> Self {
        Self {
            counts: HashMap::new(),
            invalid: HashSet::new(),
            remaining_prefilter_words: prefilter_words,
            remaining_proof_work: proof_work,
            total: 0,
            exhausted: false,
        }
    }

    fn reserve_prefilter(&mut self, work: usize) -> bool {
        let Some(remaining) = self.remaining_prefilter_words.checked_sub(work) else {
            self.exhausted = true;
            return false;
        };
        self.remaining_prefilter_words = remaining;
        true
    }

    fn reserve_method(&mut self, method: &DexMethod) -> Option<usize> {
        let code = method.code.as_ref()?;
        // SSA's local walk and its internal dominator construction each use
        // this established per-analysis limit and are charged independently.
        let limit = code
            .instructions
            .len()
            .checked_mul(usize::from(code.registers).checked_add(1)?)?
            .checked_mul(16)?
            .clamp(32_768, 2_000_000);
        let work = limit.checked_mul(2)?;
        let Some(remaining) = self.remaining_proof_work.checked_sub(work) else {
            // Proof exhaustion is local to owners mentioned by this method.
            // Keep scanning every caller so an unproved later observation
            // invalidates its owner without discarding unrelated proofs.
            self.remaining_proof_work = 0;
            return None;
        };
        self.remaining_proof_work = remaining;
        Some(limit)
    }

    fn invalidate<'a>(&mut self, owners: impl IntoIterator<Item = &'a Arc<str>>) {
        self.invalid.extend(owners.into_iter().cloned());
    }

    fn observe(&mut self, owner: Arc<str>) {
        let Some(total) = self.total.checked_add(1) else {
            self.exhausted = true;
            return;
        };
        if total > MAX_OBSERVATIONS {
            self.exhausted = true;
            return;
        }
        let count = self.counts.entry(owner).or_default();
        let Some(next) = count.checked_add(1) else {
            self.exhausted = true;
            return;
        };
        self.total = total;
        *count = next;
    }
}

fn package(descriptor: &str) -> &str {
    descriptor
        .rsplit_once('/')
        .map_or("", |(package, _)| package)
}

fn exact_annotation<'a>(
    class: &'a DexClass,
    name: &str,
) -> Option<Option<&'a crate::native_dex::DexAnnotation>> {
    let Some(set) = class
        .symbols
        .annotations
        .get(&class.annotations_offset)
        .and_then(|directory| directory.class.as_ref())
    else {
        return Some(None);
    };
    let mut matches = set.iter().filter(|annotation| {
        class
            .symbols
            .types
            .get(annotation.type_idx as usize)
            .is_some_and(|ty| ty.as_ref() == name)
    });
    let first = matches.next();
    if matches.next().is_some() || first.is_some_and(|annotation| annotation.visibility != 2) {
        None
    } else {
        Some(first.map(|annotation| annotation.as_ref()))
    }
}

fn annotation_element(
    class: &DexClass,
    annotation: &crate::native_dex::DexAnnotation,
    name: &str,
) -> Option<DexValue> {
    let mut values = annotation.elements.iter().filter(|(index, _)| {
        class
            .symbols
            .strings
            .get(*index as usize)
            .is_some_and(|value| value == name)
    });
    let value = values.next()?.1.clone();
    values.next().is_none().then_some(value)
}

fn reserve_declaration(remaining: &mut usize, work: usize) -> Result<(), ()> {
    let Some(next) = remaining.checked_sub(work) else {
        return Err(());
    };
    *remaining = next;
    Ok(())
}

fn class_declaration_work(class: &DexClass) -> Option<usize> {
    let mut work = class
        .descriptor
        .len()
        .checked_add(class.superclass.as_deref().map_or(0, str::len))?;
    for interface in &class.interfaces {
        work = work.checked_add(interface.len())?;
    }
    for field in &class.fields {
        work = work
            .checked_add(field.name.len())?
            .checked_add(field.field_type.len())?
            .checked_add(1)?;
    }
    for method in &class.methods {
        work = work
            .checked_add(method.name.len())?
            .checked_add(method.return_type.len())?
            .checked_add(1)?;
        for parameter in &method.parameters {
            work = work.checked_add(parameter.len())?;
        }
        for thrown in &method.thrown_types {
            work = work.checked_add(thrown.len())?;
        }
    }
    Some(work)
}

fn unique_java_method_signatures(class: &DexClass) -> bool {
    let mut signatures = HashSet::new();
    class
        .methods
        .iter()
        .all(|method| signatures.insert((method.name.clone(), method.parameters.clone())))
}

fn unique_type_list(types: &[Arc<str>]) -> bool {
    let mut owners = HashSet::new();
    types.iter().all(|owner| owners.insert(owner.as_ref()))
}

fn final_parent_method_conflict(candidate: &DexClass, parent: &DexClass) -> bool {
    let candidate_signatures: HashSet<(&str, &[Arc<str>])> = candidate
        .methods
        .iter()
        .map(|method| (method.name.as_ref(), method.parameters.as_slice()))
        .collect();
    parent.methods.iter().any(|parent_method| {
        !matches!(parent_method.name.as_ref(), "<init>" | "<clinit>")
            && parent_method.access_flags & 0x18 == 0x10
            && candidate_signatures.contains(&(
                parent_method.name.as_ref(),
                parent_method.parameters.as_slice(),
            ))
    })
}

fn exact_public_method(
    candidate: &DexClass,
    implementations: &HashMap<(&str, &[Arc<str>]), &DexMethod>,
    name: &str,
    parameters: &[Arc<str>],
    return_type: &str,
    declared_throws: &[Arc<str>],
) -> bool {
    let Some(method) = implementations.get(&(name, parameters)).copied() else {
        return false;
    };
    method.declaring_type == candidate.descriptor
        && method.return_type.as_ref() == return_type
        && matches!(method.access_flags, 1 | 0x11 | 0x1041 | 0x1051)
        && (method.thrown_types.is_empty() || method.thrown_types == declared_throws)
        && method.code.is_some()
        && crate::native_java::ordinary_method_declaration_authority(candidate, method)
}

fn exact_public_implementation(
    candidate: &DexClass,
    implementations: &HashMap<(&str, &[Arc<str>]), &DexMethod>,
    declaration: &DexMethod,
) -> bool {
    exact_public_method(
        candidate,
        implementations,
        &declaration.name,
        &declaration.parameters,
        &declaration.return_type,
        &declaration.thrown_types,
    )
}

fn abstract_obligations_proven(
    candidate: &DexClass,
    parent: Option<&DexClass>,
    owners: &HashMap<&str, &DexClass>,
    duplicates: &HashSet<&str>,
    declaration_work: &mut usize,
) -> Result<bool, ()> {
    let implementations: HashMap<(&str, &[Arc<str>]), &DexMethod> = candidate
        .methods
        .iter()
        .map(|method| ((method.name.as_ref(), method.parameters.as_slice()), method))
        .collect();
    if let Some(parent) = parent {
        if !unique_java_method_signatures(parent) {
            return Ok(false);
        }
        for method in &parent.methods {
            if method.access_flags & 0x400 != 0
                && !matches!(method.name.as_ref(), "<init>" | "<clinit>")
                && (method.access_flags != 0x401
                    || method.code.is_some()
                    || !crate::native_java::ordinary_method_declaration_authority(parent, method)
                    || !exact_public_implementation(candidate, &implementations, method))
            {
                return Ok(false);
            }
        }
    }

    let mut pending: Vec<(Arc<str>, bool)> = candidate
        .interfaces
        .iter()
        .cloned()
        .map(|owner| (owner, false))
        .collect();
    if let Some(parent) = parent {
        pending.extend(
            parent
                .interfaces
                .iter()
                .cloned()
                .map(|owner| (owner, false)),
        );
    }
    let mut seen = HashSet::new();
    let mut active = HashSet::new();
    while let Some((owner, exiting)) = pending.pop() {
        if exiting {
            active.remove(owner.as_ref());
            continue;
        }
        if active.contains(owner.as_ref()) {
            return Ok(false);
        }
        if !seen.insert(owner.clone()) {
            continue;
        }
        if seen.len() > MAX_OBLIGATION_OWNERS || duplicates.contains(owner.as_ref()) {
            return Ok(false);
        }
        active.insert(owner.clone());
        pending.push((owner.clone(), true));
        let Some(interface) = owners.get(owner.as_ref()).copied() else {
            if parent.is_some()
                || candidate.interfaces.len() != 1
                || candidate.interfaces[0] != owner
            {
                return Ok(false);
            }
            reserve_declaration(declaration_work, PINNED_PLATFORM_DECLARATIONS)?;
            let Some((work, obligations)) =
                crate::native_hierarchy::exact_platform_interface_obligations(owner.as_ref())
            else {
                return Ok(false);
            };
            if work != PINNED_PLATFORM_DECLARATIONS {
                return Ok(false);
            }
            if obligations.iter().any(|method| {
                !exact_public_method(
                    candidate,
                    &implementations,
                    &method.name,
                    &method.parameters,
                    &method.return_type,
                    &[],
                )
            }) {
                return Ok(false);
            }
            continue;
        };
        let Some(work) = class_declaration_work(interface) else {
            return Err(());
        };
        reserve_declaration(declaration_work, work)?;
        if !metadata_preflight(interface, declaration_work, false)?
            || interface.superclass.as_deref() != Some("Ljava/lang/Object;")
            || interface.access_flags & 0x600 != 0x600
            || interface.access_flags & !(1 | 0x200 | 0x400 | 0x1000) != 0
            || !matches!(interface.access_flags & 7, 0 | 1)
            || (interface.access_flags & 1 == 0
                && package(&candidate.descriptor) != package(&interface.descriptor))
            || !unique_java_method_signatures(interface)
            || !unique_type_list(&interface.interfaces)
            || !crate::native_java::source_header_authority(interface)
        {
            return Ok(false);
        }
        for method in &interface.methods {
            if method.name.as_ref() == "<init>" {
                return Ok(false);
            }
            if method.name.as_ref() == "<clinit>" {
                if method.access_flags != 0x10008
                    || !method.parameters.is_empty()
                    || method.return_type.as_ref() != "V"
                    || !method.thrown_types.is_empty()
                    || method.code.is_none()
                {
                    return Ok(false);
                }
                continue;
            }
            if method.access_flags & 0x8 != 0 {
                return Ok(false);
            }
            let legal_abstract = method.access_flags == 0x401 && method.code.is_none();
            let legal_default = method.access_flags == 1 && method.code.is_some();
            if !(legal_abstract || legal_default)
                || !crate::native_java::ordinary_method_declaration_authority(interface, method)
                || !exact_public_implementation(candidate, &implementations, method)
            {
                return Ok(false);
            }
        }
        pending.extend(
            interface
                .interfaces
                .iter()
                .cloned()
                .map(|owner| (owner, false)),
        );
    }
    Ok(true)
}

fn charge_value(
    class: &DexClass,
    value: &DexValue,
    remaining: &mut usize,
    depth: usize,
) -> Result<bool, ()> {
    if depth > 64 {
        return Ok(false);
    }
    reserve_declaration(remaining, 1)?;
    let symbol_len = match value {
        DexValue::String(index) => class.symbols.strings.get(*index as usize).map(String::len),
        DexValue::Type(index) => class
            .symbols
            .types
            .get(*index as usize)
            .map(|value| value.len()),
        DexValue::MethodType(index) => {
            class
                .symbols
                .protos
                .get(*index as usize)
                .and_then(|(ret, args)| {
                    args.iter()
                        .try_fold(ret.len(), |length, arg| length.checked_add(arg.len()))
                })
        }
        DexValue::Field(index) | DexValue::Enum(index) => class
            .symbols
            .fields
            .get(*index as usize)
            .and_then(|(owner, ty, name)| {
                class
                    .symbols
                    .types
                    .get(*owner as usize)?
                    .len()
                    .checked_add(class.symbols.types.get(*ty as usize)?.len())?
                    .checked_add(class.symbols.strings.get(*name as usize)?.len())
            }),
        DexValue::Method(index) => {
            class
                .symbols
                .methods
                .get(*index as usize)
                .and_then(|(owner, proto, name)| {
                    let (ret, args) = class.symbols.protos.get(*proto as usize)?;
                    class
                        .symbols
                        .types
                        .get(*owner as usize)?
                        .len()
                        .checked_add(class.symbols.strings.get(*name as usize)?.len())?
                        .checked_add(ret.len())?
                        .checked_add(
                            args.iter()
                                .try_fold(0usize, |length, arg| length.checked_add(arg.len()))?,
                        )
                })
        }
        DexValue::MethodHandle(_) => Some(1),
        DexValue::Array(values) => {
            for value in values {
                if !charge_value(class, value, remaining, depth + 1)? {
                    return Ok(false);
                }
            }
            Some(0)
        }
        DexValue::Annotation { type_idx, elements } => {
            let Some(ty) = class.symbols.types.get(*type_idx as usize) else {
                return Ok(false);
            };
            reserve_declaration(remaining, ty.len())?;
            for (name, value) in elements {
                let Some(name) = class.symbols.strings.get(*name as usize) else {
                    return Ok(false);
                };
                reserve_declaration(remaining, name.len())?;
                if !charge_value(class, value, remaining, depth + 1)? {
                    return Ok(false);
                }
            }
            Some(0)
        }
        _ => Some(0),
    };
    let Some(symbol_len) = symbol_len else {
        return Ok(false);
    };
    reserve_declaration(remaining, symbol_len)?;
    Ok(true)
}

fn charge_annotation_set(
    class: &DexClass,
    set: &DexAnnotationSet,
    remaining: &mut usize,
    reject_signature: bool,
) -> Result<bool, ()> {
    reserve_declaration(remaining, 1)?;
    for annotation in set.iter() {
        reserve_declaration(remaining, 1)?;
        let Some(ty) = class.symbols.types.get(annotation.type_idx as usize) else {
            return Ok(false);
        };
        reserve_declaration(remaining, ty.len())?;
        if reject_signature && ty.as_ref() == "Ldalvik/annotation/Signature;" {
            return Ok(false);
        }
        for (name, value) in &annotation.elements {
            reserve_declaration(remaining, 1)?;
            let Some(name) = class.symbols.strings.get(*name as usize) else {
                return Ok(false);
            };
            reserve_declaration(remaining, name.len())?;
            if !charge_value(class, value, remaining, 0)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn metadata_preflight(
    class: &DexClass,
    remaining: &mut usize,
    reject_signature: bool,
) -> Result<bool, ()> {
    for value in &class.static_values {
        if !charge_value(class, value, remaining, 0)? {
            return Ok(false);
        }
    }
    if class.annotations_offset == 0 {
        return Ok(true);
    }
    let Some(directory) = class.symbols.annotations.get(&class.annotations_offset) else {
        return Ok(false);
    };
    reserve_declaration(remaining, 1)?;
    reserve_declaration(remaining, 1)?;
    if let Some(set) = &directory.class
        && !charge_annotation_set(class, set, remaining, reject_signature)?
    {
        return Ok(false);
    }
    for set in &directory.fields {
        reserve_declaration(remaining, 1)?;
        if let Some(set) = set
            && !charge_annotation_set(class, set, remaining, false)?
        {
            return Ok(false);
        }
    }
    for set in &directory.methods {
        reserve_declaration(remaining, 1)?;
        if let Some(set) = set
            && !charge_annotation_set(class, set, remaining, false)?
        {
            return Ok(false);
        }
    }
    for parameters in &directory.parameters {
        reserve_declaration(remaining, 1)?;
        for set in parameters {
            reserve_declaration(remaining, 1)?;
            if let Some(set) = set
                && !charge_annotation_set(class, set, remaining, false)?
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn metadata_shape(class: &DexClass, remaining: &mut usize) -> Result<bool, ()> {
    if !metadata_preflight(class, remaining, true)? {
        return Ok(false);
    }

    if class
        .fields
        .iter()
        .any(|field| !field.is_static && field.access_flags & (0x10 | 0x1000) != 0)
    {
        return Ok(false);
    }
    let Some(inner) = exact_annotation(class, "Ldalvik/annotation/InnerClass;") else {
        return Ok(false);
    };
    let Some(enclosing_class) = exact_annotation(class, "Ldalvik/annotation/EnclosingClass;")
    else {
        return Ok(false);
    };
    let Some(enclosing_method) = exact_annotation(class, "Ldalvik/annotation/EnclosingMethod;")
    else {
        return Ok(false);
    };
    // This first tranche covers only independent metadata or a class-level
    // enclosure. Method-local capture/presentation remains a separate proof.
    if enclosing_method.is_some() {
        return Ok(false);
    }
    let Some(inner) = inner else {
        return Ok(enclosing_class.is_none());
    };
    let Some(enclosing_class) = enclosing_class else {
        return Ok(false);
    };
    if inner.elements.len() != 2 || enclosing_class.elements.len() != 1 {
        return Ok(false);
    }
    let Some(DexValue::Type(enclosing_index)) = annotation_element(class, enclosing_class, "value")
    else {
        return Ok(false);
    };
    let Some(enclosing_owner) = class.symbols.types.get(enclosing_index as usize) else {
        return Ok(false);
    };
    let Some(enclosing_prefix) = enclosing_owner.strip_suffix(';') else {
        return Ok(false);
    };
    let Some(suffix) = class
        .descriptor
        .strip_prefix(enclosing_prefix)
        .and_then(|suffix| suffix.strip_prefix('$'))
        .and_then(|suffix| suffix.strip_suffix(';'))
    else {
        return Ok(false);
    };
    if suffix.is_empty() {
        return Ok(false);
    }
    let Some(DexValue::Int(flags)) = annotation_element(class, inner, "accessFlags") else {
        return Ok(false);
    };
    let flags = flags as u32;
    if !matches!(flags & 7, 0 | 1 | 2 | 4) || flags & !(7 | 8 | 0x10 | 0x400 | 0x1000) != 0 {
        return Ok(false);
    }
    let Some(name) = annotation_element(class, inner, "name") else {
        return Ok(false);
    };
    let instance_fields = class.fields.iter().any(|field| !field.is_static);
    match name {
        // Anonymous/local flat presentations can be constructor-only equivalent
        // only when there is no state that could encode an enclosing capture.
        DexValue::Null => Ok(!instance_fields),
        // Ordinary fields are permitted only for an exact named static member.
        DexValue::String(index) => Ok(flags & 8 != 0
            && class
                .symbols
                .strings
                .get(index as usize)
                .is_some_and(|name| name == suffix)),
        _ => Ok(false),
    }
}

/// Source ownership for an erased zero-argument super constructor. This proves
/// only a flat presentation with no enclosing capture; caller identity and
/// forwarding behavior remain separate obligations in constructor recovery.
pub(super) fn independent_source_owner(class: &DexClass, remaining: &mut usize) -> bool {
    if class
        .methods
        .iter()
        .any(|method| method.name.as_ref() == "<init>")
    {
        return false;
    }
    metadata_shape(class, remaining) == Ok(true)
        && crate::native_java::flat_source_prefix_authority(class)
}

fn candidate_for(
    class: &DexClass,
    owners: &HashMap<&str, &DexClass>,
    duplicates: &HashSet<&str>,
    declaration_work: &mut usize,
) -> Result<Option<Candidate>, ()> {
    if duplicates.contains(class.descriptor.as_ref())
        || !class.descriptor.contains('$')
        || class.access_flags & (0x200 | 0x400 | 0x2000 | 0x4000) != 0
        || !matches!(class.access_flags & 7, 0 | 1)
    {
        return Ok(None);
    }
    let Some(member_work) = class_declaration_work(class) else {
        return Err(());
    };
    reserve_declaration(declaration_work, member_work)?;
    if !unique_java_method_signatures(class)
        || !unique_type_list(&class.interfaces)
        || class.methods.iter().any(|method| {
            matches!(method.name.as_ref(), "<init>" | "<clinit>")
                || method.access_flags & 0x400 != 0
        })
        || !metadata_shape(class, declaration_work)?
        || !crate::native_java::flat_source_prefix_authority(class)
    {
        return Ok(None);
    }

    let Some(parent_name) = class.superclass.as_deref() else {
        return Ok(None);
    };
    if duplicates.contains(parent_name) {
        return Ok(None);
    }
    if owners.contains_key("Ljava/lang/Object;") || duplicates.contains("Ljava/lang/Object;") {
        return Ok(None);
    }
    let (thrown_types, loaded_parent) = if parent_name == "Ljava/lang/Object;" {
        if !super::platform_constructor(parent_name, &[]).is_some_and(|types| types.is_empty()) {
            return Ok(None);
        }
        (vec![], None)
    } else {
        let Some(parent) = owners.get(parent_name).copied() else {
            return Ok(None);
        };
        let Some(parent_work) = class_declaration_work(parent) else {
            return Err(());
        };
        reserve_declaration(declaration_work, parent_work)?;
        // The original allocation caller and generated child's implicit
        // super() must both have authority. This first tranche therefore
        // accepts only a public loaded parent and public retained constructor.
        if !metadata_preflight(parent, declaration_work, false)?
            || parent.access_flags & (0x10 | 0x200 | 0x2000 | 0x4000) != 0
            || parent.access_flags & 7 != 1
            || !unique_java_method_signatures(parent)
            || !unique_type_list(&parent.interfaces)
            || final_parent_method_conflict(class, parent)
            || !crate::native_java::source_header_authority(parent)
            || parent.superclass.as_deref() != Some("Ljava/lang/Object;")
        {
            return Ok(None);
        }
        let Some(target) = forwarding_target(class, parent, &[]) else {
            return Ok(None);
        };
        if target.access_flags != 0x10001
            || !target.thrown_types.is_empty()
            || !exact_trivial_noarg_forwarder(parent, target, &Arc::from("Ljava/lang/Object;"))
        {
            return Ok(None);
        }
        (target.thrown_types.clone(), Some(parent))
    };
    if !abstract_obligations_proven(class, loaded_parent, owners, duplicates, declaration_work)? {
        return Ok(None);
    }
    Ok(Some(Candidate {
        parent: Arc::from(parent_name),
        thrown_types,
        access_flags: class.access_flags & 7,
    }))
}

fn pool_relevance(class: &DexClass, candidates: &BTreeMap<Arc<str>, Candidate>) -> PoolRelevance {
    let allocated = class
        .symbols
        .types
        .iter()
        .enumerate()
        .filter_map(|(index, owner)| {
            if !candidates.contains_key(owner) {
                return None;
            }
            Some((u32::try_from(index).ok()?, owner.clone()))
        })
        .collect();
    let constructors = class
        .symbols
        .methods
        .iter()
        .enumerate()
        .filter_map(|(index, (owner, _, name))| {
            let owner = class.symbols.types.get(*owner as usize)?;
            if !candidates.contains_key(owner)
                || class.symbols.strings.get(*name as usize)?.as_str() != "<init>"
            {
                return None;
            }
            Some((u32::try_from(index).ok()?, owner.clone()))
        })
        .collect();
    PoolRelevance {
        allocated,
        constructors,
    }
}

fn raw_mentions(method: &DexMethod, relevant: &PoolRelevance) -> HashSet<Arc<str>> {
    let Some(code) = method.code.as_ref() else {
        return HashSet::new();
    };
    let mut owners = HashSet::new();
    for words in code.instructions.windows(2) {
        let opcode = words[0] as u8;
        let index = u32::from(words[1]);
        if opcode == 0x22
            && let Some(owner) = relevant.allocated.get(&index)
        {
            owners.insert(owner.clone());
        }
        if matches!(opcode, 0x6e..=0x72 | 0x74..=0x78)
            && let Some(owner) = relevant.constructors.get(&index)
        {
            owners.insert(owner.clone());
        }
    }
    owners
}

fn decoded_mentions(ir: &DecodedMethod, relevant: &PoolRelevance) -> HashSet<Arc<str>> {
    ir.instructions
        .iter()
        .filter_map(|instruction| {
            let reference = instruction.reference?;
            if instruction.opcode == 0x22 && reference.kind == PoolKind::Type {
                return relevant.allocated.get(&reference.index).cloned();
            }
            if matches!(instruction.opcode, 0x6e..=0x72 | 0x74..=0x78)
                && reference.kind == PoolKind::Method
            {
                return relevant.constructors.get(&reference.index).cloned();
            }
            None
        })
        .collect()
}

fn exact_call(calls: &[BoundCall], pc: usize) -> Option<&BoundCall> {
    let mut matches = calls.iter().filter(|call| call.pc == pc);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn exact_site(
    candidate: &Candidate,
    position: usize,
    code: &crate::native_dex::DexCode,
    ir: &DecodedMethod,
    cfg: &ControlFlowGraph,
    calls: &[BoundCall],
    ssa: &SsaMethod,
) -> bool {
    let Some(new) = ir.instructions.get(position) else {
        return false;
    };
    let Some(init) = ir.instructions.get(position + 1) else {
        return false;
    };
    let (Ok(new_pc), Ok(init_pc)) = (u32::try_from(new.pc), u32::try_from(init.pc)) else {
        return false;
    };
    if new.opcode != 0x22
        || new.writes.len() != 1
        || new.writes[0].kind != ValueKind::Reference
        || init.pc != new.pc + new.width
        || !matches!(init.opcode, 0x70 | 0x76)
        || code.try_regions.iter().any(|region| {
            (region.start <= new_pc && new_pc < region.end)
                || (region.start <= init_pc && init_pc < region.end)
        })
    {
        return false;
    }
    let (Some(new_block), Some(init_block)) =
        (cfg.block_at.get(&new.pc), cfg.block_at.get(&init.pc))
    else {
        return false;
    };
    if new_block != init_block || ssa.reachable.get(*new_block) != Some(&true) {
        return false;
    }
    let Some(call) = exact_call(calls, init.pc) else {
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
    if call.kind != CallKind::Direct
        || declaring_type != &candidate.parent
        || name.as_ref() != "<init>"
        || call.return_type.as_ref() != "V"
        || !call.arguments.is_empty()
        || call.receiver.as_ref().map(|receiver| receiver.register) != Some(new.writes[0].register)
        || init.reads.len() != 1
        || init.reads[0].kind.word_count() != 1
    {
        return false;
    }
    let Some(new_ssa) = ssa
        .instructions
        .iter()
        .find(|instruction| instruction.pc == new.pc)
    else {
        return false;
    };
    let Some(init_ssa) = ssa
        .instructions
        .iter()
        .find(|instruction| instruction.pc == init.pc)
    else {
        return false;
    };
    let ([new_write], [init_read]) = (new_ssa.writes.as_slice(), init_ssa.reads.as_slice()) else {
        return false;
    };
    let ([new_value], [init_value]) = (new_write.words.as_slice(), init_read.words.as_slice())
    else {
        return false;
    };
    *new_value == *init_value
        && ssa.definitions.get(*new_value).is_some_and(|definition| {
            matches!(
                definition.kind,
                DefinitionKind::Instruction { pc, word: 0, block }
                    if pc == new.pc && block == *new_block
            )
        })
        && new
            .reference
            .is_some_and(|reference| reference.kind == PoolKind::Type)
}

fn validate_method(
    class: &DexClass,
    method: &DexMethod,
    relevant: &PoolRelevance,
    candidates: &BTreeMap<Arc<str>, Candidate>,
    mentions: &HashSet<Arc<str>>,
    observations: &mut Observations,
) {
    let Some(code) = method.code.as_ref() else {
        observations.invalidate(mentions);
        return;
    };
    let Ok(ir) = DecodedMethod::decode(code) else {
        observations.invalidate(mentions);
        return;
    };
    let Some((cfg, bound)) = super::complete_layered_method_validation(class, method, &ir) else {
        observations.invalidate(mentions);
        return;
    };
    let decoded_mentions = decoded_mentions(&ir, relevant);
    if decoded_mentions.is_empty() {
        return;
    }
    let Some(proof_work) = observations.reserve_method(method) else {
        observations.invalidate(mentions);
        return;
    };
    let Ok(ssa) =
        SsaMethod::build_with_local_and_dominator_limits(code, &ir, &cfg, proof_work, proof_work)
    else {
        observations.invalidate(&decoded_mentions);
        return;
    };

    for call in &bound.calls {
        if let CallTarget::Method {
            declaring_type,
            name,
            ..
        } = &call.target
            && name.as_ref() == "<init>"
            && candidates.contains_key(declaring_type)
        {
            observations.invalid.insert(declaring_type.clone());
        }
    }
    for (position, instruction) in ir.instructions.iter().enumerate() {
        if instruction.opcode != 0x22 {
            continue;
        }
        let Some(reference) = instruction.reference else {
            continue;
        };
        let Some(owner) = relevant.allocated.get(&reference.index) else {
            continue;
        };
        let Some(candidate) = candidates.get(owner) else {
            observations.invalid.insert(owner.clone());
            continue;
        };
        if candidate.access_flags & 1 == 0 && package(&class.descriptor) != package(owner) {
            observations.invalid.insert(owner.clone());
            continue;
        }
        if exact_site(candidate, position, code, &ir, &cfg, &bound.calls, &ssa) {
            observations.observe(owner.clone());
        } else {
            observations.invalid.insert(owner.clone());
        }
    }
}

pub(super) fn commit(
    classes: &[&DexClass],
    owners: &HashMap<&str, &DexClass>,
    duplicates: &HashSet<&str>,
    result: &mut HashMap<Arc<str>, BTreeMap<String, RecoveredConstructor>>,
) {
    let mut remaining_declaration_work = MAX_DECLARATION_WORK;
    let mut ordered = classes.to_vec();
    ordered.sort_by(|left, right| left.descriptor.cmp(&right.descriptor));
    let mut candidates = BTreeMap::new();
    for class in ordered {
        if candidates.len() >= MAX_CANDIDATES {
            return;
        }
        let candidate =
            match candidate_for(class, owners, duplicates, &mut remaining_declaration_work) {
                Ok(Some(candidate)) => candidate,
                Ok(None) => continue,
                Err(()) => return,
            };
        candidates.insert(class.descriptor.clone(), candidate);
    }
    if candidates.is_empty() {
        return;
    }

    let mut observations = Observations::new();
    let mut pools = HashMap::new();
    for class in classes {
        let pool_key = Arc::as_ptr(&class.symbols) as usize;
        if let std::collections::hash_map::Entry::Vacant(entry) = pools.entry(pool_key) {
            let pool_work = class
                .symbols
                .types
                .len()
                .checked_add(class.symbols.methods.len());
            let Some(pool_work) = pool_work else {
                return;
            };
            if !observations.reserve_prefilter(pool_work) {
                return;
            }
            entry.insert(pool_relevance(class, &candidates));
        }
        let relevant = pools.get(&pool_key).expect("inserted pool relevance");
        if relevant.allocated.is_empty() && relevant.constructors.is_empty() {
            continue;
        }
        for method in &class.methods {
            let Some(code) = method.code.as_ref() else {
                continue;
            };
            if !observations.reserve_prefilter(code.instructions.len()) {
                return;
            }
            let mentions = raw_mentions(method, relevant);
            if mentions.is_empty() {
                continue;
            }
            validate_method(
                class,
                method,
                relevant,
                &candidates,
                &mentions,
                &mut observations,
            );
            if observations.exhausted {
                return;
            }
        }
    }
    if observations.exhausted {
        return;
    }

    for (owner, candidate) in candidates {
        if observations.invalid.contains(&owner)
            || observations.counts.get(&owner).copied().unwrap_or(0) == 0
            || result
                .get(&owner)
                .is_some_and(|entries| entries.contains_key(""))
        {
            continue;
        }
        result.entry(owner).or_default().insert(
            String::new(),
            RecoveredConstructor {
                access_flags: candidate.access_flags,
                parent: candidate.parent.clone(),
                invoked_owner: candidate.parent,
                parameters: vec![],
                thrown_types: candidate.thrown_types,
                strict_allocation_order: true,
                observed_allocation: true,
                observed_super: false,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexAnnotation, DexAnnotationDirectory, DexCode, DexSymbols};
    use std::collections::BTreeMap;

    fn method(words: usize, registers: u16) -> DexMethod {
        DexMethod {
            declaring_type: Arc::from("Lsample/Caller;"),
            name: Arc::from("make"),
            return_type: Arc::from("V"),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers,
                ins: 0,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![0; words],
            }),
        }
    }

    #[test]
    fn independent_budgets_fail_atomically() {
        let mut observations = Observations::with_limits(3, 6);
        assert!(observations.reserve_prefilter(3));
        assert!(!observations.reserve_prefilter(1));
        assert!(observations.exhausted);

        let mut observations = Observations::with_limits(3, 65_536);
        assert_eq!(observations.reserve_method(&method(2, 3)), Some(32_768));
        assert_eq!(observations.remaining_proof_work, 0);
        assert_eq!(observations.reserve_method(&method(1, 1)), None);
        assert!(!observations.exhausted);

        let mut observations = Observations::with_limits(3, 65_535);
        assert_eq!(observations.reserve_method(&method(2, 3)), None);
        assert!(!observations.exhausted);
        assert_eq!(observations.remaining_proof_work, 0);
    }

    #[test]
    fn proof_exhaustion_invalidates_only_unproved_owners_and_scan_continues() {
        let proved: Arc<str> = Arc::from("Lsample/Proved$1;");
        let incomplete: Arc<str> = Arc::from("Lsample/Incomplete$2;");
        let later: Arc<str> = Arc::from("Lsample/Later$3;");
        let mut observations = Observations::with_limits(100, 65_536);
        assert_eq!(observations.reserve_method(&method(2, 3)), Some(32_768));
        observations.observe(proved.clone());
        observations.observe(incomplete.clone());
        assert_eq!(observations.reserve_method(&method(2, 3)), None);
        observations.invalidate([&incomplete]);
        assert!(observations.reserve_prefilter(10));
        assert_eq!(observations.reserve_method(&method(2, 3)), None);
        observations.invalidate([&later]);
        assert!(!observations.exhausted);
        assert_eq!(observations.counts.get(&proved), Some(&1));
        assert!(!observations.invalid.contains(&proved));
        assert!(observations.invalid.contains(&incomplete));
        assert!(observations.invalid.contains(&later));
        // A later unproved caller also withdraws an earlier positive proof.
        observations.invalidate([&proved]);
        assert!(observations.invalid.contains(&proved));
        // Unlike local proof exhaustion, an incomplete world scan is atomic.
        assert!(!observations.reserve_prefilter(100));
        assert!(observations.exhausted);
    }

    #[test]
    fn raw_operand_window_requires_a_canonical_instruction() {
        let method = DexMethod {
            code: Some(DexCode {
                registers: 1,
                ins: 0,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                // The literal word has low byte 0x22 and the following nop is
                // pool index zero to the raw two-word prefilter. Canonical
                // decoding sees const/16, nop, return-void and no allocation.
                instructions: vec![0x0013, 0x0022, 0x0000, 0x000e],
            }),
            ..method(0, 1)
        };
        let owner: Arc<str> = Arc::from("Lsample/Leaf$4;");
        let relevant = PoolRelevance {
            allocated: HashMap::from([(0, owner.clone())]),
            constructors: HashMap::new(),
        };
        assert_eq!(raw_mentions(&method, &relevant), HashSet::from([owner]));
        let decoded = DecodedMethod::decode(method.code.as_ref().unwrap()).unwrap();
        assert!(decoded_mentions(&decoded, &relevant).is_empty());
    }

    #[test]
    fn observation_limit_failure_cannot_commit_partial_counts() {
        let mut observations = Observations::with_limits(1, 1);
        observations.total = MAX_OBSERVATIONS;
        observations.observe(Arc::from("Lsample/Leaf;"));
        assert!(observations.exhausted);
        assert!(observations.counts.is_empty());
    }

    #[test]
    fn sparse_metadata_slots_are_charged_before_candidate_rendering() {
        let mut directories = BTreeMap::new();
        directories.insert(
            1,
            Arc::new(DexAnnotationDirectory {
                fields: vec![None; 128],
                ..Default::default()
            }),
        );
        let class = DexClass {
            symbols: Arc::new(DexSymbols {
                annotations: directories,
                ..Default::default()
            }),
            descriptor: Arc::from("Lsample/Sparse;"),
            superclass: Some(Arc::from("Ljava/lang/Object;")),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 1,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        };
        // Directory + class slot + 128 field slots require 130 units.
        let mut remaining = 129;
        assert_eq!(metadata_preflight(&class, &mut remaining, true), Err(()));
    }

    #[test]
    fn loaded_parent_metadata_uses_the_same_recursive_budget() {
        let nested = (0..65).fold(DexValue::Null, |value, _| DexValue::Array(vec![value]));
        let parent = DexClass {
            symbols: Arc::new(DexSymbols::default()),
            descriptor: Arc::from("Lsample/Parent;"),
            superclass: Some(Arc::from("Ljava/lang/Object;")),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 1,
            static_values: vec![nested],
            fields: vec![],
            methods: vec![],
        };
        let mut remaining = MAX_DECLARATION_WORK;
        assert!(!metadata_preflight(&parent, &mut remaining, false).unwrap());
    }

    #[test]
    fn generic_signature_gate_is_scoped_to_the_class_header() {
        let signature = Arc::new(DexAnnotation {
            visibility: 2,
            type_idx: 0,
            elements: vec![],
        });
        let mut directories = BTreeMap::new();
        directories.insert(
            1,
            Arc::new(DexAnnotationDirectory {
                methods: vec![Some(vec![signature.clone()].into())],
                ..Default::default()
            }),
        );
        let mut class = DexClass {
            symbols: Arc::new(DexSymbols {
                types: vec![Arc::from("Ldalvik/annotation/Signature;")],
                annotations: directories,
                ..Default::default()
            }),
            descriptor: Arc::from("Lsample/Leaf;"),
            superclass: Some(Arc::from("Ljava/lang/Object;")),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 1,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        };
        let mut remaining = 64;
        assert!(metadata_preflight(&class, &mut remaining, true).unwrap());

        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        let directory = Arc::get_mut(symbols.annotations.get_mut(&1).unwrap()).unwrap();
        directory.class = Some(vec![signature].into());
        let mut remaining = 64;
        assert!(!metadata_preflight(&class, &mut remaining, true).unwrap());
    }
}
