//! Exact SDK-35 Future declaration restoration, independent of body effects.
use super::*;
const FUTURE: &str = "Ljava/util/concurrent/Future;";
const RUNNABLE: &str = "Ljava/lang/Runnable;";
const EXECUTION: &str = "Ljava/util/concurrent/ExecutionException;";
const INTERRUPTED: &str = "Ljava/lang/InterruptedException;";
const TIMEOUT: &str = "Ljava/util/concurrent/TimeoutException;";

pub(super) fn signature(method: &crate::native_dex::DexMethod) -> bool {
    method.name.as_ref() == "get"
        && (method.parameters.is_empty()
            || method
                .parameters
                .iter()
                .map(AsRef::as_ref)
                .collect::<String>()
                == "JLjava/util/concurrent/TimeUnit;")
        && method.return_type.starts_with('L')
        && method.return_type.ends_with(';')
}

// Complete bounded width proof, deliberately independent of inference eligibility.
// Calls and explicit throws need a separate producer/effect proof and stay unknown.
pub(super) fn checked_effects_closed(method: &crate::native_dex::DexMethod) -> bool {
    if !signature(method) {
        return false;
    }
    let Some(code) = &method.code else {
        return false;
    };
    let words = &code.instructions;
    if words.is_empty()
        || words.len() > 4096
        || code.registers < code.ins
        || usize::from(code.tries) != code.try_regions.len()
    {
        return false;
    }
    let mut pc = 0;
    while pc < words.len() {
        let Ok((width, payload)) = crate::native_cfg::instruction_width(words, pc) else {
            return false;
        };
        if payload
            || width == 0
            || pc + width > words.len()
            || matches!(words[pc] as u8,0x27 | 0x6e..=0x72 | 0x74..=0x78 | 0xfa..=0xfd)
        {
            return false;
        }
        pc += width;
    }
    pc == words.len()
        && crate::native_ir::DecodedMethod::decode(code).is_ok()
        && crate::native_cfg::ControlFlowGraph::build(code).is_ok()
}

impl TypeHierarchy {
    // Raw declaration proof supplies dependencies while the caller index is built.
    // It does not authorize emission until every loaded caller obligation closes.
    pub(super) fn raw_future_header(
        &self,
        class: &DexClass,
        method: &crate::native_dex::DexMethod,
    ) -> Vec<&'static str> {
        if !signature(method)
            || method.access_flags & 15 != 1
            || method.access_flags & (0x100 | 0x400) != 0
            || !method.thrown_types.is_empty()
            || method.declaring_type != class.descriptor
            || self.assignable(&class.descriptor, FUTURE) != Relation::Proven
            || self.ambiguous.contains(class.descriptor.as_ref())
            || self
                .override_ambiguous_owners
                .contains(class.descriptor.as_ref())
        {
            return Vec::new();
        }
        if !class.methods.is_empty() {
            let mut matches = class
                .methods
                .iter()
                .filter(|m| m.name == method.name && m.parameters == method.parameters);
            let Some(actual) = matches.next() else {
                return Vec::new();
            };
            if matches.next().is_some()
                || actual.return_type != method.return_type
                || actual.access_flags != method.access_flags
                || actual.declaring_type != class.descriptor
            {
                return Vec::new();
            }
        } else {
            let Some(summary) = self.loaded_call_families.get(class.descriptor.as_ref()) else {
                return Vec::new();
            };
            let candidate = match summary
                .methods
                .get(&method.name)
                .and_then(|f| f.get(&method.parameters.len()))
            {
                Some(Some(candidate)) => Some(candidate),
                Some(None) => summary
                    .exact_overloads
                    .as_ref()
                    .and_then(|e| e.get(&method.name))
                    .and_then(|f| f.get(&method.parameters))
                    .and_then(Option::as_ref),
                _ => None,
            };
            if !candidate.is_some_and(|m| {
                m.parameters == method.parameters
                    && m.return_type == method.return_type
                    && m.access_flags == method.access_flags
            }) {
                return Vec::new();
            }
        }
        let signature = format!(
            "get({})",
            method
                .parameters
                .iter()
                .map(AsRef::as_ref)
                .collect::<String>()
        );
        if !self
            .indexed_candidate_contracts
            .contains(&format!("{}->{signature}", class.descriptor))
        {
            return Vec::new();
        }
        let mut types = vec![EXECUTION, INTERRUPTED];
        if !method.parameters.is_empty() {
            types.push(TIMEOUT);
        }
        let mut pending: Vec<_> = class
            .superclass
            .iter()
            .chain(class.interfaces.iter())
            .cloned()
            .map(|owner| (owner, false))
            .collect();
        let mut seen = HashSet::from([class.descriptor.clone()]);
        let mut active = HashSet::from([class.descriptor.clone()]);
        let mut sdk_seen = false;
        for _ in 0..(MAX_QUERY_NODES * 2) {
            let Some((owner, exit)) = pending.pop() else {
                return if sdk_seen { types } else { Vec::new() };
            };
            if exit {
                active.remove(owner.as_ref());
                continue;
            }
            if active.contains(owner.as_ref()) {
                return Vec::new();
            }
            if !seen.insert(owner.clone()) {
                continue;
            }
            active.insert(owner.clone());
            if self.ambiguous.contains(owner.as_ref())
                || self.override_ambiguous_owners.contains(owner.as_ref())
            {
                return Vec::new();
            }
            let key = format!("{owner}->{signature}");
            if self.ambiguous_method_contracts.contains(&key) {
                return Vec::new();
            }
            if let Some(contract) = self.method_contracts.get(&key)
                && contract.access_flags & 2 == 0
            {
                if contract.access_flags & 15 != 1
                    || self.assignable(&method.return_type, &contract.return_type)
                        != Relation::Proven
                    || contract.inferred_throws
                {
                    return Vec::new();
                }
                if contract.thrown_types.is_empty() {
                    if contract.access_flags & (0x100 | 0x400) != 0
                        || self.assignable(&owner, FUTURE) != Relation::Proven
                    {
                        return Vec::new();
                    }
                } else {
                    types.retain(|ty| {
                        contract
                            .thrown_types
                            .iter()
                            .any(|declared| self.assignable(ty, declared) == Relation::Proven)
                    });
                    if types.is_empty() {
                        return Vec::new();
                    }
                }
            }
            if owner.as_ref() == OBJECT {
                active.remove(owner.as_ref());
                continue;
            }
            if matches!(owner.as_ref(), FUTURE | RUNNABLE) {
                if self.loaded_owners.contains(owner.as_ref()) {
                    return Vec::new();
                }
                sdk_seen |= owner.as_ref() == FUTURE;
                let Some((parent, interfaces)) = platform_exceptions().types.get(owner.as_ref())
                else {
                    return Vec::new();
                };
                pending.push((owner, true));
                pending.extend(
                    parent
                        .iter()
                        .chain(interfaces.iter())
                        .map(|p| (Arc::from(p.as_str()), false)),
                );
                continue;
            }
            if !self.loaded_owners.contains(owner.as_ref())
                && known_platform_interface_excludes_signature(&owner, &signature)
            {
                active.remove(owner.as_ref());
                continue;
            }
            if !self.loaded_owners.contains(owner.as_ref()) {
                return Vec::new();
            }
            let Some(entry) = self.entries.get(owner.as_ref()) else {
                return Vec::new();
            };
            pending.push((owner, true));
            pending.extend(
                entry
                    .superclass
                    .iter()
                    .chain(entry.interfaces.iter())
                    .cloned()
                    .map(|p| (p, false)),
            );
        }
        Vec::new()
    }
    pub(crate) fn source_future_header(
        &self,
        class: &DexClass,
        method: &crate::native_dex::DexMethod,
    ) -> Vec<&'static str> {
        let types = self.raw_future_header(class, method);
        if types.iter().all(|ty| {
            self.checked_callers
                .permits(&io_method_key(class, method), ty, self)
        }) {
            types
        } else {
            Vec::new()
        }
    }
}
