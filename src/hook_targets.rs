//! Exact DEX declaration targets for loaded, possible receivers.
use crate::{
    method_lookup,
    native_dex::{DexClass, DexMethod},
    native_engine::{
        NativeDexEngine,
        disassembly::{method_id, type_name},
    },
    package_scope::PackageScope,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

const DIRECT: u32 = 0x2 | 0x8;
const ABSTRACT: u32 = 0x400;
const INTERFACE: u32 = 0x200;
const MAX_WORK: usize = 1_500_000;
const MAX_RECEIVERS: usize = 4096;
const MAX_ANCESTORS: usize = 256;
const PREVIEW: usize = 8;
const OBJECT: &str = "Ljava/lang/Object;";

struct Work<'a> {
    left: usize,
    cancel: &'a AtomicBool,
    exhausted: bool,
}
impl Work<'_> {
    fn take(&mut self, n: usize) -> Result<bool> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "CANCELLED");
        let Some(left) = self.left.checked_sub(n) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.left = left;
        Ok(true)
    }
}

fn same_member(candidate: &DexMethod, root: &DexMethod) -> bool {
    candidate.name == root.name
        && candidate.parameters == root.parameters
        && candidate.return_type == root.return_type
        && candidate.access_flags & DIRECT == 0
}

pub(crate) fn class_method<'a>(class: &'a DexClass, root: &DexMethod) -> Option<&'a DexMethod> {
    let mut candidates = class.methods.iter().filter(|m| same_member(m, root));
    let candidate = candidates.next()?;
    candidates.next().is_none().then_some(candidate)
}

fn get_class<'a>(engine: &'a NativeDexEngine, descriptor: &str) -> Option<&'a DexClass> {
    engine.class(&type_name(descriptor))
}

fn package(descriptor: &str) -> &str {
    descriptor
        .rsplit_once('/')
        .map_or("", |(package, _)| package)
}

struct Ancestry<'a> {
    supers: Vec<&'a DexClass>,
    interfaces: Vec<&'a DexClass>,
    external_super: bool,
    unknown: bool,
}
fn ancestry<'a>(
    engine: &'a NativeDexEngine,
    receiver: &'a DexClass,
    interface: bool,
    work: &mut Work<'_>,
) -> Result<Ancestry<'a>> {
    let mut result = Ancestry {
        supers: Vec::new(),
        interfaces: Vec::new(),
        external_super: false,
        unknown: false,
    };
    let mut seen = BTreeSet::new();
    let mut next = Some(receiver.descriptor.as_ref());
    let mut pending = Vec::new();
    while let Some(descriptor) = next.take() {
        if !work.take(1)? || !seen.insert(descriptor) || seen.len() > MAX_ANCESTORS {
            result.unknown = true;
            break;
        }
        let Some(class) = get_class(engine, descriptor) else {
            result.external_super = descriptor != OBJECT;
            result.unknown |= result.external_super;
            break;
        };
        result.supers.push(class);
        if interface {
            if pending.len().saturating_add(class.interfaces.len()) > MAX_ANCESTORS {
                result.unknown = true;
            } else {
                pending.extend(class.interfaces.iter().map(AsRef::as_ref));
            }
        }
        next = class.superclass.as_deref();
    }
    while let Some(descriptor) = pending.pop() {
        if !work.take(1)? || seen.len() >= MAX_ANCESTORS {
            result.unknown = true;
            break;
        }
        if !seen.insert(descriptor) {
            continue;
        }
        let Some(class) = get_class(engine, descriptor) else {
            result.unknown = true;
            continue;
        };
        result.interfaces.push(class);
        if pending.len().saturating_add(class.interfaces.len()) > MAX_ANCESTORS {
            result.unknown = true;
        } else {
            pending.extend(class.interfaces.iter().map(AsRef::as_ref));
        }
    }
    Ok(result)
}

fn class_target<'a>(
    root: &'a DexMethod,
    supers: &[&'a DexClass],
    work: &mut Work<'_>,
) -> Result<Option<&'a DexMethod>> {
    let mut target = root;
    for class in supers.iter().rev() {
        if class.descriptor == root.declaring_type {
            continue;
        }
        if !work.take(class.methods.len() + 1)? {
            return Ok(None);
        }
        if let Some(candidate) = class_method(class, root)
            && target.access_flags & 0x10 == 0
            && (target.access_flags & 0x5 != 0
                || package(&target.declaring_type) == package(&class.descriptor))
        {
            target = candidate;
        }
    }
    Ok((target.access_flags & ABSTRACT == 0).then_some(target))
}

fn interface_target<'a>(
    root: &DexMethod,
    ancestry: &Ancestry<'a>,
    work: &mut Work<'_>,
) -> Result<(Option<&'a DexMethod>, &'static str, Vec<String>)> {
    for class in &ancestry.supers {
        if !work.take(class.methods.len() + 1)? {
            return Ok((None, "work_budget", vec![]));
        }
        let Some(candidate) = class_method(class, root) else {
            continue;
        };
        if candidate.access_flags & ABSTRACT != 0 {
            return Ok((None, "abstract_declaration", vec![]));
        }
        if candidate.access_flags & 1 == 0 {
            return Ok((None, "unproven_interface_visibility", vec![]));
        }
        return Ok((Some(candidate), "resolved", vec![]));
    }
    let mut defaults = Vec::new();
    for class in &ancestry.interfaces {
        if !work.take(class.methods.len() + 1)? {
            return Ok((None, "work_budget", vec![]));
        }
        if let Some(method) = class_method(class, root)
            && method.access_flags & ABSTRACT == 0
            && defaults.len() < PREVIEW
        {
            defaults.push(method_id(method));
        }
    }
    defaults.sort_unstable();
    let status = if ancestry.external_super {
        "external_superclass"
    } else if defaults.len() > 1 {
        "potentially_ambiguous_interface_defaults"
    } else if !defaults.is_empty() {
        "interface_default_unresolved"
    } else if ancestry.unknown {
        "incomplete_interface_ancestry"
    } else {
        "implementation_unavailable"
    };
    Ok((None, status, defaults))
}

struct Target<'a> {
    method: &'a DexMethod,
    receiver_count: usize,
    preview: Vec<&'a str>,
}

struct Page {
    offset: usize,
    limit: usize,
    count: usize,
    bytes: usize,
    items: Vec<Value>,
    clipped: bool,
}
impl Page {
    fn add(&mut self, make: impl FnOnce() -> Value) -> Result<()> {
        let position = self.count;
        self.count += 1;
        if position < self.offset || self.items.len() >= self.limit || self.clipped {
            return Ok(());
        }
        let row = make();
        let bytes = serde_json::to_vec(&row)?.len();
        if self.bytes.saturating_add(bytes) > 512 * 1024 {
            ensure!(
                !self.items.is_empty(),
                "Hook target record exceeds response budget"
            );
            self.clipped = true;
        } else {
            self.items.push(row);
            self.bytes += bytes;
        }
        Ok(())
    }
}

pub(crate) fn query(engine: &NativeDexEngine, args: &Value, cancel: &AtomicBool) -> Result<Value> {
    let requested = args["method_id"].as_str().context("Missing method_id")?;
    let id = method_lookup::normalize(requested)?;
    ensure!(
        method_lookup::complete(&id),
        "Use an exact method descriptor, for example {}",
        method_lookup::EXAMPLE
    );
    let owner = method_lookup::owner(&id)?;
    let root_class = engine
        .class(owner)
        .context("Root class declaration unavailable")?;
    let root = method_lookup::resolve(root_class, &id)?;
    ensure!(
        !root.name.starts_with('<') && root.access_flags & DIRECT == 0,
        "Hook target expansion requires a virtual instance method"
    );
    let package = PackageScope::from_args(args)?;
    let mut page = Page {
        offset: args["offset"].as_u64().unwrap_or(0) as usize,
        limit: args["limit"].as_u64().unwrap_or(50) as usize,
        count: 0,
        bytes: 0,
        items: Vec::new(),
        clipped: false,
    };
    ensure!((1..=100).contains(&page.limit), "limit must be 1..100");
    let receivers = args["projection"] == "receivers";
    let interface = root_class.access_flags & INTERFACE != 0;
    let mut work = Work {
        left: MAX_WORK,
        cancel,
        exhausted: false,
    };
    let mut targets: BTreeMap<&str, Target<'_>> = BTreeMap::new();
    let (mut scanned, mut matched, mut unresolved, mut unproven) = (0, 0, 0, 0);
    let mut receiver_limit = false;
    for class in engine.graph_classes() {
        if !work.take(1)? {
            break;
        }
        if class.access_flags & (ABSTRACT | INTERFACE) != 0 {
            continue;
        }
        let name = type_name(&class.descriptor);
        if !package.matches(&name) {
            continue;
        }
        scanned += 1;
        let ancestry = ancestry(engine, class, interface, &mut work)?;
        if work.exhausted {
            break;
        }
        let Some(position) = (if interface {
            &ancestry.interfaces
        } else {
            &ancestry.supers
        })
        .iter()
        .position(|ancestor| ancestor.descriptor == root_class.descriptor) else {
            unproven += usize::from(ancestry.unknown);
            continue;
        };
        if matched == MAX_RECEIVERS {
            receiver_limit = true;
            break;
        }
        let (method, status, defaults) = if interface {
            interface_target(root, &ancestry, &mut work)?
        } else {
            (
                class_target(root, &ancestry.supers[..=position], &mut work)?,
                "abstract_declaration",
                Vec::new(),
            )
        };
        if work.exhausted {
            break;
        }
        matched += 1;
        if let Some(method) = method {
            let target = targets
                .entry(&method.declaring_type)
                .or_insert_with(|| Target {
                    method,
                    receiver_count: 0,
                    preview: Vec::new(),
                });
            target.receiver_count += 1;
            if !receivers && target.preview.len() < PREVIEW {
                target.preview.push(&class.descriptor);
            }
        } else {
            unresolved += 1;
        }
        if receivers {
            let implementation = method.map(method_id);
            page.add(|| json!({"possible_receiver_class_id":name,"installation_method_id":implementation,
                "declaring_class_id":method.map(|method| type_name(&method.declaring_type)),
                "status":method.map_or(status, |method| if method.declaring_type == class.descriptor { "declared" } else { "inherited" }),
                "bridge":method.map(|method| method.access_flags & 0x40 != 0),
                "interface_default_candidates":defaults,
                "interface_default_candidates_truncated":defaults.len() == PREVIEW,
                "runtime_receiver_class_id":Value::Null}))?;
        }
    }
    if !receivers {
        for target in targets.values() {
            page.add(|| json!({"installation_method_id":method_id(target.method),
                "declaring_class_id":type_name(&target.method.declaring_type),
                "possible_receiver_classes":target.preview.iter().map(|descriptor| type_name(descriptor)).collect::<Vec<_>>(),
                "possible_receiver_count":target.receiver_count,
                "receiver_preview_truncated":target.preview.len() < target.receiver_count,
                "bridge":target.method.access_flags & 0x40 != 0,
                "synthetic":target.method.access_flags & 0x1000 != 0,
                "native":target.method.access_flags & 0x100 != 0,
                "body_available":target.method.code.is_some(),
                "runtime_receiver_class_id":Value::Null}))?;
        }
    }
    ensure!(page.offset <= page.count, "Invalid hook target offset");
    let next = page.offset + page.items.len();
    Ok(
        json!({"requested_method_id":requested,"method_id":id,"projection":if receivers {"receivers"}else{"targets"},
        "items":page.items,"total":page.count,"offset":page.offset,"next_offset":(next<page.count).then_some(next),
        "installation_target_count":targets.len(),"possible_receiver_count":matched,
        "coverage":{"scan_complete":!work.exhausted&&!receiver_limit,"scoped_concrete_classes_scanned":scanned,
            "unproven_subtype_classes":unproven,"unresolved_receivers":unresolved,
            "work_budget_exhausted":work.exhausted,"receiver_limit_reached":receiver_limit,
            "loaded_receiver_limit":MAX_RECEIVERS,"package_scope":package.describe()},
        "scope":"Possible loaded receivers and exact declaring DEX methods. Receiver projection pages every matched receiver, including unresolved declarations. Runtime receiver identity requires observation."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use std::sync::Arc;

    fn ty(name: &str) -> Arc<str> {
        format!("L{};", name.replace('.', "/")).into()
    }

    fn fixture(
        name: &str,
        super_name: &str,
        interfaces: &[&str],
        access_flags: u32,
        methods: &[(&str, u32)],
    ) -> (String, DexClass) {
        let descriptor = ty(name);
        let class = DexClass {
            symbols: Arc::new(DexSymbols::default()),
            descriptor: descriptor.clone(),
            superclass: Some(ty(super_name)),
            interfaces: interfaces.iter().map(|name| ty(name)).collect(),
            access_flags,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: methods
                .iter()
                .map(|&(parameter, flags)| DexMethod {
                    declaring_type: descriptor.clone(),
                    name: "run".into(),
                    return_type: "V".into(),
                    parameters: if parameter.is_empty() {
                        vec![]
                    } else {
                        vec![parameter.into()]
                    },
                    thrown_types: vec![],
                    access_flags: flags,
                    code: (flags & ABSTRACT == 0).then(|| DexCode {
                        registers: 2,
                        ins: if parameter.is_empty() { 1 } else { 2 },
                        outs: 0,
                        tries: 0,
                        try_regions: vec![],
                        instructions: vec![0x0e],
                        offset: 16,
                    }),
                })
                .collect(),
        };
        (name.into(), class)
    }

    #[test]
    fn hook_targets_deduplicate_inherited_declaring_methods_and_page_receivers() {
        let mut classes = BTreeMap::from([
            fixture(
                "example.Contract",
                "java.lang.Object",
                &[],
                INTERFACE | ABSTRACT,
                &[("", 1 | ABSTRACT)],
            ),
            fixture(
                "example.Base",
                "java.lang.Object",
                &[],
                ABSTRACT,
                &[("", 1), ("I", 1)],
            ),
            fixture(
                "example.Concrete",
                "example.Base",
                &["example.Contract"],
                1,
                &[("", 2), ("I", 1)],
            ),
            fixture("example.Static", "example.Concrete", &[], 1, &[("", 9)]),
            fixture(
                "example.External",
                "missing.Parent",
                &["example.Contract"],
                1,
                &[],
            ),
            fixture(
                "example.Left",
                "java.lang.Object",
                &["example.Contract"],
                INTERFACE | ABSTRACT,
                &[("", 1)],
            ),
            fixture(
                "example.Right",
                "java.lang.Object",
                &["example.Contract"],
                INTERFACE | ABSTRACT,
                &[("", 1)],
            ),
            fixture(
                "example.Defaults",
                "java.lang.Object",
                &["example.Left", "example.Right"],
                1,
                &[],
            ),
        ]);
        for index in 0..10 {
            let (name, child) = fixture(
                &format!("example.Child{index}"),
                "example.Concrete",
                &[],
                1,
                &[],
            );
            classes.insert(name, child);
        }
        let engine = NativeDexEngine::from_test_classes(classes);
        let cancel = AtomicBool::new(false);
        let mut args = json!({"method_id":"Lexample/Contract;->run()V"});
        let targets = query(&engine, &args, &cancel).unwrap();
        assert_eq!(targets["installation_target_count"], 1);
        assert_eq!(targets["coverage"]["unresolved_receivers"], 2);
        assert_eq!(
            targets["items"][0]["installation_method_id"],
            "example.Base.run()V"
        );
        assert_eq!(targets["items"][0]["possible_receiver_count"], 12);
        assert_eq!(targets["items"][0]["receiver_preview_truncated"], true);
        assert!(targets["items"][0]["runtime_receiver_class_id"].is_null());

        args["projection"] = json!("receivers");
        args["limit"] = json!(3);
        let mut rows = Vec::new();
        loop {
            let page = query(&engine, &args, &cancel).unwrap();
            rows.extend(page["items"].as_array().unwrap().iter().cloned());
            if page["next_offset"].is_null() {
                break;
            }
            args["offset"] = page["next_offset"].clone();
        }
        assert_eq!(rows.len(), 14);
        let external = rows
            .iter()
            .find(|item| item["possible_receiver_class_id"] == "example.External")
            .unwrap();
        assert_eq!(external["status"], "external_superclass");
        let defaults = rows
            .iter()
            .find(|item| item["possible_receiver_class_id"] == "example.Defaults")
            .unwrap();
        assert_eq!(
            defaults["status"],
            "potentially_ambiguous_interface_defaults"
        );
        assert_eq!(
            defaults["interface_default_candidates"],
            json!(["example.Left.run()V", "example.Right.run()V"])
        );
    }

    #[test]
    fn abstract_roots_select_exact_overloads_and_expose_bridges() {
        let engine = NativeDexEngine::from_test_classes(BTreeMap::from([
            fixture(
                "app.Abs",
                "java.lang.Object",
                &[],
                ABSTRACT,
                &[("", 1 | ABSTRACT)],
            ),
            fixture("app.Child", "app.Abs", &[], 1, &[("", 1 | 0x40), ("I", 1)]),
            fixture("app.Inherited", "app.Child", &[], 1, &[]),
            fixture("app.Missing", "app.Abs", &[], 1, &[("I", 1)]),
        ]));
        let cancel = AtomicBool::new(false);
        let result = query(&engine, &json!({"method_id":"app.Abs.run()V"}), &cancel).unwrap();
        assert_eq!(
            result["items"][0]["installation_method_id"],
            "app.Child.run()V"
        );
        assert_eq!(result["items"][0]["possible_receiver_count"], 2);
        assert_eq!(result["items"][0]["bridge"], true);
        assert_eq!(result["coverage"]["unresolved_receivers"], 1);
        assert!(query(&engine, &json!({"method_id":"app.Abs.run"}), &cancel).is_err());
        assert!(query(&engine, &json!({"method_id":"app.Abs.run(I)V"}), &cancel).is_err());
    }
}
