#[path = "invoke_index.rs"]
mod index;
pub(crate) use index::Cache;
// Exact DEX call-site queries. Filters select results after depth-bounded traversal.
use super::{CallGraph, Component, Direction, Edge, Node, component, dispatch_targets_with, owner};
use crate::{native_dex::DexClass, native_engine::NativeDexEngine, native_xrefs};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::atomic::{AtomicBool, Ordering},
};

const NODE_LIMIT: usize = 5_000;
const EDGE_LIMIT: usize = 20_000;

struct Options<'a> {
    direction: &'a str,
    interface_dispatch: bool,
    superclass_dispatch: bool,
    semantic_edges: bool,
    depth: usize,
    direct_only: bool,
    projection: &'a str,
    package_prefix: Option<String>,
    class_name: Option<String>,
    annotation: Option<&'a str>,
    component_type: Option<Component>,
    offset: usize,
    limit: usize,
    /// Expand at most this many methods; later discoveries stay leaves. Never set
    /// from request arguments: MCP queries traverse fully and report totals.
    visit_limit: Option<usize>,
    /// Run only the `lambda_creation` producer when `semantic_edges` is on, so a
    /// Callers graph reaches callback creators without dispatch guesses. Never set
    /// from request arguments: MCP queries report every producer.
    lambda_creators_only: bool,
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

/// Package filters accept dotted or slash form, with or without a trailing dot.
fn package_filter(prefix: &str) -> Result<String> {
    let normalized = prefix.replace('/', ".");
    let trimmed = normalized.trim_matches('.');
    ensure!(!trimmed.is_empty(), "package_prefix must name a package");
    Ok(trimmed.to_owned())
}

/// Class filters accept an exact FQCN, a slash-separated name, or a DEX descriptor.
fn class_filter(name: &str) -> String {
    if name.starts_with('L') && name.ends_with(';') {
        crate::native_engine::disassembly::type_name(name)
    } else {
        name.replace('/', ".")
    }
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
        let semantic_edges = args["semantic_edges"].as_bool().unwrap_or(false);
        ensure!(
            !semantic_edges || direction == "callers",
            "semantic_edges requires direction=callers"
        );
        ensure!(
            !semantic_edges || !(interface_dispatch || superclass_dispatch),
            "semantic_edges already includes transitive interface and virtual dispatch; omit dispatch_through_interfaces and dispatch_through_superclasses"
        );
        Ok(Self {
            interface_dispatch,
            superclass_dispatch,
            semantic_edges,
            direction,
            depth: if direct_only { 1 } else { requested_depth },
            direct_only,
            projection,
            package_prefix: string_option(args, "package_prefix")?
                .map(package_filter)
                .transpose()?,
            class_name: string_option(args, "class_name")?.map(class_filter),
            annotation: string_option(args, "annotation")?,
            component_type,
            offset,
            limit,
            visit_limit: None,
            lambda_creators_only: false,
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

    /// Whether a declaration is loaded and carries instructions to scan.
    fn body_state(&self, method: &str) -> &'static str {
        let Some(position) = self.method_position(method) else {
            return "not_loaded";
        };
        let declared = owner(method)
            .and_then(|name| self.class(name))
            .and_then(|class| class.methods.get(position));
        match declared {
            Some(m) if m.code.is_some() => "code",
            Some(_) => "no_code",
            None => "not_loaded",
        }
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
        if options
            .class_name
            .as_deref()
            .is_some_and(|expected| class != expected)
        {
            return false;
        }
        if options.package_prefix.as_deref().is_some_and(|prefix| {
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

/// Method bodies whose instructions cannot be decoded contribute no call sites.
/// They are reported with the result instead of failing or silently vanishing.
#[derive(Clone, Debug, Default)]
pub(super) struct ScanFailures {
    count: usize,
    samples: Vec<Value>,
}
impl ScanFailures {
    const SAMPLE_LIMIT: usize = 25;
    fn record(&mut self, method: &str, error: &anyhow::Error) {
        self.count += 1;
        if self.samples.len() < Self::SAMPLE_LIMIT {
            self.samples
                .push(json!({"method_id":method,"error":format!("{error:#}")}));
        }
    }
    pub(super) fn count(&self) -> usize {
        self.count
    }
    fn json(&self) -> Value {
        json!({"count":self.count,"samples":self.samples,"sample_limit":Self::SAMPLE_LIMIT,
            "semantics":"Method bodies whose DEX instructions could not be decoded (reserved opcodes, truncated instructions or invalid symbol indexes). Such bodies do not pass DEX verification, so they contribute no executable calls; they are listed here rather than failing the query. Counted within the scanned scope: every loaded method for caller or bidirectional queries, reachable bodies only for callee queries."})
    }
}

/// Scan one body for invokes and `new-instance` allocations, converting malformed
/// bytecode into a reported gap. Cancellation still propagates as an error.
pub(super) fn scan_method(
    class: &DexClass,
    method: &crate::native_dex::DexMethod,
    cancel: &AtomicBool,
    failures: &mut ScanFailures,
) -> Result<(Vec<native_xrefs::CallSite>, Vec<native_xrefs::Allocation>)> {
    match native_xrefs::method_sites(class, method, cancel) {
        Ok(sites) => Ok(sites),
        Err(error) => {
            check_cancel(cancel)?;
            failures.record(
                &crate::native_engine::disassembly::method_id(method),
                &error,
            );
            Ok((Vec::new(), Vec::new()))
        }
    }
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
    let (provided, cached) = indexed_sites(&catalog, &normalized, &options, cancel, cache)?;
    let mut result = query_catalog_with_sites(catalog, &normalized, args, cancel, Some(provided))?;
    result["invoke_index_cached"] = json!(cached);
    result["invoke_index"] = cache.status();
    Ok(result)
}

/// Build the graph the GUI window draws, sharing the project's invoke index with
/// MCP queries so reverse traversals stop rescanning every class. Callers and
/// bidirectional graphs read the index; callee graphs scan reachable bodies only.
/// Expansion stops after `NODE_LIMIT` methods, like the legacy builder, so a
/// popular root or a deep bidirectional walk cannot visit the whole app before
/// the projection discards everything past the limit anyway. With
/// `lambda_creators`, a Callers graph also follows `lambda_creation` edges from
/// the methods allocating a callback class to its instance methods; each edge
/// reports them in `inferred`, apart from its declared `sites`.
pub(crate) fn graph_cached(
    engine: &NativeDexEngine,
    root: &str,
    depth: usize,
    direction: Direction,
    lambda_creators: bool,
    cancel: &AtomicBool,
    cache: &Cache,
) -> Result<CallGraph> {
    check_cancel(cancel)?;
    let catalog = Catalog::new(engine.graph_classes(), cancel)?;
    graph_cached_catalog(
        catalog,
        root,
        depth,
        direction,
        lambda_creators,
        cancel,
        cache,
    )
}
fn graph_cached_catalog(
    catalog: Catalog<'_>,
    root: &str,
    depth: usize,
    direction: Direction,
    lambda_creators: bool,
    cancel: &AtomicBool,
    cache: &Cache,
) -> Result<CallGraph> {
    ensure!(
        !lambda_creators || direction == Direction::Callers,
        "Lambda creators apply to Callers graphs"
    );
    let args = json!({"direction":direction.as_str(),"depth":depth,"projection":"graph"});
    let mut options = Options::parse(&args)?;
    options.visit_limit = Some(NODE_LIMIT);
    options.semantic_edges = lambda_creators;
    options.lambda_creators_only = lambda_creators;
    let normalized = crate::method_lookup::graph_selector(root, |name| catalog.class(name))?;
    let provided = match direction {
        Direction::Callees => None,
        Direction::Callers | Direction::Both => {
            Some(indexed_sites(&catalog, &normalized, &options, cancel, cache)?.0)
        }
    };
    let traversal = traverse(&catalog, &normalized, &options, cancel, provided)?;
    let mut graph = project_graph(&catalog, &traversal, &normalized, &options, cancel)?.graph;
    graph.direction = direction;
    Ok(graph)
}

/// Collect every declared invoke a reverse or bidirectional traversal needs from
/// the shared index, building the index first when the cache is empty.
fn indexed_sites(
    catalog: &Catalog<'_>,
    normalized: &str,
    options: &Options<'_>,
    cancel: &AtomicBool,
    cache: &Cache,
) -> Result<(Provided, bool)> {
    let (index, cached) = cache.get(catalog)?;
    let canonical = catalog.canonical(normalized);
    let dispatch = dispatch_aliases(catalog, normalized, options, cancel)?;
    // Reverse shards are keyed by the resolved declaration, so an alias naming a
    // non-declaring ancestor is found through its own resolution. Only sites whose
    // raw target names an ancestor type of the root class qualify.
    let alias_keys: BTreeSet<String> = dispatch
        .aliases
        .iter()
        .map(|alias| catalog.canonical(alias))
        .collect();
    let mut initial = vec![];
    for key in &alias_keys {
        for site in index.neighbors(catalog, key, true)? {
            if dispatch.aliases.contains(&site.callee)
                && matches!(site.opcode, 0x6e | 0x74 | 0x72 | 0x78)
            {
                initial.push(site);
            }
        }
    }
    let mut report = SemanticReport::default();
    let (sites, semantic) = collect(
        catalog,
        &canonical,
        options,
        &*index,
        initial,
        cancel,
        &mut report,
    )?;
    Ok((
        Provided {
            sites,
            semantic,
            report,
            failures: index.failures.clone(),
        },
        cached,
    ))
}

fn callee_sites(
    catalog: &Catalog<'_>,
    root: &str,
    depth: usize,
    cancel: &AtomicBool,
    failures: &mut ScanFailures,
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
        for site in scan_method(class, &class.methods[position], cancel, failures)?.0 {
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

/// Ancestor-typed invokes that may dispatch to the root at runtime.
struct Dispatch {
    aliases: HashSet<String>,
    status: &'static str,
    /// The first loaded superclass declaration of the member that the root
    /// does not override; invokes resolving to it never reach the root.
    stopped_at: Option<String>,
}

/// A supertype of the root class is an alias owner for the root member, whether
/// or not that type redeclares it: an `invoke-virtual` or `invoke-interface`
/// whose receiver is statically typed as a supertype may run the root override.
/// Superclasses count only while the root overrides their declarations under
/// the runtime's rules; siblings and unrelated types never qualify. Only a
/// concrete instance method declared by its own class can be such a target.
fn dispatch_aliases(
    catalog: &Catalog<'_>,
    root: &str,
    options: &Options<'_>,
    cancel: &AtomicBool,
) -> Result<Dispatch> {
    if !(options.interface_dispatch || options.superclass_dispatch) {
        return Ok(Dispatch {
            aliases: HashSet::new(),
            status: "not_requested",
            stopped_at: None,
        });
    }
    dispatch_aliases_for(
        catalog,
        root,
        options.interface_dispatch,
        options.superclass_dispatch,
        cancel,
    )
}

/// Package of a dotted class name; the empty string for the default package.
fn package(class: &str) -> &str {
    class.rsplit_once('.').map_or("", |(package, _)| package)
}

/// Alias owners for `root` through interfaces and/or superclasses.
///
/// Superclasses are walked in order under the override rules the runtime
/// applies: the root overrides a loaded ancestor's virtual declaration only when
/// it is not final and is public, protected, or package-private in the package
/// of the method overriding it (overriding is transitive, so a same-package
/// intermediate override carries a cross-package child through). A declaration
/// the root cannot override, and the non-declaring superclasses passed since
/// the last override, are not aliases: an invoke on any of them resolves to that
/// declaration. A final or foreign package-private declaration also ends the
/// walk, since it holds the vtable slot of everything above it. A private or
/// static declaration is a direct method outside the vtable: an invoke that
/// resolves to it fails, and the root still overrides declarations above it.
/// Unloaded superclasses stay aliases: nothing can be proven about them.
/// Interfaces are collected from every loaded class in the ancestry, since
/// interface selection starts at the receiver class and reaches the root method
/// before any non-overriding ancestor.
fn dispatch_aliases_for(
    catalog: &Catalog<'_>,
    root: &str,
    interfaces: bool,
    superclasses: bool,
    cancel: &AtomicBool,
) -> Result<Dispatch> {
    let none = |status| {
        Ok(Dispatch {
            aliases: HashSet::new(),
            status,
            stopped_at: None,
        })
    };
    let Some((owner_name, class)) = owner(root).and_then(|name| Some((name, catalog.class(name)?)))
    else {
        return none("skipped_root_class_not_loaded");
    };
    let member = &root[owner_name.len() + 1..];
    let Some(position) = catalog.method_position(root) else {
        return none("skipped_root_not_declared_in_its_class");
    };
    let method = &class.methods[position];
    if member.starts_with('<') || method.access_flags & 0xa != 0 {
        return none("skipped_constructor_static_or_private_root");
    }
    if class.access_flags & 0x200 != 0 || method.access_flags & 0x400 != 0 {
        return none("skipped_interface_or_abstract_root");
    }
    const BUDGET: &str = "Dispatch ancestry exceeds budget; query ancestor methods explicitly";
    let mut aliases = HashSet::new();
    let mut seen = HashSet::from([class.descriptor.clone()]);
    let mut interface_queue: VecDeque<std::sync::Arc<str>> =
        class.interfaces.iter().cloned().collect();
    // Superclass chain: `pending` holds loaded ancestors that do not declare the
    // member, committed once the next declaration proves overridable.
    let mut pending = Vec::new();
    let mut overrider_package = package(owner_name).to_owned();
    let mut stopped_at: Option<String> = None;
    let mut ended = false;
    let mut next = class.superclass.clone();
    while let Some(descriptor) = next.take() {
        check_cancel(cancel)?;
        if !seen.insert(descriptor.clone()) {
            break;
        }
        ensure!(seen.len() <= 256, BUDGET);
        let name = crate::native_engine::disassembly::type_name(&descriptor);
        let id = format!("{name}.{member}");
        let Some(ancestor) = catalog.class(&name) else {
            if superclasses && !ended {
                aliases.extend(pending.drain(..));
                aliases.insert(id);
            }
            break;
        };
        interface_queue.extend(ancestor.interfaces.iter().cloned());
        next = ancestor.superclass.clone();
        if ended {
            continue;
        }
        let Some(position) = catalog.method_position(&id) else {
            pending.push(id);
            continue;
        };
        let flags = ancestor.methods[position].access_flags;
        let direct = flags & 0xa != 0;
        let overridable = !direct
            && flags & 0x10 == 0
            && (flags & 0x5 != 0 || package(&name) == overrider_package);
        if !overridable {
            pending.clear();
            stopped_at.get_or_insert(id);
            ended = !direct;
            continue;
        }
        overrider_package = package(&name).to_owned();
        if superclasses {
            aliases.extend(pending.drain(..));
            aliases.insert(id);
        } else {
            pending.clear();
        }
    }
    // A chain that ends in a loaded class proves nothing against the ancestors
    // still pending: an interface may declare the member they inherit.
    if superclasses {
        aliases.extend(pending);
    }
    while let Some(descriptor) = interface_queue.pop_front() {
        check_cancel(cancel)?;
        if !seen.insert(descriptor.clone()) {
            continue;
        }
        ensure!(seen.len() <= 256 && interface_queue.len() <= 4096, BUDGET);
        let name = crate::native_engine::disassembly::type_name(&descriptor);
        if interfaces {
            aliases.insert(format!("{name}.{member}"));
        }
        if let Some(interface) = catalog.class(&name) {
            interface_queue.extend(interface.interfaces.iter().cloned());
        }
    }
    Ok(Dispatch {
        aliases,
        status: "applied",
        stopped_at,
    })
}

fn record(site: &native_xrefs::CallSite, depth: usize, resolved: &str) -> Value {
    json!({"caller_method_id":site.caller,"caller_signature":site.caller,"callee_method_id":site.callee,"raw_callee_method_id":site.callee,"resolved_callee_method_id":resolved,"call_site_offset":site.pc,"depth":depth,
        "invoke_instruction":crate::native_engine::disassembly::invoke_text(&site.words, site.pc, &site.callee),"dex_entry":site.dex_entry,"method_index":site.method_index,"invoke_opcode":site.opcode,"invoke_words":site.words,"caller_code_offset_bytes":site.code_offset,"dispatch":site.dispatch})
}

/// Caller-side evidence source: the project disk index for MCP queries, or an
/// in-memory full scan that serves as the oracle the index must agree with.
trait Lookup {
    /// Declared invokes into (`reverse`) or out of a canonical method id.
    fn neighbors(
        &self,
        catalog: &Catalog<'_>,
        method: &str,
        reverse: bool,
    ) -> Result<Vec<native_xrefs::CallSite>>;
    /// Whether any invoke names `raw_callee` verbatim, before alias resolution.
    fn references_raw(&self, raw_callee: &str) -> bool;
    /// Every `new-instance` of `class`, with its allocating method.
    fn allocators(
        &self,
        catalog: &Catalog<'_>,
        class: &str,
    ) -> Result<Vec<native_xrefs::Allocation>>;
}

struct ScanLookup {
    sites: Vec<native_xrefs::CallSite>,
    by_caller: HashMap<String, Vec<usize>>,
    by_callee: HashMap<String, Vec<usize>>,
    raw_callees: HashSet<String>,
    allocations: HashMap<String, Vec<native_xrefs::Allocation>>,
}
impl ScanLookup {
    fn new(
        catalog: &Catalog<'_>,
        sites: Vec<native_xrefs::CallSite>,
        allocations: Vec<native_xrefs::Allocation>,
    ) -> Result<Self> {
        let mut by_caller: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_callee: HashMap<String, Vec<usize>> = HashMap::new();
        let mut raw_callees = HashSet::new();
        let mut canonical = HashMap::new();
        for (i, site) in sites.iter().enumerate() {
            check_cancel(catalog.cancel)?;
            by_caller.entry(site.caller.clone()).or_default().push(i);
            let key = canonical
                .entry(site.callee.clone())
                .or_insert_with(|| catalog.canonical(&site.callee))
                .clone();
            by_callee.entry(key).or_default().push(i);
            raw_callees.insert(site.callee.clone());
        }
        let mut grouped: HashMap<String, Vec<native_xrefs::Allocation>> = HashMap::new();
        for allocation in allocations {
            grouped
                .entry(allocation.class.clone())
                .or_default()
                .push(allocation);
        }
        Ok(Self {
            sites,
            by_caller,
            by_callee,
            raw_callees,
            allocations: grouped,
        })
    }
}
impl Lookup for ScanLookup {
    fn neighbors(
        &self,
        _: &Catalog<'_>,
        method: &str,
        reverse: bool,
    ) -> Result<Vec<native_xrefs::CallSite>> {
        let map = if reverse {
            &self.by_callee
        } else {
            &self.by_caller
        };
        Ok(map
            .get(method)
            .into_iter()
            .flatten()
            .map(|&i| self.sites[i].clone())
            .collect())
    }
    fn references_raw(&self, raw_callee: &str) -> bool {
        self.raw_callees.contains(raw_callee)
    }
    fn allocators(&self, _: &Catalog<'_>, class: &str) -> Result<Vec<native_xrefs::Allocation>> {
        Ok(self.allocations.get(class).cloned().unwrap_or_default())
    }
}

/// Bytecode backing a semantic edge: an ancestor-typed invoke, or the
/// `new-instance` that creates a callback object.
enum Evidence {
    Invoke(native_xrefs::CallSite),
    Allocation(native_xrefs::Allocation, &'static str),
}

/// A caller-side relationship that no DEX invoke states directly, labelled by
/// the producer that inferred it. `from` runs before `to` at runtime.
struct SemanticEdge {
    from: String,
    to: String,
    kind: &'static str,
    evidence: Evidence,
}
impl SemanticEdge {
    fn pc(&self) -> usize {
        match &self.evidence {
            Evidence::Invoke(site) => site.pc,
            Evidence::Allocation(allocation, _) => allocation.pc,
        }
    }
    fn key(&self) -> (&'static str, String, usize, String) {
        (self.kind, self.from.clone(), self.pc(), self.to.clone())
    }
}

/// Coverage of the semantic producers for one query.
#[derive(Default)]
struct SemanticReport {
    expanded: usize,
    skipped: usize,
    samples: Vec<Value>,
}
impl SemanticReport {
    const METHOD_LIMIT: usize = 2048;
    const KINDS: [&'static str; 3] = [
        "lambda_creation",
        "possible_interface_dispatch",
        "possible_virtual_dispatch",
    ];
    fn skip(&mut self, method: &str, reason: &str) {
        self.skipped += 1;
        if self.samples.len() < ScanFailures::SAMPLE_LIMIT {
            self.samples
                .push(json!({"method_id":method,"reason":reason}));
        }
    }
    fn json(&self, enabled: bool) -> Value {
        if !enabled {
            return json!({"enabled":false});
        }
        json!({"enabled":true,"kinds":Self::KINDS,"complete":self.skipped == 0,"methods_expanded":self.expanded,
            "methods_skipped":self.skipped,"skipped_samples":self.samples,"sample_limit":ScanFailures::SAMPLE_LIMIT,"method_budget":Self::METHOD_LIMIT,
            "semantics":"Opt-in labelled edges that no DEX invoke states directly, produced for every traversed method of a caller query and never replacing declared calls. lambda_creation: a new-instance of a callback class (synthetic, anonymous $N, Lambda-named, or extending a Kotlin lambda/continuation base) links the allocating method to each concrete instance method of that class; the allocation is exact, the callback relationship is inferred. possible_interface_dispatch and possible_virtual_dispatch: invoke-interface or invoke-virtual sites whose raw target names the traversed member on an interface or unloaded supertype of its class, or on a loaded superclass whose declaration the traversed method overrides under runtime access rules (a final or foreign-package package-private declaration ends the superclass walk; a private or static one excludes only the classes resolving to it). Not covered: invoke-custom (indy) lambdas, named listener classes without those markers, reflection, LiveData/Flow/RxJava value delivery, dependency-injection factories, Intent, Handler or executor delivery. complete=false when the method budget or an ancestry budget skipped a method; skipped_samples lists them."})
    }
}

const LAMBDA_BASES: [&str; 5] = [
    "kotlin.jvm.internal.Lambda",
    "kotlin.coroutines.jvm.internal.SuspendLambda",
    "kotlin.coroutines.jvm.internal.RestrictedSuspendLambda",
    "kotlin.coroutines.jvm.internal.ContinuationImpl",
    "kotlin.coroutines.jvm.internal.BaseContinuationImpl",
];

/// Classes whose instance methods run as callbacks after construction rather
/// than through a direct invoke from application code, with the marker matched.
fn callback_class(class: &DexClass) -> Option<&'static str> {
    if class.access_flags & 0x1000 != 0 {
        return Some("synthetic_class");
    }
    let name = crate::native_engine::disassembly::type_name(&class.descriptor);
    let simple = name.rsplit('.').next().unwrap_or(&name);
    if let Some((_, tail)) = simple.rsplit_once('$')
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        return Some("anonymous_class");
    }
    if simple.contains("Lambda") {
        return Some("lambda_named_class");
    }
    let parent = class
        .superclass
        .as_deref()
        .map(crate::native_engine::disassembly::type_name);
    if parent.is_some_and(|parent| LAMBDA_BASES.contains(&parent.as_str())) {
        return Some("kotlin_lambda_base");
    }
    None
}

/// Semantic producers for one traversed method: ancestor-typed invokes that may
/// run it as an override, and allocations of its class when that class is a
/// callback carrier. `lambda_creators_only` keeps the allocation producer alone.
fn semantic_into(
    catalog: &Catalog<'_>,
    method: &str,
    options: &Options<'_>,
    lookup: &dyn Lookup,
    cancel: &AtomicBool,
    report: &mut SemanticReport,
) -> Result<Vec<SemanticEdge>> {
    let mut edges = Vec::new();
    if report.expanded >= SemanticReport::METHOD_LIMIT {
        report.skip(method, "semantic method budget reached");
        return Ok(edges);
    }
    report.expanded += 1;
    let dispatch = if options.lambda_creators_only {
        Ok(None)
    } else {
        dispatch_aliases_for(catalog, method, true, true, cancel).map(Some)
    };
    match dispatch {
        Ok(None) => {}
        Ok(Some(dispatch)) => {
            let keys: BTreeSet<String> = dispatch
                .aliases
                .iter()
                .filter(|alias| lookup.references_raw(alias))
                .map(|alias| catalog.canonical(alias))
                .collect();
            for key in &keys {
                for site in lookup.neighbors(catalog, key, true)? {
                    if dispatch.aliases.contains(&site.callee)
                        && matches!(site.opcode, 0x6e | 0x74 | 0x72 | 0x78)
                    {
                        let kind = if matches!(site.opcode, 0x72 | 0x78) {
                            "possible_interface_dispatch"
                        } else {
                            "possible_virtual_dispatch"
                        };
                        edges.push(SemanticEdge {
                            from: site.caller.clone(),
                            to: method.into(),
                            kind,
                            evidence: Evidence::Invoke(site),
                        });
                    }
                }
            }
        }
        Err(error) => {
            check_cancel(cancel)?;
            report.skip(method, &format!("{error:#}"));
        }
    }
    if let Some((owner_name, class)) =
        owner(method).and_then(|name| Some((name, catalog.class(name)?)))
        && let Some(position) = catalog.method_position(method)
        && let Some(marker) = callback_class(class)
    {
        let member = &method[owner_name.len() + 1..];
        let declared = &class.methods[position];
        if !member.starts_with('<') && declared.access_flags & 0x40a == 0 {
            for allocation in lookup.allocators(catalog, owner_name)? {
                edges.push(SemanticEdge {
                    from: allocation.method.clone(),
                    to: method.into(),
                    kind: "lambda_creation",
                    evidence: Evidence::Allocation(allocation, marker),
                });
            }
        }
    }
    Ok(edges)
}

/// Caller-side traversal shared by the disk index and the in-memory oracle.
/// Declared invokes reach callers; with `semantic_edges`, every traversed method
/// also contributes the producers above, and their sources are traversed too.
fn collect(
    catalog: &Catalog<'_>,
    root_key: &str,
    options: &Options<'_>,
    lookup: &dyn Lookup,
    initial: Vec<native_xrefs::CallSite>,
    cancel: &AtomicBool,
    report: &mut SemanticReport,
) -> Result<(Vec<native_xrefs::CallSite>, Vec<SemanticEdge>)> {
    let mut queue = VecDeque::from([(root_key.to_string(), 0usize)]);
    let mut visited = HashSet::from([root_key.to_string()]);
    let mut seen_sites = HashSet::new();
    let mut sites = Vec::new();
    for site in initial {
        if seen_sites.insert((site.caller.clone(), site.pc, site.callee.clone())) {
            sites.push(site);
        }
    }
    let mut seen_edges = HashSet::new();
    let mut semantic = Vec::new();
    while let Some((current, depth)) = queue.pop_front() {
        check_cancel(cancel)?;
        if depth >= options.depth {
            continue;
        }
        for reverse in [true, false] {
            if !reverse && options.direction == "callers" {
                continue;
            }
            for site in lookup.neighbors(catalog, &current, reverse)? {
                let next = if reverse {
                    site.caller.clone()
                } else {
                    catalog.canonical(&site.callee)
                };
                if visited.insert(next.clone())
                    && options
                        .visit_limit
                        .is_none_or(|limit| visited.len() <= limit)
                {
                    queue.push_back((next, depth + 1));
                }
                if seen_sites.insert((site.caller.clone(), site.pc, site.callee.clone())) {
                    sites.push(site);
                }
            }
        }
        if options.semantic_edges {
            for edge in semantic_into(catalog, &current, options, lookup, cancel, report)? {
                if visited.insert(edge.from.clone()) {
                    queue.push_back((edge.from.clone(), depth + 1));
                }
                if seen_edges.insert(edge.key()) {
                    semantic.push(edge);
                }
            }
        }
    }
    Ok((sites, semantic))
}

/// Traversal output handed to the projection stage by a caller-side collector.
struct Provided {
    sites: Vec<native_xrefs::CallSite>,
    semantic: Vec<SemanticEdge>,
    report: SemanticReport,
    failures: ScanFailures,
}

fn semantic_record(edge: &SemanticEdge, depth: usize, catalog: &Catalog<'_>) -> Value {
    let mut row = match &edge.evidence {
        Evidence::Invoke(site) => {
            let mut row = record(site, depth, &catalog.canonical(&site.callee));
            row["confidence"] = json!("possible");
            row["possible_implementation_method_id"] = json!(edge.to);
            row
        }
        Evidence::Allocation(allocation, marker) => json!({
            "caller_method_id":allocation.method,"caller_signature":allocation.method,
            "callee_method_id":Value::Null,"raw_callee_method_id":Value::Null,"resolved_callee_method_id":Value::Null,
            "call_site_offset":allocation.pc,"depth":depth,
            "instruction":format!("new-instance v{}, {}", allocation.register, allocation.descriptor),
            "opcode":"new-instance","opcode_value":0x22,"instruction_words":allocation.words,
            "destination_register":allocation.register,"type_index":allocation.type_index,
            "allocated_class_id":allocation.class,"allocated_descriptor":allocation.descriptor,
            "callback_class_marker":marker,"dex_entry":allocation.dex_entry,
            "caller_code_offset_bytes":allocation.code_offset,"dispatch":"allocation (no invoke)",
            "confidence":"exact_allocation"
        }),
    };
    row["relationship"] = json!("semantic_edge");
    row["kind"] = json!(edge.kind);
    row["semantic_target_method_id"] = json!(edge.to);
    row
}

fn query_catalog(
    catalog: Catalog<'_>,
    root: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    query_catalog_with_sites(catalog, root, args, cancel, None)
}
/// Everything a depth-bounded traversal learned before projection.
struct Traversal {
    root_key: String,
    dispatch: Dispatch,
    sites: Vec<native_xrefs::CallSite>,
    semantic: Vec<SemanticEdge>,
    report: SemanticReport,
    failures: ScanFailures,
    /// Number of declared sites; higher indexes into `keys` are semantic edges.
    declared: usize,
    /// Canonical (caller, callee) pair per declared site or semantic edge.
    keys: Vec<(String, String)>,
    canonical_cache: HashMap<String, String>,
    depths: HashMap<String, usize>,
    /// Sites whose neighbor passed the filters, keyed by site with the least depth.
    selected_sites: BTreeMap<usize, usize>,
    traversed_sites: HashSet<usize>,
}

/// Walk declared calls from a normalized root up to the requested depth.
/// Filters never prune the walk; they only decide which sites are selected.
fn traverse(
    catalog: &Catalog<'_>,
    root: &str,
    options: &Options<'_>,
    cancel: &AtomicBool,
    provided: Option<Provided>,
) -> Result<Traversal> {
    check_cancel(cancel)?;
    let root_key = catalog.canonical(root);
    let dispatch = dispatch_aliases(catalog, root, options, cancel)?;
    let aliases = &dispatch.aliases;
    // Outgoing queries scan only reachable bodies. Reverse traversal needs the
    // project-wide call index, independently of graph projection limits.
    let mut scan = ScanFailures::default();
    let mut report = SemanticReport::default();
    let mut semantic = Vec::new();
    let (mut sites, failures) = if let Some(provided) = provided {
        semantic = provided.semantic;
        report = provided.report;
        (provided.sites, provided.failures)
    } else if options.direction == "callees" {
        let sites = callee_sites(catalog, &root_key, options.depth, cancel, &mut scan)?;
        (sites, scan)
    } else {
        let mut sites = Vec::new();
        let mut allocations = Vec::new();
        for class in catalog.classes.values() {
            check_cancel(cancel)?;
            for method in &class.methods {
                let (calls, allocated) = scan_method(class, method, cancel, &mut scan)?;
                sites.extend(calls);
                if options.semantic_edges {
                    allocations.extend(allocated);
                }
            }
        }
        if options.semantic_edges {
            // The oracle path runs the same collector as the disk index so both
            // agree on which methods were expanded.
            let lookup = ScanLookup::new(catalog, sites, allocations)?;
            let (collected, edges) = collect(
                catalog,
                &root_key,
                options,
                &lookup,
                Vec::new(),
                cancel,
                &mut report,
            )?;
            semantic = edges;
            sites = collected;
        }
        (sites, scan)
    };
    sites.sort_by(|a, b| (&a.caller, a.pc, &a.callee).cmp(&(&b.caller, b.pc, &b.callee)));
    semantic
        .sort_by(|a, b| (a.kind, &a.from, a.pc(), &a.to).cmp(&(b.kind, &b.from, b.pc(), &b.to)));
    let declared = sites.len();
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
    // Semantic edges only ever point towards the traversed method, so they join
    // the reverse adjacency and never the forward one.
    for (j, edge) in semantic.iter().enumerate() {
        check_cancel(cancel)?;
        let from = canonical_cache
            .entry(edge.from.clone())
            .or_insert_with(|| catalog.canonical(&edge.from))
            .clone();
        incoming
            .entry(edge.to.clone())
            .or_default()
            .push(declared + j);
        keys.push((from, edge.to.clone()));
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
                .or_insert_with(|| catalog.matches(next, options))
            {
                selected_sites
                    .entry(i)
                    .and_modify(|d| *d = (*d).min(depth + 1))
                    .or_insert(depth + 1);
            }
        }
    }
    check_cancel(cancel)?;
    Ok(Traversal {
        root_key,
        dispatch,
        sites,
        semantic,
        report,
        failures,
        declared,
        keys,
        canonical_cache,
        depths,
        selected_sites,
        traversed_sites,
    })
}

/// Retained nodes and edges, with the totals the limits were applied to.
struct GraphProjection {
    graph: CallGraph,
    matching_nodes: usize,
    matching_edges: usize,
}

/// Keep the root plus every traversed declaration that passes the filters, then
/// connect retained nodes. Declared invokes and semantic edges between the same
/// declarations are counted apart so a graph edge never hides its evidence.
fn project_graph(
    catalog: &Catalog<'_>,
    traversal: &Traversal,
    root: &str,
    options: &Options<'_>,
    cancel: &AtomicBool,
) -> Result<GraphProjection> {
    let (dispatch_targets, dispatch_targets_truncated) =
        dispatch_targets_with(|name| catalog.class(name), root, cancel)?;
    let mut methods: Vec<_> = traversal
        .depths
        .iter()
        .filter(|(method, _)| **method != traversal.root_key && catalog.matches(method, options))
        .map(|(method, &depth)| (method.clone(), depth))
        .collect();
    methods.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
    methods.insert(0, (traversal.root_key.clone(), 0));
    let matching_nodes = methods.len();
    let matching_keys: HashSet<_> = methods.iter().map(|(method, _)| method.as_str()).collect();
    let mut counts: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();
    for &i in &traversal.traversed_sites {
        check_cancel(cancel)?;
        let (from, to) = &traversal.keys[i];
        if matching_keys.contains(from.as_str()) && matching_keys.contains(to.as_str()) {
            let entry = counts
                .entry((from.clone(), to.clone()))
                .or_insert((0usize, 0usize));
            if i < traversal.declared {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
    }
    let matching_edges = counts.len();
    methods.truncate(NODE_LIMIT);
    let indices: HashMap<_, _> = methods
        .iter()
        .enumerate()
        .map(|(i, (method, _))| (method.clone(), i))
        .collect();
    let mut graph = CallGraph {
        dispatch_targets,
        dispatch_targets_truncated,
        ..Default::default()
    };
    for (i, (method, depth)) in methods.into_iter().enumerate() {
        check_cancel(cancel)?;
        graph.nodes.push(Node {
            component: catalog.component(&method),
            available: catalog.available(&method),
            method: if i == 0 { root.into() } else { method },
            depth,
        });
    }
    for ((from, to), (sites, inferred)) in counts {
        if let (Some(&from), Some(&to)) = (indices.get(&from), indices.get(&to)) {
            if graph.edges.len() == EDGE_LIMIT {
                break;
            }
            graph.edges.push(Edge {
                from,
                to,
                sites,
                inferred,
            });
        }
    }
    graph.truncated = matching_nodes > graph.nodes.len() || matching_edges > graph.edges.len();
    Ok(GraphProjection {
        graph,
        matching_nodes,
        matching_edges,
    })
}

fn query_catalog_with_sites(
    catalog: Catalog<'_>,
    root: &str,
    args: &Value,
    cancel: &AtomicBool,
    provided: Option<Provided>,
) -> Result<Value> {
    let options = Options::parse(args)?;
    let normalized_root = crate::method_lookup::graph_selector(root, |name| catalog.class(name))?;
    let root = normalized_root.as_str();
    let t = traverse(&catalog, root, &options, cancel, provided)?;
    let root_key = t.root_key.as_str();
    let aliases = &t.dispatch.aliases;
    let declared = t.declared;
    let boundary = t.depths.values().any(|&depth| depth == options.depth);
    let filters = json!({"package_prefix":&options.package_prefix,"class_name":&options.class_name,"annotation":options.annotation,
        "component_type":options.component_type.map(|c| c.label().to_ascii_lowercase())});
    let metadata = json!({
        "root_method_id":root,"resolved_root_method_id":root_key,"root_body":catalog.body_state(root_key),
        "direction":options.direction,"depth":options.depth,"direct_only":options.direct_only,
        "projection":options.projection,"filters":filters,"depth_boundary_reached":boundary,
        "depth_boundary_semantics":"true when a traversed method lies at the requested depth; its own neighbors were not expanded, so deeper relationships may exist. false means every method reachable in the requested direction was expanded before the depth limit.",
        "scan_complete":t.failures.count() == 0,"undecodable_methods":t.failures.json(),
        "filter_semantics":"Filters select canonical neighbor declarations after traversal; they do not prune traversal. Compact records match the neighbor in the requested traversal direction. Graph nodes retain matching declarations plus the root; edges connect retained nodes. package_prefix and class_name accept dotted, slash or descriptor forms; a trailing dot is ignored.",
        "annotation_semantics":"Exact DEX descriptor or fully qualified type name on the class or method declaration; combined filters use AND.",
        "component_semantics":"Static superclass ancestry classification.",
        "semantic_edges":t.report.json(options.semantic_edges),
        "analysis":if options.semantic_edges {
            "static DEX declared calls plus labelled semantic edges (lambda_creation from new-instance allocations of callback classes; possible_interface_dispatch and possible_virtual_dispatch from supertype-typed invokes) for every traversed method; inherited aliases resolved through known superclasses; no reflection, Intent target, invoke-custom (lambda) target or data-flow inference"
        } else {
            "static DEX declared calls; inherited aliases resolved through known superclasses (not through interface default methods); no runtime dispatch, reflection, Intent target or invoke-custom (lambda) target inference"
        }
    });
    let mut result = metadata;
    if root_key != root {
        result["root_resolution_note"] = json!(
            "method_id names an inherited member that its class does not declare. Traversal uses the declaring ancestor resolved_root_method_id, so callers include invokes naming any class that resolves to that declaration."
        );
    }
    if options.projection == "call_sites" {
        let total = t.selected_sites.len();
        result["direct_matching_total"] = json!(
            t.selected_sites
                .values()
                .filter(|&&depth| depth == 1)
                .count()
        );
        result["call_site_semantics"] = json!(if options.semantic_edges {
            "Records mix two kinds of evidence, told apart by relationship. exact_declared_call and possible_*_dispatch records are one raw DEX invoke each to callee_method_id. semantic_edge records carry kind: possible_interface_dispatch and possible_virtual_dispatch are also one invoke each, while lambda_creation is one new-instance allocation (opcode new-instance, callee_method_id null, allocated_class_id set) that links the allocating method to semantic_target_method_id. depth is traversal distance from the query root; only depth=1 records are direct root neighbors. call_sites defaults to depth=1; explicit depth includes transitive sites."
        } else {
            "Each record is one raw DEX invoke to callee_method_id. depth is traversal distance from the query root; only depth=1 records are direct root neighbors. call_sites defaults to depth=1; explicit depth includes transitive sites."
        });
        let record_for = |i: usize, depth: usize| {
            if i >= declared {
                return semantic_record(&t.semantic[i - declared], depth, &catalog);
            }
            let mut row = record(&t.sites[i], depth, &t.canonical_cache[&t.sites[i].callee]);
            let inferred = aliases.contains(&t.sites[i].callee)
                && matches!(t.sites[i].opcode, 0x6e | 0x74 | 0x72 | 0x78);
            row["relationship"] = json!(if inferred {
                if matches!(t.sites[i].opcode, 0x72 | 0x78) {
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
        let inferred = t
            .selected_sites
            .keys()
            .filter(|&&i| {
                i < declared
                    && aliases.contains(&t.sites[i].callee)
                    && matches!(t.sites[i].opcode, 0x6e | 0x74 | 0x72 | 0x78)
            })
            .count();
        let mut semantic_kinds: BTreeMap<&str, usize> = BTreeMap::new();
        for &i in t.selected_sites.keys().filter(|&&i| i >= declared) {
            *semantic_kinds
                .entry(t.semantic[i - declared].kind)
                .or_default() += 1;
        }
        let semantic_selected: usize = semantic_kinds.values().sum();
        result["exact_matching_total"] = json!(total - inferred - semantic_selected);
        result["possible_dispatch_total"] = json!(inferred);
        result["semantic_edge_total"] = json!(semantic_selected);
        result["semantic_edge_kinds"] = json!(semantic_kinds);
        result["dispatch_through_interfaces"] = json!(options.interface_dispatch);
        result["dispatch_through_superclasses"] = json!(options.superclass_dispatch);
        result["dispatch_expansion"] = json!(t.dispatch.status);
        result["dispatch_superclass_stop"] = json!(t.dispatch.stopped_at);
        result["dispatch_expansion_semantics"] = json!(
            "applied: invoke-virtual/invoke-interface sites whose raw target names the root member on a supertype of the root class (declaring or not) are added as possible dispatch. Superclasses count only while the root overrides their declarations under runtime rules. The first loaded superclass declaration the root does not override is named in dispatch_superclass_stop: neither it nor the non-declaring superclasses below it are inferred, because an invoke on any of them resolves to that declaration. A final or foreign-package package-private declaration also excludes every superclass above it (it holds their vtable slot); a private or static declaration is a direct method, so superclasses above it stay possible. Unloaded superclasses and interfaces anywhere in the ancestry remain possible. invoke-super, direct and static calls and sibling or unrelated receiver types are never added. skipped_*: no inference because the root is not a concrete instance method declared by its own loaded class."
        );
        let mut hash = Sha256::new();
        for (&i, &depth) in &t.selected_sites {
            check_cancel(cancel)?;
            hash.update(serde_json::to_vec(&record_for(i, depth))?);
            hash.update(b"\n");
        }
        let records: Vec<_> = t
            .selected_sites
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
    let GraphProjection {
        graph,
        matching_nodes,
        matching_edges,
    } = project_graph(&catalog, &t, root, &options, cancel)?;
    let masks = graph.component_reachability();
    let kinds = |mask: u8| {
        ["Activity", "Service", "Receiver", "Provider"]
            .into_iter()
            .enumerate()
            .filter_map(|(i, kind)| ((mask & (1 << i)) != 0).then_some(kind))
            .collect::<Vec<_>>()
    };
    result["nodes"] = json!(graph.nodes.iter().enumerate().map(|(i,n)| json!({"id":i,"method_id":n.method,"depth":n.depth,"component":n.component.map(|c| c.label()),"available":n.available,"reachable_components":kinds(masks[i])})).collect::<Vec<_>>());
    result["edges"] = json!(graph.edges.iter().map(|e| {
        let mut edge = json!({"from":e.from,"to":e.to,"call_sites":e.sites,"highlighted":masks[e.to] != 0,"reachable_components":kinds(masks[e.to])});
        if options.semantic_edges {
            edge["semantic_edges"] = json!(e.inferred);
        }
        edge
    }).collect::<Vec<_>>());
    result["root"] = json!(0);
    result["truncated"] = json!(graph.truncated);
    result["node_limit"] = json!(NODE_LIMIT);
    result["edge_limit"] = json!(EDGE_LIMIT);
    result["matching_node_count"] = json!(matching_nodes);
    result["matching_edge_count"] = json!(matching_edges);
    result["dispatch_targets"] = json!(graph.dispatch_targets);
    result["dispatch_targets_truncated"] = json!(graph.dispatch_targets_truncated);
    result["dispatch_note"] = json!(
        "Related ancestor declarations are possible dispatch entry points; query their callers separately."
    );
    result["node_id_semantics"] = json!(
        "Non-root method_id values are declaring methods resolved through known superclasses; the root keeps the requested id and resolves to resolved_root_method_id. call_sites counts raw DEX invokes between the two declarations."
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

    #[test]
    fn superclass_dispatch_respects_override_rules_across_packages() {
        let cancel = AtomicBool::new(false);
        // Chain q.Child -> p.Mid -> p.Base -> r.Top -> x.Unknown (unloaded).
        // p.Base.run() is package-private and q.Child.run() lives in another
        // package, so it does not override it: a call through any superclass
        // stays on p.Base at runtime, which also holds r.Top's vtable slot.
        let base = |flags: u32| {
            let mut base = class("p.Base", Some("r.Top"), vec![("run", vec![], vec![])]);
            base.methods[0].access_flags = flags;
            base
        };
        let mid = |declares_run: bool| {
            let member = if declares_run { "run" } else { "other" };
            let mut mid = class("p.Mid", Some("p.Base"), vec![(member, vec![], vec![])]);
            mid.methods[0].access_flags = 1;
            mid
        };
        let mut top = class("r.Top", Some("x.Unknown"), vec![("run", vec![], vec![])]);
        top.methods[0].access_flags = 1;
        let mut child = class("q.Child", Some("p.Mid"), vec![("run", vec![], vec![])]);
        child.methods[0].access_flags = 1;
        let targets = [
            "p.Base.run()V",
            "p.Mid.run()V",
            "r.Top.run()V",
            "x.Unknown.run()V",
        ];
        let mut caller = class("p.Caller", None, vec![("call", vec![], targets.to_vec())]);
        let body = caller.methods[0].code.as_mut().unwrap();
        for k in 0..targets.len() {
            body.instructions[1 + 4 * k] = 0x106e;
        }
        body.outs = 1;
        let query = |base: &DexClass, mid: &DexClass, args: &Value| {
            let catalog = Catalog::new([base, mid, &top, &child, &caller], &cancel).unwrap();
            query_cached_catalog(catalog, "q.Child.run()V", args, &cancel, &Cache::default())
                .unwrap()
        };
        let receivers = |result: &Value| -> Vec<String> {
            let mut names: Vec<String> = result["call_sites"]
                .as_array()
                .unwrap()
                .iter()
                .map(|site| site["raw_callee_method_id"].as_str().unwrap().to_owned())
                .collect();
            names.sort();
            names
        };
        let explicit = json!({"direction":"callers","projection":"call_sites","dispatch_through_superclasses":true});
        let semantic = json!({"direction":"callers","projection":"call_sites","depth":2,"semantic_edges":true});

        let result = query(&base(0), &mid(false), &explicit);
        assert_eq!(result["matching_total"], 0, "{result}");
        assert_eq!(result["dispatch_expansion"], "applied");
        assert_eq!(result["dispatch_superclass_stop"], "p.Base.run()V");
        let result = query(&base(0), &mid(false), &semantic);
        assert_eq!(result["matching_total"], 0, "{result}");

        // A protected declaration is overridable from any package: every
        // superclass receiver type is a possible target.
        let result = query(&base(4), &mid(false), &explicit);
        assert_eq!(receivers(&result), targets, "{result}");
        assert_eq!(result["possible_dispatch_total"], 4);
        assert_eq!(result["dispatch_superclass_stop"], Value::Null);
        let result = query(&base(4), &mid(false), &semantic);
        assert_eq!(
            result["semantic_edge_kinds"],
            json!({"possible_virtual_dispatch":4}),
            "{result}"
        );

        // Overriding is transitive: a same-package override in p.Mid of the
        // package-private method carries the cross-package child through.
        let result = query(&base(0), &mid(true), &explicit);
        assert_eq!(receivers(&result), targets, "{result}");
        assert_eq!(result["dispatch_superclass_stop"], Value::Null);

        // A final declaration ends the walk the same way as a foreign one.
        let result = query(&base(0x11), &mid(false), &explicit);
        assert_eq!(result["matching_total"], 0, "{result}");
        assert_eq!(result["dispatch_superclass_stop"], "p.Base.run()V");

        // A private or static declaration is a direct method: invokes resolving
        // to it fail, while the child still overrides r.Top.run() above it.
        for flags in [2, 8] {
            let result = query(&base(flags), &mid(false), &explicit);
            assert_eq!(receivers(&result), &targets[2..], "{result}");
            assert_eq!(result["dispatch_superclass_stop"], "p.Base.run()V");
        }
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
    fn visit_limit_stops_expanding_but_keeps_discovered_leaves() {
        let classes = vec![
            class("a.A", None, vec![("run", vec![], vec!["a.B.run()V"])]),
            class("a.B", None, vec![("run", vec![], vec!["a.C.run()V"])]),
            class("a.C", None, vec![("run", vec![], vec!["a.D.run()V"])]),
            class("a.D", None, vec![("run", vec![], vec![])]),
        ];
        let cancel = AtomicBool::new(false);
        let catalog = Catalog::new(&classes, &cancel).unwrap();
        let (index, _) = Cache::default().get(&catalog).unwrap();
        let args = json!({"direction":"callers","depth":20});
        let callers = |limit: Option<usize>| {
            let mut options = Options::parse(&args).unwrap();
            options.visit_limit = limit;
            let mut report = SemanticReport::default();
            let (sites, _) = collect(
                &catalog,
                "a.D.run()V",
                &options,
                &*index,
                vec![],
                &cancel,
                &mut report,
            )
            .unwrap();
            let mut callers: Vec<String> = sites.into_iter().map(|site| site.caller).collect();
            callers.sort();
            callers
        };
        // Unbounded: the whole chain. Two expansions: D and C expand, B stays a leaf
        // and A is never reached.
        assert_eq!(callers(None), ["a.A.run()V", "a.B.run()V", "a.C.run()V"]);
        assert_eq!(callers(Some(2)), ["a.B.run()V", "a.C.run()V"]);
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
    fn undecodable_bodies_are_reported_instead_of_failing_or_vanishing() {
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
        // Direct callee queries decode only the root body, which is valid.
        let direct = run(
            &classes,
            "app.Root.run()V",
            json!({"projection":"call_sites","direction":"callees","direct_only":true}),
        );
        assert_eq!(direct["matching_total"], 1);
        assert_eq!(direct["scan_complete"], true);
        assert_eq!(direct["undecodable_methods"]["count"], 0);
        // Transitive callee queries reach the broken body and say so.
        let deep = run(
            &classes,
            "app.Root.run()V",
            json!({"projection":"call_sites","direction":"callees","depth":2}),
        );
        assert_eq!(deep["matching_total"], 1);
        assert_eq!(deep["scan_complete"], false);
        assert_eq!(deep["undecodable_methods"]["count"], 1);
        assert_eq!(
            deep["undecodable_methods"]["samples"][0]["method_id"],
            "app.Middle.run()V"
        );
        assert_eq!(
            deep["undecodable_methods"]["samples"][0]["error"],
            "Invalid method index"
        );
        // Caller queries scan every body: both broken ones are counted, by scan
        // and by the disk index alike.
        let args = json!({"projection":"call_sites","direction":"callers","direct_only":true});
        let scanned = run(&classes, "app.Root.run()V", args.clone());
        assert_eq!(scanned["matching_total"], 0);
        assert_eq!(scanned["scan_complete"], false);
        assert_eq!(scanned["undecodable_methods"]["count"], 2);
        let cancel = AtomicBool::new(false);
        let cache = Cache::default();
        let indexed = query_cached_catalog(
            Catalog::new(&classes, &cancel).unwrap(),
            "app.Root.run()V",
            &args,
            &cancel,
            &cache,
        )
        .unwrap();
        assert_eq!(indexed["scan_complete"], false);
        assert_eq!(indexed["undecodable_methods"]["count"], 2);
        assert_eq!(indexed["invoke_index"]["undecodable_methods"], 2);
        let names: Vec<_> = indexed["undecodable_methods"]["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["method_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["app.Middle.run()V", "app.Root.unrelated()V"]);
        // Cancellation still wins over tolerance.
        let cancelled = AtomicBool::new(true);
        assert!(
            query_catalog(
                Catalog::new(&classes, &AtomicBool::new(false)).unwrap(),
                "app.Root.run()V",
                &args,
                &cancelled
            )
            .is_err()
        );
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

    #[test]
    fn filters_normalize_trailing_dots_descriptors_and_slashes() {
        let classes = vec![
            class("app.work.Good", None, vec![("run", vec![], vec![])]),
            class(
                "app.Root",
                None,
                vec![("entry", vec![], vec!["app.work.Good.run()V"])],
            ),
            class(
                "other.Bad",
                None,
                vec![("entry", vec![], vec!["app.work.Good.run()V"])],
            ),
        ];
        let callers = |extra: Value| {
            let mut args = json!({"projection":"call_sites","direction":"callers"});
            for (k, v) in extra.as_object().unwrap() {
                args[k] = v.clone();
            }
            run(&classes, "app.work.Good.run()V", args)
        };
        assert_eq!(callers(json!({}))["matching_total"], 2);
        for prefix in ["app", "app.", "app/", ".app."] {
            let result = callers(json!({"package_prefix":prefix}));
            assert_eq!(result["matching_total"], 1, "{prefix}");
            assert_eq!(result["filters"]["package_prefix"], "app", "{prefix}");
            assert_eq!(
                result["call_sites"][0]["caller_method_id"],
                "app.Root.entry()V"
            );
        }
        assert_eq!(callers(json!({"package_prefix":"ap"}))["matching_total"], 0);
        assert_eq!(
            callers(json!({"package_prefix":"other."}))["matching_total"],
            1
        );
        for name in ["app.Root", "Lapp/Root;", "app/Root"] {
            let result = callers(json!({"class_name":name}));
            assert_eq!(result["matching_total"], 1, "{name}");
            assert_eq!(result["filters"]["class_name"], "app.Root", "{name}");
        }
        assert_eq!(
            callers(json!({"class_name":"app.Nope"}))["matching_total"],
            0
        );
        let cancel = AtomicBool::new(false);
        for prefix in [".", "/", ""] {
            assert!(
                query_catalog(
                    Catalog::new(&classes, &cancel).unwrap(),
                    "app.work.Good.run()V",
                    &json!({"projection":"call_sites","direction":"callers","package_prefix":prefix}),
                    &cancel
                )
                .is_err(),
                "{prefix:?}"
            );
        }
    }

    #[test]
    fn resolved_ids_expose_inherited_root_and_callee_declarations() {
        let fixture = |abstract_parent: bool| {
            let mut parent = class("app.Parent", None, vec![("run", vec![], vec![])]);
            parent.methods[0].access_flags = 1;
            if abstract_parent {
                parent.access_flags = 0x401;
                parent.methods[0].access_flags = 0x401;
                parent.methods[0].code = None;
            }
            vec![
                parent,
                class("app.Child", Some("app.Parent"), vec![]),
                class(
                    "app.Caller",
                    None,
                    vec![("entry", vec![], vec!["app.Child.run()V"])],
                ),
            ]
        };
        let classes = fixture(false);
        let callers = run(
            &classes,
            "app.Child.run()V",
            json!({"projection":"call_sites","direction":"callers"}),
        );
        assert_eq!(callers["root_method_id"], "app.Child.run()V");
        assert_eq!(callers["resolved_root_method_id"], "app.Parent.run()V");
        assert_eq!(callers["root_body"], "code");
        assert!(
            callers["root_resolution_note"]
                .as_str()
                .unwrap()
                .contains("declaring ancestor")
        );
        assert!(callers["depth_boundary_semantics"].is_string());
        assert_eq!(callers["matching_total"], 1);
        let site = &callers["call_sites"][0];
        assert_eq!(site["raw_callee_method_id"], "app.Child.run()V");
        assert_eq!(site["callee_method_id"], "app.Child.run()V");
        assert_eq!(site["resolved_callee_method_id"], "app.Parent.run()V");
        assert_eq!(site["relationship"], "exact_declared_call");
        let declared = run(
            &classes,
            "app.Parent.run()V",
            json!({"projection":"call_sites","direction":"callers"}),
        );
        assert_eq!(declared["resolved_root_method_id"], "app.Parent.run()V");
        assert!(declared.get("root_resolution_note").is_none());
        assert_eq!(declared["matching_total"], 1);
        let callees = run(
            &classes,
            "app.Caller.entry()V",
            json!({"projection":"call_sites","direction":"callees"}),
        );
        assert_eq!(
            callees["call_sites"][0]["resolved_callee_method_id"],
            "app.Parent.run()V"
        );
        let graph = run(
            &classes,
            "app.Caller.entry()V",
            json!({"projection":"graph","direction":"callees"}),
        );
        assert_eq!(graph["nodes"][1]["method_id"], "app.Parent.run()V");
        assert!(graph["node_id_semantics"].is_string());
        // Abstract and external roots describe their body state honestly.
        let no_code = run(
            &fixture(true),
            "app.Child.run()V",
            json!({"projection":"call_sites","direction":"callers"}),
        );
        assert_eq!(no_code["root_body"], "no_code");
        assert_eq!(no_code["matching_total"], 1);
        let external = run(
            &classes,
            "ext.Lib.run()V",
            json!({"projection":"call_sites","direction":"callers"}),
        );
        assert_eq!(external["root_body"], "not_loaded");
        assert_eq!(external["resolved_root_method_id"], "ext.Lib.run()V");
        assert_eq!(external["matching_total"], 0);
    }

    #[test]
    fn dispatch_expansion_covers_non_declaring_ancestors_and_excludes_siblings() {
        let cancel = AtomicBool::new(false);
        let mut base = class(
            "p.Base",
            Some("ext.Framework"),
            vec![("run", vec![], vec![])],
        );
        base.methods[0].access_flags = 1;
        let intermediate = class("p.Intermediate", Some("p.Base"), vec![]);
        let sibling = class("p.Sibling", Some("p.Base"), vec![]);
        let mut iface = class("p.Iface", None, vec![("run", vec![], vec![])]);
        iface.access_flags = 0x201;
        iface.methods[0].access_flags = 0x401;
        iface.methods[0].code = None;
        let mut sub_iface = class("p.SubIface", None, vec![]);
        sub_iface.access_flags = 0x201;
        sub_iface.interfaces = vec!["Lp/Iface;".into()];
        let mut impl_class = class(
            "p.Impl",
            Some("p.Intermediate"),
            vec![("run", vec![], vec![])],
        );
        impl_class.interfaces = vec!["Lp/SubIface;".into()];
        impl_class.methods[0].access_flags = 1;
        let mut caller = class(
            "p.Caller",
            None,
            vec![(
                "call",
                vec![],
                vec![
                    "p.Intermediate.run()V",
                    "p.Sibling.run()V",
                    "p.Base.run()V",
                    "p.SubIface.run()V",
                    "p.Iface.run()V",
                    "p.Impl.run()V",
                    "unknown.Ext.run()V",
                    "ext.Framework.run()V",
                    "p.Intermediate.run()V",
                ],
            )],
        );
        let body = caller.methods[0].code.as_mut().unwrap();
        body.outs = 1;
        for (call, opcode) in [
            (0, 0x106e),
            (1, 0x106e),
            (2, 0x106e),
            (3, 0x1072),
            (4, 0x1072),
            (5, 0x106e),
            (6, 0x106e),
            (7, 0x106e),
            (8, 0x106f),
        ] {
            body.instructions[call * 4 + 1] = opcode;
        }
        let classes = vec![
            base,
            intermediate,
            sibling,
            iface,
            sub_iface,
            impl_class,
            caller,
        ];
        let cache = Cache::default();
        let both = |root: &str, args: Value| {
            let scanned = run(&classes, root, args.clone());
            let mut indexed = query_cached_catalog(
                Catalog::new(&classes, &cancel).unwrap(),
                root,
                &args,
                &cancel,
                &cache,
            )
            .unwrap();
            let object = indexed.as_object_mut().unwrap();
            object.remove("invoke_index");
            object.remove("invoke_index_cached");
            assert_eq!(indexed, scanned, "{root} {args}");
            scanned
        };
        let raw = |result: &Value| -> Vec<String> {
            result["call_sites"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["raw_callee_method_id"].as_str().unwrap().to_owned())
                .collect()
        };
        let full = both(
            "p.Impl.run()V",
            json!({"projection":"call_sites","direction":"callers","dispatch_through_interfaces":true,"dispatch_through_superclasses":true}),
        );
        assert_eq!(full["dispatch_expansion"], "applied");
        assert_eq!(full["matching_total"], 6);
        assert_eq!(full["exact_matching_total"], 1);
        assert_eq!(full["possible_dispatch_total"], 5);
        let targets = raw(&full);
        for expected in [
            "p.Intermediate.run()V",
            "p.Base.run()V",
            "p.SubIface.run()V",
            "p.Iface.run()V",
            "p.Impl.run()V",
            "ext.Framework.run()V",
        ] {
            assert!(
                targets.contains(&expected.to_owned()),
                "{expected} in {targets:?}"
            );
        }
        for excluded in ["p.Sibling.run()V", "unknown.Ext.run()V"] {
            assert!(!targets.contains(&excluded.to_owned()), "{excluded}");
        }
        assert_eq!(
            targets
                .iter()
                .filter(|t| t.as_str() == "p.Intermediate.run()V")
                .count(),
            1,
            "invoke-super must not be inferred as dispatch"
        );
        for site in full["call_sites"].as_array().unwrap() {
            if site["relationship"] != "exact_declared_call" {
                assert_eq!(site["possible_implementation_method_id"], "p.Impl.run()V");
            }
        }
        let supers = both(
            "p.Impl.run()V",
            json!({"projection":"call_sites","direction":"callers","dispatch_through_superclasses":true}),
        );
        assert_eq!(supers["possible_dispatch_total"], 3);
        assert!(!raw(&supers).contains(&"p.Iface.run()V".to_owned()));
        let interfaces = both(
            "p.Impl.run()V",
            json!({"projection":"call_sites","direction":"callers","dispatch_through_interfaces":true}),
        );
        assert_eq!(interfaces["possible_dispatch_total"], 2);
        assert!(!raw(&interfaces).contains(&"p.Base.run()V".to_owned()));
        let plain = both(
            "p.Impl.run()V",
            json!({"projection":"call_sites","direction":"callers"}),
        );
        assert_eq!(plain["dispatch_expansion"], "not_requested");
        assert_eq!(plain["matching_total"], 1);
        // A root its class does not declare resolves to the declaring ancestor
        // and performs no dispatch inference; every invoke resolving there is exact.
        let inherited = both(
            "p.Intermediate.run()V",
            json!({"projection":"call_sites","direction":"callers","dispatch_through_interfaces":true,"dispatch_through_superclasses":true}),
        );
        assert_eq!(
            inherited["dispatch_expansion"],
            "skipped_root_not_declared_in_its_class"
        );
        assert_eq!(inherited["resolved_root_method_id"], "p.Base.run()V");
        assert_eq!(inherited["exact_matching_total"], 4);
        assert_eq!(inherited["possible_dispatch_total"], 0);
        let external = both(
            "unknown.Ext.run()V",
            json!({"projection":"call_sites","direction":"callers","dispatch_through_superclasses":true}),
        );
        assert_eq!(
            external["dispatch_expansion"],
            "skipped_root_class_not_loaded"
        );
        assert_eq!(external["matching_total"], 1);
        let abstract_root = both(
            "p.Iface.run()V",
            json!({"projection":"call_sites","direction":"callers","dispatch_through_interfaces":true}),
        );
        assert_eq!(
            abstract_root["dispatch_expansion"],
            "skipped_interface_or_abstract_root"
        );
        assert_eq!(abstract_root["matching_total"], 1);
    }

    /// Splice a `new-instance` of `target` before the final return of a fixture method.
    pub(super) fn allocate(class: &mut DexClass, method: usize, target: &str) {
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        let index = symbols.types.len() as u16;
        symbols
            .types
            .push(format!("L{};", target.replace('.', "/")).into());
        let code = class.methods[method].code.as_mut().unwrap();
        let end = code.instructions.len() - 1;
        code.instructions.splice(end..end, [0x0022, index]);
    }

    /// `app.Outer.start` allocates the anonymous listener `app.Outer$1`, whose
    /// `onEvent` calls `app.Sink.handle`; `app.Bus.fire` invokes the listener
    /// interface, `app.Scheduler.go` calls `fire`, and `app.Outer.other` calls `start`.
    fn listener_fixture() -> Vec<DexClass> {
        let mut outer = class(
            "app.Outer",
            None,
            vec![
                ("start", vec![], vec![]),
                ("other", vec![], vec!["app.Outer.start()V"]),
            ],
        );
        allocate(&mut outer, 0, "app.Outer$1");
        let mut callback = class(
            "app.Outer$1",
            None,
            vec![("onEvent", vec![], vec!["app.Sink.handle()V"])],
        );
        callback.interfaces = vec!["Lapi/Listener;".into()];
        callback.methods[0].access_flags = 1;
        let mut listener = class("api.Listener", None, vec![("onEvent", vec![], vec![])]);
        listener.access_flags = 0x201;
        listener.methods[0].access_flags = 0x401;
        listener.methods[0].code = None;
        let sink = class("app.Sink", None, vec![("handle", vec![], vec![])]);
        let mut bus = class(
            "app.Bus",
            None,
            vec![("fire", vec![], vec!["api.Listener.onEvent()V"])],
        );
        bus.methods[0].code.as_mut().unwrap().instructions[1] = 0x1072;
        let scheduler = class(
            "app.Scheduler",
            None,
            vec![("go", vec![], vec!["app.Bus.fire()V"])],
        );
        vec![outer, callback, listener, sink, bus, scheduler]
    }

    #[test]
    fn gui_lambda_creators_follow_allocations_and_skip_dispatch_guesses() {
        let classes = listener_fixture();
        let cancel = AtomicBool::new(false);
        let cache = Cache::default();
        let graph = |root: &str, direction: Direction, creators: bool| {
            graph_cached_catalog(
                Catalog::new(&classes, &cancel).unwrap(),
                root,
                5,
                direction,
                creators,
                &cancel,
                &cache,
            )
        };
        let shape = |graph: &CallGraph| {
            let nodes: Vec<(String, usize)> = graph
                .nodes
                .iter()
                .map(|n| (n.method.clone(), n.depth))
                .collect();
            let edges: Vec<(String, String, usize, usize)> = graph
                .edges
                .iter()
                .map(|e| {
                    (
                        graph.nodes[e.from].method.clone(),
                        graph.nodes[e.to].method.clone(),
                        e.sites,
                        e.inferred,
                    )
                })
                .collect();
            (nodes, edges)
        };
        let root = "app.Outer$1.onEvent()V";
        // Declared calls alone never reach the callback method.
        let plain = graph(root, Direction::Callers, false).unwrap();
        assert_eq!(shape(&plain), (vec![(root.to_owned(), 0)], vec![]));
        // The creator joins through its allocation and the walk continues from it,
        // while the interface dispatch from app.Bus.fire stays out of the graph.
        let creators = graph(root, Direction::Callers, true).unwrap();
        assert_eq!(creators.direction, Direction::Callers);
        assert!(!creators.truncated);
        assert_eq!(
            shape(&creators),
            (
                vec![
                    (root.to_owned(), 0),
                    ("app.Outer.start()V".to_owned(), 1),
                    ("app.Outer.other()V".to_owned(), 2),
                ],
                vec![
                    (
                        "app.Outer.other()V".to_owned(),
                        "app.Outer.start()V".to_owned(),
                        1,
                        0,
                    ),
                    ("app.Outer.start()V".to_owned(), root.to_owned(), 0, 1),
                ],
            )
        );
        assert_eq!(
            creators
                .edges
                .iter()
                .map(Edge::inferred_only)
                .collect::<Vec<_>>(),
            [false, true]
        );
        assert!(creators.nodes.iter().all(|n| n.available));
        // Other directions refuse the option before touching the index.
        for direction in [Direction::Callees, Direction::Both] {
            let error = graph(root, direction, true).unwrap_err();
            assert!(error.to_string().contains("Callers graphs"), "{error:#}");
        }
        // The MCP graph projection keeps every producer and its edge fields.
        let full = query_cached_catalog(
            Catalog::new(&classes, &cancel).unwrap(),
            root,
            &json!({"direction":"callers","projection":"graph","depth":5,"semantic_edges":true}),
            &cancel,
            &cache,
        )
        .unwrap();
        let methods: BTreeSet<&str> = full["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["method_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            methods,
            BTreeSet::from([
                root,
                "app.Outer.start()V",
                "app.Outer.other()V",
                "app.Bus.fire()V",
                "app.Scheduler.go()V",
            ])
        );
        let edges = full["edges"].as_array().unwrap();
        let inferred: Vec<_> = edges.iter().filter(|e| e["call_sites"] == 0).collect();
        assert_eq!(inferred.len(), 2, "{edges:?}");
        for edge in &inferred {
            assert_eq!(edge["semantic_edges"], 1);
            let keys: BTreeSet<&str> = edge
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                BTreeSet::from([
                    "from",
                    "to",
                    "call_sites",
                    "highlighted",
                    "reachable_components",
                    "semantic_edges",
                ])
            );
        }
    }

    #[test]
    fn semantic_edges_follow_lambda_creation_and_transitive_dispatch_in_both_paths() {
        let classes = listener_fixture();
        let cancel = AtomicBool::new(false);
        let cache = Cache::default();
        let cached = |root: &str, args: &Value| {
            let mut actual = query_cached_catalog(
                Catalog::new(&classes, &cancel).unwrap(),
                root,
                args,
                &cancel,
                &cache,
            )
            .unwrap();
            let object = actual.as_object_mut().unwrap();
            object.remove("invoke_index");
            object.remove("invoke_index_cached");
            actual
        };
        let root = "app.Sink.handle()V";
        let deep = json!({"direction":"callers","projection":"call_sites","depth":5,"semantic_edges":true});
        let result = run(&classes, root, deep.clone());
        assert_eq!(result, cached(root, &deep), "{result}");
        assert_eq!(result["matching_total"], 5, "{result}");
        assert_eq!(result["exact_matching_total"], 3);
        assert_eq!(result["semantic_edge_total"], 2);
        assert_eq!(result["possible_dispatch_total"], 0);
        assert_eq!(
            result["semantic_edge_kinds"],
            json!({"lambda_creation":1,"possible_interface_dispatch":1})
        );
        assert_eq!(result["semantic_edges"]["enabled"], true);
        assert_eq!(result["semantic_edges"]["complete"], true);
        assert_eq!(result["semantic_edges"]["methods_expanded"], 6);
        assert!(
            result["analysis"]
                .as_str()
                .unwrap()
                .contains("lambda_creation")
        );
        let semantics = result["call_site_semantics"].as_str().unwrap();
        assert!(
            semantics.contains("lambda_creation is one new-instance allocation"),
            "{semantics}"
        );
        assert!(semantics.contains("callee_method_id null"), "{semantics}");
        let rows = result["call_sites"].as_array().unwrap();
        let exact: Vec<_> = rows
            .iter()
            .filter(|r| r["relationship"] == "exact_declared_call")
            .map(|r| (r["caller_method_id"].clone(), r["depth"].clone()))
            .collect();
        assert_eq!(
            exact,
            vec![
                (json!("app.Outer$1.onEvent()V"), json!(1)),
                (json!("app.Outer.other()V"), json!(3)),
                (json!("app.Scheduler.go()V"), json!(3)),
            ]
        );
        assert!(
            rows.iter()
                .filter(|r| r["relationship"] == "exact_declared_call")
                .all(|r| r.get("kind").is_none())
        );
        let lambda = rows
            .iter()
            .find(|r| r["kind"] == "lambda_creation")
            .unwrap();
        assert_eq!(lambda["relationship"], "semantic_edge");
        assert_eq!(lambda["caller_method_id"], "app.Outer.start()V");
        assert_eq!(
            lambda["semantic_target_method_id"],
            "app.Outer$1.onEvent()V"
        );
        assert_eq!(lambda["allocated_class_id"], "app.Outer$1");
        assert_eq!(lambda["callback_class_marker"], "anonymous_class");
        assert_eq!(lambda["call_site_offset"], 0);
        assert_eq!(lambda["depth"], 2);
        assert_eq!(lambda["confidence"], "exact_allocation");
        assert!(lambda["callee_method_id"].is_null());
        assert_eq!(lambda["instruction"], "new-instance v0, Lapp/Outer$1;");
        assert_eq!(lambda["opcode_value"], 0x22);
        assert!(lambda.get("invoke_opcode").is_none());
        let dispatch = rows
            .iter()
            .find(|r| r["kind"] == "possible_interface_dispatch")
            .unwrap();
        assert_eq!(dispatch["relationship"], "semantic_edge");
        assert_eq!(dispatch["caller_method_id"], "app.Bus.fire()V");
        assert_eq!(dispatch["raw_callee_method_id"], "api.Listener.onEvent()V");
        assert_eq!(dispatch["invoke_opcode"], 0x72);
        assert_eq!(dispatch["call_site_offset"], 1);
        assert_eq!(
            dispatch["possible_implementation_method_id"],
            "app.Outer$1.onEvent()V"
        );
        assert_eq!(dispatch["confidence"], "possible");
        assert_eq!(dispatch["depth"], 2);

        let plain = run(
            &classes,
            root,
            json!({"direction":"callers","projection":"call_sites","depth":5}),
        );
        assert_eq!(plain["matching_total"], 1);
        assert_eq!(plain["semantic_edge_total"], 0);
        assert_eq!(plain["semantic_edges"], json!({"enabled":false}));
        assert!(
            plain["call_site_semantics"]
                .as_str()
                .unwrap()
                .starts_with("Each record is one raw DEX invoke")
        );

        let shallow = json!({"direction":"callers","projection":"call_sites","depth":2,"semantic_edges":true});
        let result = run(&classes, root, shallow.clone());
        assert_eq!(result, cached(root, &shallow));
        assert_eq!(result["matching_total"], 3);
        assert_eq!(result["depth_boundary_reached"], true);

        let graph_args = json!({"direction":"callers","depth":5,"semantic_edges":true});
        let graph = run(&classes, root, graph_args.clone());
        assert_eq!(graph, cached(root, &graph_args));
        let nodes = graph["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 6);
        let depth_of =
            |id: &str| nodes.iter().find(|n| n["method_id"] == id).unwrap()["depth"].clone();
        assert_eq!(depth_of("app.Outer$1.onEvent()V"), 1);
        assert_eq!(depth_of("app.Bus.fire()V"), 2);
        assert_eq!(depth_of("app.Outer.start()V"), 2);
        assert_eq!(depth_of("app.Scheduler.go()V"), 3);
        let edges = graph["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 5);
        let inferred: Vec<_> = edges.iter().filter(|e| e["semantic_edges"] == 1).collect();
        assert_eq!(inferred.len(), 2);
        assert!(inferred.iter().all(|e| e["call_sites"] == 0));
        assert_eq!(
            edges
                .iter()
                .filter(|e| e["call_sites"] == 1 && e["semantic_edges"] == 0)
                .count(),
            3
        );
        let plain_graph = run(&classes, root, json!({"direction":"callers","depth":5}));
        assert_eq!(plain_graph["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(plain_graph["edges"].as_array().unwrap().len(), 1);
        assert!(plain_graph["edges"][0].get("semantic_edges").is_none());

        let direct = json!({"direction":"callers","projection":"call_sites","semantic_edges":true});
        let result = run(&classes, "app.Outer$1.onEvent()V", direct.clone());
        assert_eq!(result, cached("app.Outer$1.onEvent()V", &direct));
        assert_eq!(result["matching_total"], 2);
        assert_eq!(result["exact_matching_total"], 0);
        assert_eq!(result["direct_matching_total"], 2);

        let filtered = json!({"direction":"callers","projection":"call_sites","depth":5,"semantic_edges":true,"class_name":"app.Bus"});
        let result = run(&classes, root, filtered.clone());
        assert_eq!(result, cached(root, &filtered));
        assert_eq!(result["matching_total"], 1);
        assert_eq!(
            result["call_sites"][0]["kind"],
            "possible_interface_dispatch"
        );

        for args in [
            json!({"semantic_edges":true}),
            json!({"semantic_edges":true,"direction":"both"}),
            json!({"semantic_edges":true,"direction":"callers","dispatch_through_interfaces":true}),
            json!({"semantic_edges":true,"direction":"callers","projection":"call_sites","dispatch_through_superclasses":true}),
        ] {
            assert!(Options::parse(&args).is_err(), "{args}");
        }
    }

    #[test]
    fn semantic_method_budget_is_reported_as_incomplete_coverage() {
        let mut wide = vec![class("c.Root", None, vec![("m", vec![], vec![])])];
        for i in 0..SemanticReport::METHOD_LIMIT + 2 {
            wide.push(class(
                &format!("c.C{i}"),
                None,
                vec![("m", vec![], vec!["c.Root.m()V"])],
            ));
        }
        let result = run(
            &wide,
            "c.Root.m()V",
            json!({"direction":"callers","depth":2,"semantic_edges":true}),
        );
        assert_eq!(result["semantic_edges"]["complete"], false);
        assert_eq!(
            result["semantic_edges"]["methods_expanded"],
            SemanticReport::METHOD_LIMIT
        );
        assert_eq!(result["semantic_edges"]["methods_skipped"], 3);
        assert_eq!(
            result["semantic_edges"]["skipped_samples"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            result["semantic_edges"]["skipped_samples"][0]["reason"],
            "semantic method budget reached"
        );
        assert_eq!(
            result["matching_node_count"],
            SemanticReport::METHOD_LIMIT + 3
        );
        let unlimited = run(
            &wide,
            "c.Root.m()V",
            json!({"direction":"callers","depth":1,"semantic_edges":true}),
        );
        assert_eq!(unlimited["semantic_edges"]["complete"], true);
        assert_eq!(unlimited["semantic_edges"]["methods_expanded"], 1);
    }
}
