#[path = "invoke_index.rs"]
mod index;
pub(crate) use index::Cache;
// Exact DEX call-site queries. Filters select results after depth-bounded traversal.
use super::{CallGraph, Component, Edge, Node, component, dispatch_targets_with, owner};
use crate::{native_dex::DexClass, native_engine::NativeDexEngine, native_xrefs};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::atomic::{AtomicBool, Ordering},
};

const NODE_LIMIT: usize = 5_000;
const EDGE_LIMIT: usize = 20_000;

struct Options<'a> {
    direction: &'a str,
    interface_dispatch: bool,
    superclass_dispatch: bool,
    depth: usize,
    direct_only: bool,
    projection: &'a str,
    package_prefix: Option<&'a str>,
    class_name: Option<&'a str>,
    annotation: Option<&'a str>,
    component_type: Option<Component>,
    offset: usize,
    limit: usize,
}

fn string_option<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    args.get(key)
        .map(|v| {
            let s = v
                .as_str()
                .with_context(|| format!("{key} must be a string"))?;
            ensure!(!s.is_empty(), "{key} must be nonempty");
            Ok(s)
        })
        .transpose()
}

fn integer_option(args: &Value, key: &str, default: usize) -> Result<usize> {
    args.get(key)
        .map(|v| {
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .with_context(|| format!("{key} must be a nonnegative integer"))
        })
        .unwrap_or(Ok(default))
}

impl<'a> Options<'a> {
    fn parse(args: &'a Value) -> Result<Self> {
        ensure!(args.is_object(), "Call graph arguments must be an object");
        let direction = string_option(args, "direction")?.unwrap_or("callees");
        ensure!(
            ["callers", "callees", "both"].contains(&direction),
            "Invalid direction"
        );
        let direct_only = args
            .get("direct_only")
            .map(|v| v.as_bool().context("direct_only must be a boolean"))
            .transpose()?
            .unwrap_or(false);
        let projection = string_option(args, "projection")?.unwrap_or("graph");
        let requested_depth = integer_option(
            args,
            "depth",
            if projection == "call_sites" { 1 } else { 20 },
        )?;
        ensure!(
            (1..=100).contains(&requested_depth),
            "Call graph depth must be 1–100"
        );
        ensure!(
            !direct_only || args.get("depth").is_none_or(|_| requested_depth <= 1),
            "direct_only conflicts with depth greater than 1"
        );
        ensure!(
            ["graph", "call_sites"].contains(&projection),
            "Invalid projection"
        );
        ensure!(
            projection == "call_sites"
                || (args.get("offset").is_none() && args.get("limit").is_none()),
            "offset and limit require projection=call_sites"
        );
        let offset = integer_option(args, "offset", 0)?;
        let limit = integer_option(args, "limit", 100)?;
        ensure!((1..=500).contains(&limit), "limit must be 1–500");
        let component_type = string_option(args, "component_type")?
            .map(|s| match s {
                "activity" => Ok(Component::Activity),
                "service" => Ok(Component::Service),
                "receiver" => Ok(Component::Receiver),
                "provider" => Ok(Component::Provider),
                _ => anyhow::bail!("Invalid component_type"),
            })
            .transpose()?;
        let interface_dispatch = args["dispatch_through_interfaces"]
            .as_bool()
            .unwrap_or(false);
        let superclass_dispatch = args["dispatch_through_superclasses"]
            .as_bool()
            .unwrap_or(false);
        ensure!(
            !(interface_dispatch || superclass_dispatch)
                || (direction == "callers"
                    && projection == "call_sites"
                    && (direct_only || requested_depth == 1)),
            "Dispatch expansion requires direction=callers, projection=call_sites and depth=1"
        );
        Ok(Self {
            interface_dispatch,
            superclass_dispatch,
            direction,
            depth: if direct_only { 1 } else { requested_depth },
            direct_only,
            projection,
            package_prefix: string_option(args, "package_prefix")?,
            class_name: string_option(args, "class_name")?,
            annotation: string_option(args, "annotation")?,
            component_type,
            offset,
            limit,
        })
    }
}

struct Catalog<'a> {
    classes: BTreeMap<String, &'a DexClass>,
    method_positions: RefCell<HashMap<String, HashMap<String, usize>>>,
    cancel: &'a AtomicBool,
}

impl<'a> Catalog<'a> {
    fn new(
        classes: impl IntoIterator<Item = &'a DexClass>,
        cancel: &'a AtomicBool,
    ) -> Result<Self> {
        let mut indexed = BTreeMap::new();
        for class in classes {
            check_cancel(cancel)?;
            indexed.insert(
                crate::native_engine::disassembly::type_name(&class.descriptor),
                class,
            );
        }
        Ok(Self {
            classes: indexed,
            method_positions: RefCell::new(HashMap::new()),
            cancel,
        })
    }

    fn class(&self, name: &str) -> Option<&'a DexClass> {
        self.classes.get(name).copied()
    }

    fn parent(&self, name: &str) -> Option<String> {
        self.class(name)?
            .superclass
            .as_deref()
            .map(crate::native_engine::disassembly::type_name)
    }

    fn method_position(&self, method: &str) -> Option<usize> {
        let name = owner(method)?;
        let mut cache = self.method_positions.borrow_mut();
        if !cache.contains_key(name) {
            let class = self.class(name)?;
            if cache.len() >= 1024 {
                cache.clear();
            }
            let mut positions = HashMap::new();
            for (i, m) in class.methods.iter().enumerate() {
                if self.cancel.load(Ordering::Relaxed) {
                    return None;
                }
                positions.insert(crate::native_engine::disassembly::method_id(m), i);
            }
            cache.insert(name.into(), positions);
        }
        cache.get(name)?.get(method).copied()
    }

    /// Resolve aliases only when a matching declaration is present in a known ancestor.
    fn canonical(&self, method: &str) -> String {
        let Some(original) = owner(method) else {
            return method.into();
        };
        let member = &method[original.len() + 1..];
        let mut name = original.to_string();
        let mut seen = HashSet::new();
        while seen.insert(name.clone()) {
            if self.cancel.load(Ordering::Relaxed) {
                break;
            }
            if self.class(&name).is_none() {
                break;
            }
            let id = format!("{name}.{member}");
            if self.method_position(&id).is_some() {
                return id;
            }
            if member.starts_with('<') {
                return method.into();
            }
            match self.parent(&name) {
                Some(next) => name = next,
                None => break,
            }
        }
        method.into()
    }

    fn available(&self, method: &str) -> bool {
        self.method_position(method).is_some()
    }

    fn component(&self, method: &str) -> Option<Component> {
        owner(method).and_then(|name| component(name.into(), |n| self.parent(n)))
    }

    fn annotated(&self, method: &str, expected: &str) -> bool {
        let Some(class) = owner(method).and_then(|name| self.class(name)) else {
            return false;
        };
        let Some(directory) = class.symbols.annotations.get(&class.annotations_offset) else {
            return false;
        };
        let matches = |set: &crate::native_dex::DexAnnotationSet| {
            set.iter().any(|annotation| {
                class
                    .symbols
                    .types
                    .get(annotation.type_idx as usize)
                    .is_some_and(|descriptor| {
                        descriptor.as_ref() == expected
                            || crate::native_engine::disassembly::type_name(descriptor) == expected
                    })
            })
        };
        directory.class.as_ref().is_some_and(matches)
            || self
                .method_position(method)
                .and_then(|i| directory.methods.get(i))
                .and_then(Option::as_ref)
                .is_some_and(matches)
    }

    fn matches(&self, method: &str, options: &Options<'_>) -> bool {
        let Some(class) = owner(method) else {
            return false;
        };
        if options.class_name.is_some_and(|expected| class != expected) {
            return false;
        }
        if options.package_prefix.is_some_and(|prefix| {
            let package = class.rsplit_once('.').map_or("", |(package, _)| package);
            package != prefix
                && !package
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        }) {
            return false;
        }
        if options
            .annotation
            .is_some_and(|expected| !self.annotated(method, expected))
        {
            return false;
        }
        if options
            .component_type
            .is_some_and(|expected| self.component(method) != Some(expected))
        {
            return false;
        }
        true
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Call graph cancelled");
    Ok(())
}

/// Query static DEX metadata only; never decompiles Java or infers runtime dispatch.
pub fn query(
    engine: &NativeDexEngine,
    root: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    query_cached(engine, root, args, cancel, &Cache::default())
}
pub(crate) fn query_cached(
    engine: &NativeDexEngine,
    root: &str,
    args: &Value,
    cancel: &AtomicBool,
    cache: &Cache,
) -> Result<Value> {
    check_cancel(cancel)?;
    let catalog = Catalog::new(engine.graph_classes(), cancel)?;
    query_cached_catalog(catalog, root, args, cancel, cache)
}
fn query_cached_catalog(
    catalog: Catalog<'_>,
    root: &str,
    args: &Value,
    cancel: &AtomicBool,
    cache: &Cache,
) -> Result<Value> {
    let options = Options::parse(args)?;
    let normalized = crate::method_lookup::graph_selector(root, |name| catalog.class(name))?;
    if options.direction == "callees" {
        return query_catalog(catalog, &normalized, args, cancel);
    }
    let (index, cached) = cache.get(&catalog)?;
    let canonical = catalog.canonical(&normalized);
    let aliases = interface_aliases(&catalog, &normalized, &options, cancel)?;
    let mut queue = VecDeque::from([(canonical.clone(), 0)]);
    let mut visited = HashSet::from([canonical]);
    let mut seen_sites = HashSet::new();
    let mut sites = vec![];
    for alias in &aliases {
        for site in index.neighbors(&catalog, alias, true)? {
            if matches!(site.opcode, 0x6e | 0x74 | 0x72 | 0x78)
                && seen_sites.insert((site.caller.clone(), site.pc, site.callee.clone()))
            {
                sites.push(site);
            }
        }
    }
    while let Some((current, depth)) = queue.pop_front() {
        check_cancel(cancel)?;
        if depth >= options.depth {
            continue;
        }
        for reverse in [true, false] {
            if !reverse && options.direction == "callers" {
                continue;
            }
            for site in index.neighbors(&catalog, &current, reverse)? {
                let next = if reverse {
                    site.caller.clone()
                } else {
                    catalog.canonical(&site.callee)
                };
                if visited.insert(next.clone()) {
                    queue.push_back((next, depth + 1));
                }
                if seen_sites.insert((site.caller.clone(), site.pc, site.callee.clone())) {
                    sites.push(site);
                }
            }
        }
    }
    let mut result = query_catalog_with_sites(catalog, &normalized, args, cancel, Some(sites))?;
    result["invoke_index_cached"] = json!(cached);
    result["invoke_index"] = cache.status();
    Ok(result)
}

fn callee_sites(
    catalog: &Catalog<'_>,
    root: &str,
    depth: usize,
    cancel: &AtomicBool,
) -> Result<Vec<native_xrefs::CallSite>> {
    let mut sites = Vec::new();
    let mut seen = HashSet::from([root.to_string()]);
    let mut queue = VecDeque::from([(root.to_string(), 0usize)]);
    while let Some((method, current_depth)) = queue.pop_front() {
        check_cancel(cancel)?;
        if current_depth >= depth {
            continue;
        }
        let Some(class) = owner(&method).and_then(|name| catalog.class(name)) else {
            continue;
        };
        let Some(position) = catalog.method_position(&method) else {
            check_cancel(cancel)?;
            continue;
        };
        for site in native_xrefs::method_call_sites(class, &class.methods[position], cancel)? {
            check_cancel(cancel)?;
            let callee = catalog.canonical(&site.callee);
            if seen.insert(callee.clone()) {
                queue.push_back((callee, current_depth + 1))
            }
            sites.push(site);
        }
    }
    Ok(sites)
}

fn interface_aliases(
    catalog: &Catalog<'_>,
    root: &str,
    options: &Options<'_>,
    cancel: &AtomicBool,
) -> Result<HashSet<String>> {
    if !(options.interface_dispatch || options.superclass_dispatch) {
        return Ok(HashSet::new());
    }
    if let Some(class) = owner(root).and_then(|name| catalog.class(name))
        && (class.access_flags & 0x200 != 0
            || class.methods.iter().any(|m| {
                crate::native_engine::disassembly::method_id(m) == root
                    && m.access_flags & 0x400 != 0
            }))
    {
        return Ok(HashSet::new());
    }
    let (ancestors, truncated) = dispatch_targets_with(|name| catalog.class(name), root, cancel)?;
    ensure!(
        !truncated,
        "Dispatch ancestry exceeds budget; query ancestor methods explicitly"
    );
    Ok(ancestors
        .into_iter()
        .filter(|id| {
            owner(id).and_then(|n| catalog.class(n)).is_some_and(|c| {
                if c.access_flags & 0x200 != 0 {
                    options.interface_dispatch
                } else {
                    options.superclass_dispatch
                }
            })
        })
        .collect())
}
fn record(site: &native_xrefs::CallSite, depth: usize) -> Value {
    json!({"caller_method_id":site.caller,"caller_signature":site.caller,"callee_method_id":site.callee,"raw_callee_method_id":site.callee,"call_site_offset":site.pc,"depth":depth,
        "invoke_instruction":crate::native_engine::disassembly::invoke_text(&site.words, site.pc, &site.callee),"dex_entry":site.dex_entry,"method_index":site.method_index,"invoke_opcode":site.opcode,"invoke_words":site.words,"caller_code_offset_bytes":site.code_offset,"dispatch":site.dispatch})
}

fn query_catalog(
    catalog: Catalog<'_>,
    root: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    query_catalog_with_sites(catalog, root, args, cancel, None)
}
fn query_catalog_with_sites(
    catalog: Catalog<'_>,
    root: &str,
    args: &Value,
    cancel: &AtomicBool,
    provided: Option<Vec<native_xrefs::CallSite>>,
) -> Result<Value> {
    let options = Options::parse(args)?;
    let normalized_root = crate::method_lookup::graph_selector(root, |name| catalog.class(name))?;
    let root = normalized_root.as_str();
    check_cancel(cancel)?;
    let root_key = catalog.canonical(root);
    let aliases = interface_aliases(&catalog, root, &options, cancel)?;
    // Outgoing queries scan only reachable bodies. Reverse traversal needs the
    // project-wide call index, independently of graph projection limits.
    let mut sites = if let Some(sites) = provided {
        sites
    } else if options.direction == "callees" {
        callee_sites(&catalog, &root_key, options.depth, cancel)?
    } else {
        let mut sites = Vec::new();
        for class in catalog.classes.values() {
            check_cancel(cancel)?;
            sites.extend(native_xrefs::call_sites(class, None, cancel)?);
        }
        sites
    };
    sites.sort_by(|a, b| (&a.caller, a.pc, &a.callee).cmp(&(&b.caller, b.pc, &b.callee)));
    let mut outgoing: HashMap<String, Vec<usize>> = HashMap::new();
    let mut incoming: HashMap<String, Vec<usize>> = HashMap::new();
    let mut keys = Vec::with_capacity(sites.len());
    let mut canonical_cache = HashMap::new();
    for (i, site) in sites.iter().enumerate() {
        check_cancel(cancel)?;
        let caller = canonical_cache
            .entry(site.caller.clone())
            .or_insert_with(|| catalog.canonical(&site.caller))
            .clone();
        let callee = canonical_cache
            .entry(site.callee.clone())
            .or_insert_with(|| catalog.canonical(&site.callee))
            .clone();
        let callee =
            if aliases.contains(&site.callee) && matches!(site.opcode, 0x6e | 0x74 | 0x72 | 0x78) {
                root_key.clone()
            } else {
                callee
            };
        outgoing.entry(caller.clone()).or_default().push(i);
        incoming.entry(callee.clone()).or_default().push(i);
        keys.push((caller, callee));
    }
    let mut depths = HashMap::from([(root_key.clone(), 0usize)]);
    let mut queue = VecDeque::from([root_key.clone()]);
    let mut selected_sites: BTreeMap<usize, usize> = BTreeMap::new();
    let mut traversed_sites = HashSet::new();
    let mut filter_cache = HashMap::new();
    while let Some(current) = queue.pop_front() {
        check_cancel(cancel)?;
        let depth = depths[&current];
        if depth >= options.depth {
            continue;
        }
        let forwards = outgoing
            .get(&current)
            .into_iter()
            .flatten()
            .map(|&i| (i, false))
            .filter(|_| options.direction != "callers");
        let reverse = incoming
            .get(&current)
            .into_iter()
            .flatten()
            .map(|&i| (i, true))
            .filter(|_| options.direction != "callees");
        for (i, backwards) in forwards.chain(reverse) {
            check_cancel(cancel)?;
            let next = if backwards { &keys[i].0 } else { &keys[i].1 };
            if !depths.contains_key(next) {
                depths.insert(next.clone(), depth + 1);
                queue.push_back(next.clone());
            }
            traversed_sites.insert(i);
            if *filter_cache
                .entry(next.clone())
                .or_insert_with(|| catalog.matches(next, &options))
            {
                selected_sites
                    .entry(i)
                    .and_modify(|d| *d = (*d).min(depth + 1))
                    .or_insert(depth + 1);
            }
        }
    }
    check_cancel(cancel)?;
    let boundary = depths.values().any(|&depth| depth == options.depth);
    let filters = json!({"package_prefix":options.package_prefix,"class_name":options.class_name,"annotation":options.annotation,
        "component_type":options.component_type.map(|c| c.label().to_ascii_lowercase())});
    let metadata = json!({
        "root_method_id":root,"direction":options.direction,"depth":options.depth,"direct_only":options.direct_only,
        "projection":options.projection,"filters":filters,"depth_boundary_reached":boundary,
        "filter_semantics":"Filters select canonical neighbor declarations after traversal; they do not prune traversal. Compact records match the neighbor in the requested traversal direction. Graph nodes retain matching declarations plus the root; edges connect retained nodes.",
        "annotation_semantics":"Exact DEX descriptor or fully qualified type name on the class or method declaration; combined filters use AND.",
        "component_semantics":"Static superclass ancestry classification.",
        "analysis":"static DEX declared calls; inherited aliases resolved through known superclasses; no runtime dispatch, reflection, or Intent target inference"
    });
    let mut result = metadata;
    if options.projection == "call_sites" {
        let total = selected_sites.len();
        result["direct_matching_total"] =
            json!(selected_sites.values().filter(|&&depth| depth == 1).count());
        result["call_site_semantics"] = json!(
            "Each record is one raw DEX invoke to callee_method_id. depth is traversal distance from the query root; only depth=1 records are direct root neighbors. call_sites defaults to depth=1; explicit depth includes transitive sites."
        );
        let record_for = |i: usize, depth: usize| {
            let mut row = record(&sites[i], depth);
            let inferred = aliases.contains(&sites[i].callee)
                && matches!(sites[i].opcode, 0x6e | 0x74 | 0x72 | 0x78);
            row["relationship"] = json!(if inferred {
                if matches!(sites[i].opcode, 0x72 | 0x78) {
                    "possible_interface_dispatch"
                } else {
                    "possible_virtual_dispatch"
                }
            } else {
                "exact_declared_call"
            });
            if inferred {
                row["possible_implementation_method_id"] = json!(root);
            }
            row
        };
        let inferred = selected_sites
            .keys()
            .filter(|&&i| {
                aliases.contains(&sites[i].callee)
                    && matches!(sites[i].opcode, 0x6e | 0x74 | 0x72 | 0x78)
            })
            .count();
        result["exact_matching_total"] = json!(total - inferred);
        result["possible_dispatch_total"] = json!(inferred);
        result["dispatch_through_interfaces"] = json!(options.interface_dispatch);
        result["dispatch_through_superclasses"] = json!(options.superclass_dispatch);
        let mut hash = Sha256::new();
        for (&i, &depth) in &selected_sites {
            check_cancel(cancel)?;
            hash.update(serde_json::to_vec(&record_for(i, depth))?);
            hash.update(b"\n");
        }
        let records: Vec<_> = selected_sites
            .iter()
            .skip(options.offset)
            .take(options.limit)
            .map(|(&i, &depth)| record_for(i, depth))
            .collect();
        let returned = records.len();
        let next = options.offset.saturating_add(returned);
        let has_more = next < total;
        result["call_sites"] = json!(records);
        result["matching_total"] = json!(total);
        result["offset"] = json!(options.offset);
        result["limit"] = json!(options.limit);
        result["returned_count"] = json!(returned);
        result["has_more"] = json!(has_more);
        result["next_offset"] = if has_more { json!(next) } else { Value::Null };
        result["truncated"] = json!(false);
        result["source_hash"] = json!(format!("{:x}", hash.finalize()));
        result["source_hash_semantics"] = json!(
            "SHA-256 of every matching compact record in deterministic order, serialized as JSON followed by a newline; independent of offset and limit"
        );
        result["call_site_offset_units"] =
            json!("DEX code units (16-bit), relative to the caller method instruction array");
        check_cancel(cancel)?;
        return Ok(result);
    }
    let (dispatch_targets, dispatch_targets_truncated) =
        dispatch_targets_with(|name| catalog.class(name), root, cancel)?;
    let mut methods: Vec<_> = depths
        .iter()
        .filter(|(method, _)| **method != root_key && catalog.matches(method, &options))
        .map(|(method, &depth)| (method.clone(), depth))
        .collect();
    methods.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
    methods.insert(0, (root_key.clone(), 0));
    let matching_nodes = methods.len();
    let matching_keys: HashSet<_> = methods.iter().map(|(method, _)| method.as_str()).collect();
    let mut counts = BTreeMap::new();
    for i in traversed_sites {
        check_cancel(cancel)?;
        let (from, to) = &keys[i];
        if matching_keys.contains(from.as_str()) && matching_keys.contains(to.as_str()) {
            *counts.entry((from.clone(), to.clone())).or_insert(0usize) += 1;
        }
    }
    let matching_edges = counts.len();
    methods.truncate(NODE_LIMIT);
    let indices: HashMap<_, _> = methods
        .iter()
        .enumerate()
        .map(|(i, (method, _))| (method.clone(), i))
        .collect();
    let mut graph = CallGraph::default();
    for (i, (method, depth)) in methods.into_iter().enumerate() {
        check_cancel(cancel)?;
        graph.nodes.push(Node {
            component: catalog.component(&method),
            available: catalog.available(&method),
            method: if i == 0 { root.into() } else { method },
            depth,
        });
    }
    for ((from, to), count) in counts {
        if let (Some(&from), Some(&to)) = (indices.get(&from), indices.get(&to)) {
            if graph.edges.len() == EDGE_LIMIT {
                break;
            }
            graph.edges.push(Edge {
                from,
                to,
                sites: count,
            });
        }
    }
    graph.truncated = matching_nodes > graph.nodes.len() || matching_edges > graph.edges.len();
    let masks = graph.component_reachability();
    let kinds = |mask: u8| {
        ["Activity", "Service", "Receiver", "Provider"]
            .into_iter()
            .enumerate()
            .filter_map(|(i, kind)| ((mask & (1 << i)) != 0).then_some(kind))
            .collect::<Vec<_>>()
    };
    result["nodes"] = json!(graph.nodes.iter().enumerate().map(|(i,n)| json!({"id":i,"method_id":n.method,"depth":n.depth,"component":n.component.map(|c| c.label()),"available":n.available,"reachable_components":kinds(masks[i])})).collect::<Vec<_>>());
    result["edges"] = json!(graph.edges.iter().map(|e| json!({"from":e.from,"to":e.to,"call_sites":e.sites,"highlighted":masks[e.to] != 0,"reachable_components":kinds(masks[e.to])})).collect::<Vec<_>>());
    result["root"] = json!(0);
    result["truncated"] = json!(graph.truncated);
    result["node_limit"] = json!(NODE_LIMIT);
    result["edge_limit"] = json!(EDGE_LIMIT);
    result["matching_node_count"] = json!(matching_nodes);
    result["matching_edge_count"] = json!(matching_edges);
    result["dispatch_targets"] = json!(dispatch_targets);
    result["dispatch_targets_truncated"] = json!(dispatch_targets_truncated);
    result["dispatch_note"] = json!(
        "Related ancestor declarations are possible dispatch entry points; query their callers separately."
    );
    check_cancel(cancel)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{
        DexAnnotation, DexAnnotationDirectory, DexAnnotationSet, DexCode, DexMethod, DexSymbols,
    };
    use std::sync::Arc;

    pub(super) fn class(
        name: &str,
        parent: Option<&str>,
        methods: Vec<(&str, Vec<&str>, Vec<&str>)>,
    ) -> DexClass {
        let descriptor: Arc<str> = format!("L{};", name.replace('.', "/")).into();
        let mut symbols = DexSymbols::default();
        let methods = methods
            .into_iter()
            .map(|(name, parameters, calls)| {
                let mut instructions = vec![];
                for target in calls {
                    let target_owner = owner(target).unwrap();
                    let member = &target[target_owner.len() + 1..];
                    let (member_name, signature) = member.split_once('(').unwrap();
                    assert_eq!(signature, ")V");
                    let index = symbols.methods.len() as u16;
                    let type_index = symbols.types.len() as u16;
                    symbols
                        .types
                        .push(format!("L{};", target_owner.replace('.', "/")).into());
                    let proto = symbols.protos.len() as u16;
                    symbols.protos.push(("V".into(), vec![]));
                    let name_index = symbols.strings.len() as u32;
                    symbols.strings.push(member_name.into());
                    symbols.methods.push((type_index, proto, name_index));
                    // A NOP makes the code-unit offset differ from an instruction index.
                    instructions.extend([0x0000, 0x0071, index, 0]);
                }
                instructions.push(0x000e);
                DexMethod {
                    declaring_type: descriptor.clone(),
                    name: name.into(),
                    return_type: "V".into(),
                    parameters: parameters.into_iter().map(Arc::from).collect(),
                    thrown_types: vec![],
                    access_flags: 9,
                    code: Some(DexCode {
                        registers: 1,
                        ins: 0,
                        outs: 0,
                        tries: 0,
                        try_regions: vec![],
                        instructions,
                        offset: 0,
                    }),
                }
            })
            .collect();
        DexClass {
            descriptor,
            superclass: parent.map(|p| format!("L{};", p.replace('.', "/")).into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods,
            symbols: Arc::new(symbols),
        }
    }

    #[test]
    fn interface_dispatch_keeps_declared_target_and_separate_counts() {
        let cancel = AtomicBool::new(false);
        let mut interface = class("p.Contract", None, vec![("run", vec![], vec![])]);
        interface.access_flags = 0x201;
        interface.methods[0].access_flags = 0x401;
        let mut implementation = class("p.Impl", None, vec![("run", vec![], vec![])]);
        implementation.interfaces = vec!["Lp/Contract;".into()];
        implementation.methods[0].access_flags = 1;
        let mut caller = class(
            "p.Caller",
            None,
            vec![("call", vec![], vec!["p.Contract.run()V", "p.Impl.run()V"])],
        );
        let body = caller.methods[0].code.as_mut().unwrap();
        body.instructions[1] = 0x1072;
        body.instructions[5] = 0x106e;
        body.outs = 1;
        let args = json!({"direction":"callers","projection":"call_sites","dispatch_through_interfaces":true,"limit":1});
        let run = |args: &Value| {
            query_cached_catalog(
                Catalog::new([&interface, &implementation, &caller], &cancel).unwrap(),
                "p.Impl.run()V",
                args,
                &cancel,
                &Cache::default(),
            )
            .unwrap()
        };
        let first = run(&args);
        assert_eq!(first["matching_total"], 2);
        assert_eq!(first["exact_matching_total"], 1);
        assert_eq!(first["possible_dispatch_total"], 1);
        assert_eq!(
            first["call_sites"][0]["raw_callee_method_id"],
            "p.Contract.run()V"
        );
        assert_eq!(
            first["call_sites"][0]["relationship"],
            "possible_interface_dispatch"
        );
        let mut next = args.clone();
        next["offset"] = json!(1);
        let second = run(&next);
        assert_eq!(
            second["call_sites"][0]["relationship"],
            "exact_declared_call"
        );
        assert_eq!(first["source_hash"], second["source_hash"]);
        next["dispatch_through_interfaces"] = json!(false);
        next["offset"] = json!(0);
        assert_eq!(run(&next)["matching_total"], 1);
        implementation.access_flags = 0x401;
        implementation.methods[0].access_flags = 0x401;
        implementation.methods[0].code = None;
        let abstract_result = query_cached_catalog(
            Catalog::new([&interface, &implementation, &caller], &cancel).unwrap(),
            "p.Impl.run()V",
            &args,
            &cancel,
            &Cache::default(),
        )
        .unwrap();
        assert_eq!(abstract_result["possible_dispatch_total"], 0);
    }

    #[test]
    fn superclass_dispatch_preserves_raw_target_and_excludes_super_calls() {
        let cancel = AtomicBool::new(false);
        let mut base = class("p.Base", None, vec![("run", vec![], vec![])]);
        base.access_flags = 0x401;
        base.methods[0].access_flags = 0x401;
        base.methods[0].code = None;
        let mut child = class("p.Child", Some("p.Base"), vec![("run", vec![], vec![])]);
        child.methods[0].access_flags = 1;
        let mut caller = class(
            "p.Caller",
            None,
            vec![("call", vec![], vec!["p.Base.run()V", "p.Base.run()V"])],
        );
        let body = caller.methods[0].code.as_mut().unwrap();
        body.instructions[1] = 0x106e;
        body.instructions[5] = 0x106f;
        body.outs = 1;
        let run = |expand| {
            query_cached_catalog(Catalog::new([&base,&child,&caller],&cancel).unwrap(),"p.Child.run()V",
            &json!({"direction":"callers","projection":"call_sites","dispatch_through_superclasses":expand}),&cancel,&Cache::default()).unwrap()
        };
        assert_eq!(run(false)["matching_total"], 0);
        let result = run(true);
        assert_eq!(result["matching_total"], 1);
        assert_eq!(result["possible_dispatch_total"], 1);
        assert_eq!(
            result["call_sites"][0]["raw_callee_method_id"],
            "p.Base.run()V"
        );
        assert_eq!(
            result["call_sites"][0]["relationship"],
            "possible_virtual_dispatch"
        );
    }

    fn annotate(
        class: &mut DexClass,
        class_type: Option<&str>,
        method_type: Option<(usize, &str)>,
    ) {
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        let mut set = |name: &str| -> DexAnnotationSet {
            let index = symbols.types.len() as u32;
            symbols.types.push(name.into());
            vec![Arc::new(DexAnnotation {
                visibility: 1,
                type_idx: index,
                elements: vec![],
            })]
            .into()
        };
        let mut directory = DexAnnotationDirectory {
            class: class_type.map(&mut set),
            methods: vec![None; class.methods.len()],
            ..Default::default()
        };
        if let Some((i, name)) = method_type {
            directory.methods[i] = Some(set(name))
        }
        class.annotations_offset = 24;
        symbols.annotations.insert(24, Arc::new(directory));
    }

    fn run(classes: &[DexClass], root: &str, args: Value) -> Value {
        let cancel = AtomicBool::new(false);
        query_catalog(
            Catalog::new(classes, &cancel).unwrap(),
            root,
            &args,
            &cancel,
        )
        .unwrap()
    }

    #[test]
    fn disk_query_matches_full_scan_for_transitive_filtered_and_bidirectional_graphs() {
        let classes = vec![
            class(
                "app.A",
                None,
                vec![("run", vec![], vec!["app.B.run()V", "api.Child.call()V"])],
            ),
            class(
                "app.B",
                None,
                vec![("run", vec![], vec!["api.Parent.call()V"])],
            ),
            class(
                "api.Parent",
                None,
                vec![("call", vec![], vec!["app.A.run()V"])],
            ),
            class("api.Child", Some("api.Parent"), vec![]),
        ];
        let cancel = AtomicBool::new(false);
        let cache = Cache::default();
        for direction in ["callers", "callees", "both"] {
            for depth in [1, 2, 4] {
                for projection in ["graph", "call_sites"] {
                    for filter in [None, Some("app.")] {
                        let mut args =
                            json!({"direction":direction,"depth":depth,"projection":projection});
                        if let Some(prefix) = filter {
                            args["package_prefix"] = json!(prefix);
                        }
                        if projection == "call_sites" {
                            args["offset"] = json!(1);
                            args["limit"] = json!(2);
                        }
                        let expected = run(&classes, "api.Child.call()V", args.clone());
                        let mut actual = query_cached_catalog(
                            Catalog::new(&classes, &cancel).unwrap(),
                            "api.Child.call()V",
                            &args,
                            &cancel,
                            &cache,
                        )
                        .unwrap();
                        actual.as_object_mut().unwrap().remove("invoke_index");
                        actual
                            .as_object_mut()
                            .unwrap()
                            .remove("invoke_index_cached");
                        assert_eq!(actual, expected, "{args}");
                    }
                }
            }
        }
    }

    #[test]
    fn compact_pagination_preserves_all_callers_overloads_and_exact_offsets() {
        let names: Vec<_> = (0..83).map(|i| format!("caller{i:03}")).collect();
        let mut methods: Vec<_> = names
            .iter()
            .map(|name| (name.as_str(), vec![], vec!["target.Root.run()V"]))
            .collect();
        methods.extend([
            (
                "overload",
                vec!["I"],
                vec!["target.Root.run()V", "target.Root.run()V"],
            ),
            (
                "overload",
                vec!["Ljava/lang/String;"],
                vec!["target.Root.run()V"],
            ),
        ]);
        let classes = vec![
            class("app.Callers", None, methods),
            class("target.Root", None, vec![("run", vec![], vec![])]),
        ];
        let mut all = vec![];
        let mut offset = 0;
        let mut source_hash = None;
        loop {
            let result = run(
                &classes,
                "target.Root.run()V",
                json!({"direction":"callers","direct_only":true,"projection":"call_sites","offset":offset,"limit":17}),
            );
            assert_eq!(result["matching_total"], 86);
            assert_eq!(result["truncated"], false);
            assert_eq!(result["depth"], 1);
            if let Some(hash) = &source_hash {
                assert_eq!(&result["source_hash"], hash)
            } else {
                source_hash = Some(result["source_hash"].clone())
            }
            all.extend(result["call_sites"].as_array().unwrap().iter().cloned());
            if result["has_more"] == false {
                assert!(result["next_offset"].is_null());
                break;
            }
            offset = result["next_offset"].as_u64().unwrap();
        }
        assert_eq!(all.len(), 86);
        assert_eq!(
            all.iter()
                .filter(|r| r["caller_method_id"] == "app.Callers.overload(I)V")
                .map(|r| r["call_site_offset"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 5]
        );
        assert!(
            all.iter()
                .any(|r| r["caller_signature"] == "app.Callers.overload(Ljava/lang/String;)V")
        );
        let unique: HashSet<_> = all
            .iter()
            .map(|r| {
                (
                    r["caller_method_id"].as_str().unwrap(),
                    r["call_site_offset"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(unique.len(), 86);
        let beyond = run(
            &classes,
            "target.Root.run()V",
            json!({"direction":"callers","direct_only":true,"projection":"call_sites","offset":1000}),
        );
        assert_eq!(beyond["returned_count"], 0);
        assert_eq!(beyond["has_more"], false);
        assert_eq!(Some(beyond["source_hash"].clone()), source_hash);
    }

    #[test]
    fn compact_direct_enumeration_exceeds_legacy_graph_node_cap() {
        let names: Vec<_> = (0..5005).map(|i| format!("caller{i:04}")).collect();
        let methods = names
            .iter()
            .map(|name| (name.as_str(), vec![], vec!["target.Root.run()V"]))
            .collect();
        let classes = vec![class("app.Callers", None, methods)];
        let result = run(
            &classes,
            "target.Root.run()V",
            json!({"direction":"callers","direct_only":true,"projection":"call_sites","offset":5000,"limit":500}),
        );
        assert_eq!(result["matching_total"], 5005);
        assert_eq!(result["returned_count"], 5);
        assert_eq!(result["truncated"], false);
        assert_eq!(result["has_more"], false);
        let graph = run(
            &classes,
            "target.Root.run()V",
            json!({"direction":"callers","direct_only":true}),
        );
        assert_eq!(graph["matching_node_count"], 5006);
        assert_eq!(graph["matching_edge_count"], 5005);
        assert_eq!(graph["nodes"].as_array().unwrap().len(), NODE_LIMIT);
        assert_eq!(graph["truncated"], true);
    }

    #[test]
    fn filters_match_exact_package_class_annotations_and_component_ancestry() {
        let mut good = class(
            "app.work.Good",
            Some("app.BaseService"),
            vec![("run", vec![], vec!["target.Root.run()V"])],
        );
        annotate(
            &mut good,
            Some("Ltags/ClassMark;"),
            Some((0, "Ltags/MethodMark;")),
        );
        let mut method_only = class(
            "app.MethodOnly",
            None,
            vec![("run", vec![], vec!["target.Root.run()V"])],
        );
        annotate(&mut method_only, None, Some((0, "Ltags/MethodMark;")));
        let classes = vec![
            good,
            method_only,
            class(
                "application.Bad",
                None,
                vec![("run", vec![], vec!["target.Root.run()V"])],
            ),
            class("app.BaseService", Some("android.app.Service"), vec![]),
        ];
        let base = json!({"direction":"callers","direct_only":true,"projection":"call_sites","package_prefix":"app"});
        assert_eq!(
            run(&classes, "target.Root.run()V", base.clone())["matching_total"],
            2
        );
        for annotation in ["tags.MethodMark", "Ltags/MethodMark;"] {
            let mut args = base.clone();
            args["annotation"] = json!(annotation);
            assert_eq!(
                run(&classes, "target.Root.run()V", args)["matching_total"],
                2
            );
        }
        let mut args = base.clone();
        args["annotation"] = json!("tags.ClassMark");
        assert_eq!(
            run(&classes, "target.Root.run()V", args.clone())["matching_total"],
            1
        );
        args["component_type"] = json!("service");
        args["class_name"] = json!("app.work.Good");
        assert_eq!(
            run(&classes, "target.Root.run()V", args.clone())["matching_total"],
            1
        );
        args["annotation"] = json!("MethodMark");
        assert_eq!(
            run(&classes, "target.Root.run()V", args)["matching_total"],
            0
        );
        let mut args = base.clone();
        args["class_name"] = json!("app.work.Goo");
        assert_eq!(
            run(&classes, "target.Root.run()V", args)["matching_total"],
            0
        );
        let mut args = base;
        args["projection"] = json!("graph");
        args["class_name"] = json!("app.work.Good");
        let graph = run(&classes, "target.Root.run()V", args);
        assert_eq!(graph["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(graph["nodes"][0]["method_id"], "target.Root.run()V");
        assert_eq!(graph["nodes"][1]["component"], "Service");
    }

    #[test]
    fn aliases_recursion_both_direction_and_post_traversal_filters_are_stable() {
        let mut parent = class(
            "app.Parent",
            None,
            vec![
                ("other", vec![], vec![]),
                ("run", vec![], vec!["app.Child.run()V", "leaf.End.run()V"]),
            ],
        );
        annotate(&mut parent, None, Some((1, "Ltags/Inherited;")));
        let classes = vec![
            parent,
            class("app.Child", Some("app.Parent"), vec![]),
            class(
                "app.Caller",
                None,
                vec![("entry", vec![], vec!["app.Child.run()V"])],
            ),
            class(
                "leaf.End",
                None,
                vec![("run", vec![], vec!["tail.Last.run()V"])],
            ),
        ];
        let args = json!({"direction":"both","direct_only":true,"projection":"call_sites"});
        let result = run(&classes, "app.Child.run()V", args.clone());
        assert_eq!(result["matching_total"], 3);
        assert_eq!(result, run(&classes, "app.Child.run()V", args));
        let sites = result["call_sites"].as_array().unwrap();
        assert_eq!(
            sites
                .iter()
                .filter(
                    |r| r["caller_method_id"] == "app.Parent.run()V" && r["call_site_offset"] == 1
                )
                .count(),
            1
        );
        assert!(
            sites
                .iter()
                .any(|r| r["callee_method_id"] == "app.Child.run()V")
        );
        let alias_filter = run(
            &classes,
            "app.Child.run()V",
            json!({"projection":"call_sites","direction":"callees","direct_only":true,"class_name":"app.Parent"}),
        );
        assert_eq!(alias_filter["matching_total"], 1);
        assert_eq!(
            alias_filter["call_sites"][0]["callee_method_id"],
            "app.Child.run()V"
        );
        let annotated_alias = run(
            &classes,
            "app.Child.run()V",
            json!({"projection":"call_sites","direction":"callees","direct_only":true,"annotation":"tags.Inherited"}),
        );
        assert_eq!(annotated_alias["matching_total"], 1);
        assert_eq!(
            annotated_alias["call_sites"][0]["callee_method_id"],
            "app.Child.run()V"
        );
        let graph = run(
            &classes,
            "app.Child.run()V",
            json!({"direction":"both","direct_only":true}),
        );
        assert_eq!(graph["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(graph["edges"].as_array().unwrap().len(), 3);
        let filtered = run(
            &classes,
            "app.Child.run()V",
            json!({"projection":"call_sites","direction":"callees","depth":2,"class_name":"tail.Last"}),
        );
        assert_eq!(filtered["matching_total"], 1);
        assert_eq!(filtered["call_sites"][0]["depth"], 2);
        assert_eq!(
            filtered["call_sites"][0]["caller_method_id"],
            "leaf.End.run()V"
        );
    }

    #[test]
    fn callee_queries_scan_only_reachable_bodies_within_requested_depth() {
        let mut root = class(
            "app.Root",
            None,
            vec![
                ("run", vec![], vec!["app.Middle.run()V"]),
                ("unrelated", vec![], vec![]),
            ],
        );
        root.methods[1].code.as_mut().unwrap().instructions = vec![0x0071, u16::MAX, 0];
        let mut middle = class("app.Middle", None, vec![("run", vec![], vec![])]);
        middle.methods[0].code.as_mut().unwrap().instructions = vec![0x0071, u16::MAX, 0];
        let classes = vec![root, middle];
        let args = json!({"projection":"call_sites","direction":"callees","direct_only":true});
        let result = run(&classes, "app.Root.run()V", args);
        assert_eq!(result["matching_total"], 1);
        for args in [
            json!({"projection":"call_sites","direction":"callees","depth":2}),
            json!({"projection":"call_sites","direction":"callers","direct_only":true}),
        ] {
            let cancel = AtomicBool::new(false);
            assert!(
                query_catalog(
                    Catalog::new(&classes, &cancel).unwrap(),
                    "app.Root.run()V",
                    &args,
                    &cancel
                )
                .is_err()
            );
        }
    }

    #[test]
    fn compact_defaults_are_direct_and_explicit_depth_preserves_transitive_sites() {
        let classes = vec![
            class("app.Root", None, vec![("run", vec![], vec![])]),
            class(
                "app.Middle",
                None,
                vec![("run", vec![], vec!["app.Root.run()V"])],
            ),
            class(
                "app.Outer",
                None,
                vec![("run", vec![], vec!["app.Middle.run()V"])],
            ),
        ];
        let direct = run(
            &classes,
            "Lapp/Root;->run()V",
            json!({"projection":"call_sites","direction":"callers"}),
        );
        assert_eq!(direct["root_method_id"], "app.Root.run()V");
        assert_eq!(direct["matching_total"], 1);
        assert_eq!(direct["depth"], 1);
        assert_eq!(
            direct["call_sites"][0]["callee_method_id"],
            "app.Root.run()V"
        );
        let transitive = run(
            &classes,
            "app.Root.run()V",
            json!({"projection":"call_sites","direction":"callers","depth":2}),
        );
        assert_eq!(transitive["matching_total"], 2);
        assert_eq!(transitive["direct_matching_total"], 1);
        assert_eq!(
            transitive["call_sites"][1]["callee_method_id"],
            "app.Middle.run()V"
        );
        assert_eq!(transitive["call_sites"][1]["depth"], 2);
        assert_eq!(Options::parse(&json!({})).unwrap().depth, 20);
    }

    #[test]
    fn dex_local_method_indexes_keep_raw_target_and_container_provenance() {
        // Both independent symbol tables assign index 0 to different methods.
        let first = class(
            "app.First",
            None,
            vec![("run", vec![], vec!["target.Right.run()V"])],
        );
        first.symbols.origin.set("classes.dex".into()).unwrap();
        let second = class(
            "app.Second",
            None,
            vec![("run", vec![], vec!["target.Wrong.run()V"])],
        );
        second.symbols.origin.set("classes2.dex".into()).unwrap();
        let classes = vec![first, second];
        let args = json!({"projection":"call_sites","direction":"callers"});
        let right = run(&classes, "target.Right.run()V", args.clone());
        let wrong = run(&classes, "target.Wrong.run()V", args);
        for (result, caller, entry, callee) in [
            (
                right,
                "app.First.run()V",
                "classes.dex",
                "target.Right.run()V",
            ),
            (
                wrong,
                "app.Second.run()V",
                "classes2.dex",
                "target.Wrong.run()V",
            ),
        ] {
            assert_eq!(result["matching_total"], 1);
            let site = &result["call_sites"][0];
            assert_eq!(site["caller_method_id"], caller);
            assert_eq!(site["raw_callee_method_id"], callee);
            assert_eq!(site["dex_entry"], entry);
            assert_eq!(site["method_index"], 0);
            assert_eq!(site["invoke_opcode"], 0x71);
            assert_eq!(site["invoke_words"], json!([0x71, 0, 0]));
            assert!(
                site["invoke_instruction"]
                    .as_str()
                    .unwrap()
                    .contains("invoke-static {}")
            );
            assert!(
                site["invoke_instruction"]
                    .as_str()
                    .unwrap()
                    .contains(callee)
            );
            assert_eq!(site["call_site_offset"], 1);
        }
    }

    #[test]
    fn unresolved_sibling_methods_do_not_alias_through_external_parent() {
        let classes = vec![
            class("app.Left", Some("external.Parent"), vec![]),
            class("app.Right", Some("external.Parent"), vec![]),
            class(
                "app.Caller",
                None,
                vec![(
                    "run",
                    vec![],
                    vec!["app.Left.missing()V", "app.Right.missing()V"],
                )],
            ),
        ];
        let result = run(
            &classes,
            "app.Left.missing()V",
            json!({"projection":"call_sites","direction":"callers"}),
        );
        assert_eq!(result["matching_total"], 1);
        assert_eq!(
            result["call_sites"][0]["callee_method_id"],
            "app.Left.missing()V"
        );
    }

    #[test]
    fn cancellation_and_invalid_options_return_errors() {
        let args = json!({"projection":"call_sites","direct_only":true});
        let cancel = AtomicBool::new(true);
        assert!(
            Catalog::new([], &cancel)
                .and_then(|catalog| query_catalog(catalog, "C.run()V", &args, &cancel))
                .is_err()
        );
        for args in [
            json!({"direct_only":true,"depth":2}),
            json!({"direct_only":"true"}),
            json!({"offset":1}),
            json!({"projection":"call_sites","limit":501}),
            json!({"projection":"call_sites","offset":-1}),
            json!({"annotation":[]}),
            json!({"component_type":"Activity"}),
        ] {
            assert!(Options::parse(&args).is_err(), "{args}");
        }
    }
}
