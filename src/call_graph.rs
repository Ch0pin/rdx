//! Bounded static call graph. Component classification follows DEX ancestry.
#[path = "call_graph_query.rs"]
mod query;
use crate::{native_engine::NativeDexEngine, native_xrefs};
use anyhow::{Context, Result, ensure};
pub use query::query;
pub(crate) use query::{Cache as QueryCache, graph_cached, query_cached};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    Activity,
    Service,
    Receiver,
    Provider,
}
impl Component {
    pub fn label(self) -> &'static str {
        match self {
            Self::Activity => "Activity",
            Self::Service => "Service",
            Self::Receiver => "Receiver",
            Self::Provider => "Provider",
        }
    }
}
#[derive(Debug)]
pub struct Node {
    pub method: String,
    pub depth: usize,
    pub component: Option<Component>,
    pub available: bool,
}
#[derive(Debug)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    /// Declared invoke sites folded into this edge.
    pub sites: usize,
    /// Inferred links folded into this edge, such as the allocation of a callback
    /// class by the caller; they never stand in for a declared call.
    pub inferred: usize,
}
impl Edge {
    /// No declared invoke supports this edge; it exists through inference alone.
    pub fn inferred_only(&self) -> bool {
        self.sites == 0 && self.inferred > 0
    }
}
/// Which call edges a graph follows from its root method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    /// Methods that call the root, then their callers.
    Callers,
    /// Methods the root calls, then their callees.
    #[default]
    Callees,
    /// Callers and callees together.
    Both,
}
impl Direction {
    pub const ALL: [Self; 3] = [Self::Callers, Self::Callees, Self::Both];
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == text)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Callers => "callers",
            Self::Callees => "callees",
            Self::Both => "both",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Callers => "Callers",
            Self::Callees => "Callees",
            Self::Both => "Both",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Callers => {
                "Methods that call the selected method, and their callers, up to the depth."
            }
            Self::Callees => {
                "Methods the selected method calls, and their callees, up to the depth."
            }
            Self::Both => "Callers and callees of the selected method together.",
        }
    }
}
#[derive(Debug, Default)]
pub struct CallGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub truncated: bool,
    pub dispatch_targets: Vec<String>,
    pub dispatch_targets_truncated: bool,
    /// Direction the graph was built in; edges always point from caller to callee.
    pub direction: Direction,
}
/// Manifest exported state of a class, merged over its own declarations and over declared
/// components that inherit from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exposure {
    /// `Some(true)` when any declaration is exported, `None` when a declaration leaves the flag
    /// unresolved, `Some(false)` when every declaration denies external access.
    pub exported: Option<bool>,
    /// One line per contributing manifest declaration.
    pub details: Vec<String>,
}

pub fn owner(method: &str) -> Option<&str> {
    method
        .split_once('(')?
        .0
        .rsplit_once('.')
        .map(|(owner, _)| owner)
}
fn component(mut name: String, parent: impl Fn(&str) -> Option<String>) -> Option<Component> {
    let mut seen = HashSet::new();
    for _ in 0..256 {
        let kind = match name.as_str() {
            "android.app.Activity" => Some(Component::Activity),
            "android.app.Service" => Some(Component::Service),
            "android.content.BroadcastReceiver" => Some(Component::Receiver),
            "android.content.ContentProvider" => Some(Component::Provider),
            _ => None,
        };
        if kind.is_some() {
            return kind;
        }
        if !seen.insert(name.clone()) {
            return None;
        }
        name = parent(&name)?;
    }
    None
}
pub(crate) fn parent(engine: &NativeDexEngine, name: &str) -> Option<String> {
    engine
        .class(name)?
        .superclass
        .as_deref()
        .map(crate::native_engine::disassembly::type_name)
}
/// Exported state per class, keyed by Java class name. Each declared component contributes to
/// its target class and to every loaded superclass, so a base class inherits the exposure of
/// the components built on it. `parent` returns the superclass of a loaded class, or `None`
/// for classes outside the DEX, which end the walk without an entry.
pub fn exposure_map(
    components: &[crate::manifest_summary::ManifestComponent],
    parent: impl Fn(&str) -> Option<String>,
) -> HashMap<String, Exposure> {
    let mut map: HashMap<String, Exposure> = HashMap::new();
    for component in components {
        if component.target.is_empty() {
            continue;
        }
        let state = if component.is_exported() {
            Some(true)
        } else if component.exported.starts_with("false") {
            Some(false)
        } else {
            None
        };
        let declaration = if component.kind == "activity-alias" {
            format!("alias {}: {}", component.name, component.exported)
        } else {
            format!("{}: {}", component.kind, component.exported)
        };
        let mut class = component.target.clone();
        let mut seen = HashSet::new();
        let mut inherited = false;
        while seen.insert(class.clone()) && seen.len() <= 256 {
            let next = parent(&class);
            if inherited && next.is_none() {
                break;
            }
            let entry = map.entry(class.clone()).or_insert_with(|| Exposure {
                exported: Some(false),
                details: Vec::new(),
            });
            entry.exported = match (entry.exported, state) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (None, _) | (_, None) => None,
                _ => Some(false),
            };
            entry.details.push(if inherited {
                format!("{declaration} (via subclass {})", component.target)
            } else {
                declaration.clone()
            });
            let Some(next) = next else { break };
            class = next;
            inherited = true;
        }
    }
    map
}
fn body(engine: &NativeDexEngine, target: &str) -> Option<(String, String)> {
    let original = owner(target)?;
    let member = target.strip_prefix(&format!("{original}."))?;
    let mut name = original.to_string();
    let mut seen = HashSet::new();
    for _ in 0..256 {
        if !seen.insert(name.clone()) {
            return None;
        }
        let class = engine.class(&name)?;
        let id = format!("{name}.{member}");
        if class
            .methods
            .iter()
            .any(|m| crate::native_engine::disassembly::method_id(m) == id)
        {
            return Some((name, id));
        }
        // Constructors are not inherited.
        if member.starts_with('<') {
            return None;
        }
        name = parent(engine, &name)?;
    }
    None
}
/// Resolve inherited aliases using available superclass metadata. External bodies
/// remain unavailable; this does not infer runtime virtual dispatch.
fn canonical(engine: &NativeDexEngine, target: &str) -> String {
    body(engine, target).map_or_else(|| target.into(), |(_, id)| id)
}

/// Ancestor declarations are possible dispatch entry points, not proven callers.
fn dispatch_targets_with<'a>(
    lookup: impl Fn(&str) -> Option<&'a crate::native_dex::DexClass>,
    root: &str,
    cancel: &AtomicBool,
) -> Result<(Vec<String>, bool)> {
    let Some(owner) = owner(root) else {
        return Ok((vec![], false));
    };
    let Some(class) = lookup(owner) else {
        return Ok((vec![], false));
    };
    let member = &root[owner.len() + 1..];
    if member.starts_with('<')
        || class
            .methods
            .iter()
            .find(|m| crate::native_engine::disassembly::method_id(m) == root)
            .is_some_and(|m| m.access_flags & 0xa != 0)
    {
        return Ok((vec![], false));
    }
    let mut queue = VecDeque::new();
    queue.extend(class.interfaces.iter().cloned());
    queue.extend(class.superclass.iter().cloned());
    let mut truncated = queue.len() > 256;
    queue.truncate(256);
    let mut seen = HashSet::from([class.descriptor.clone()]);
    let mut targets = Vec::new();
    while let Some(descriptor) = queue.pop_front() {
        ensure!(!cancel.load(Ordering::Relaxed), "Call graph cancelled");
        if !seen.insert(descriptor.clone()) {
            continue;
        }
        if seen.len() > 256 {
            return Ok((targets, true));
        }
        let name = crate::native_engine::disassembly::type_name(&descriptor);
        let Some(ancestor) = lookup(&name) else {
            continue;
        };
        let id = format!("{name}.{member}");
        if ancestor.methods.iter().any(|m| {
            m.access_flags & 0xa == 0 && crate::native_engine::disassembly::method_id(m) == id
        }) {
            targets.push(id);
        }
        queue.extend(ancestor.interfaces.iter().cloned());
        queue.extend(ancestor.superclass.iter().cloned());
        truncated |= queue.len() > 256;
        queue.truncate(256);
    }
    targets.sort();
    targets.dedup();
    Ok((targets, truncated))
}
pub fn build(
    engine: &NativeDexEngine,
    root: &str,
    depth: usize,
    cancel: &AtomicBool,
) -> Result<CallGraph> {
    build_direction(engine, root, depth, "callees", cancel)
}

pub fn build_direction(
    engine: &NativeDexEngine,
    root: &str,
    depth: usize,
    direction: &str,
    cancel: &AtomicBool,
) -> Result<CallGraph> {
    ensure!((1..=100).contains(&depth), "Call graph depth must be 1–100");
    let direction = Direction::parse(direction).context("Invalid direction")?;
    let root_owner = owner(root).context("Select a method")?;
    let mut incoming: HashMap<String, Vec<native_xrefs::CallSite>> = HashMap::new();
    if direction != Direction::Callees {
        for class in engine.graph_classes() {
            ensure!(!cancel.load(Ordering::Relaxed), "Call graph cancelled");
            for site in native_xrefs::call_sites(class, None, cancel)? {
                incoming
                    .entry(canonical(engine, &site.callee))
                    .or_default()
                    .push(site);
            }
        }
    }
    let (dispatch_targets, dispatch_targets_truncated) =
        dispatch_targets_with(|name| engine.class(name), root, cancel)?;
    let mut graph = CallGraph {
        dispatch_targets,
        dispatch_targets_truncated,
        direction,
        ..Default::default()
    };
    graph.nodes.push(Node {
        method: root.into(),
        depth: 0,
        component: component(root_owner.into(), |n| parent(engine, n)),
        available: body(engine, root).is_some(),
    });
    let mut indices = HashMap::from([(canonical(engine, root), 0)]);
    let mut queue = VecDeque::from([0]);
    let mut edge_indices: HashMap<(usize, usize), usize> = HashMap::new();
    let mut seen_sites = HashSet::new();
    while let Some(current) = queue.pop_front() {
        ensure!(!cancel.load(Ordering::Relaxed), "Call graph cancelled");
        if graph.nodes[current].depth >= depth {
            continue;
        }
        let method = &graph.nodes[current].method;
        let outgoing = if direction != Direction::Callers {
            if let Some((class, method)) = body(engine, method) {
                native_xrefs::call_sites(
                    engine.class(&class).context("Missing call owner")?,
                    Some(&method),
                    cancel,
                )?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let key = canonical(engine, method);
        let reverse = incoming.get(&key).into_iter().flatten().map(|s| (s, true));
        for (site, backwards) in outgoing.iter().map(|s| (s, false)).chain(reverse) {
            ensure!(!cancel.load(Ordering::Relaxed), "Call graph cancelled");
            if !seen_sites.insert((site.caller.clone(), site.pc)) {
                continue;
            }
            let next_method = if backwards {
                &site.caller
            } else {
                &site.callee
            };
            let next_key = canonical(engine, next_method);
            let next = if let Some(&index) = indices.get(&next_key) {
                index
            } else {
                if graph.nodes.len() >= 5000 {
                    graph.truncated = true;
                    continue;
                }
                let index = graph.nodes.len();
                graph.nodes.push(Node {
                    depth: graph.nodes[current].depth + 1,
                    component: owner(next_method)
                        .and_then(|n| component(n.into(), |n| parent(engine, n))),
                    available: body(engine, next_method).is_some(),
                    method: next_method.clone(),
                });
                indices.insert(next_key, index);
                queue.push_back(index);
                index
            };
            let (from, to) = if backwards {
                (next, current)
            } else {
                (current, next)
            };
            if let Some(&index) = edge_indices.get(&(from, to)) {
                graph.edges[index].sites += 1;
            } else {
                if graph.edges.len() >= 20000 {
                    graph.truncated = true;
                    return Ok(graph);
                }
                edge_indices.insert((from, to), graph.edges.len());
                graph.edges.push(Edge {
                    from,
                    to,
                    sites: 1,
                    inferred: 0,
                });
            }
        }
    }
    Ok(graph)
}

impl CallGraph {
    /// All helper nodes that can reach each component kind, including cycles.
    pub fn component_reachability(&self) -> Vec<u8> {
        self.reachability(|n| match n.component {
            Some(Component::Activity) => 1,
            Some(Component::Service) => 2,
            Some(Component::Receiver) => 4,
            Some(Component::Provider) => 8,
            None => 0,
        })
    }
    /// Propagates each node's seed bits to every node that can reach it, including cycles.
    pub fn reachability(&self, seed: impl Fn(&Node) -> u8) -> Vec<u8> {
        self.propagate(seed, |edge| (edge.to, edge.from))
    }
    /// Propagates each node's seed bits to every node reachable from it, including cycles.
    pub fn reachable_from(&self, seed: impl Fn(&Node) -> u8) -> Vec<u8> {
        self.propagate(seed, |edge| (edge.from, edge.to))
    }
    fn propagate(
        &self,
        seed: impl Fn(&Node) -> u8,
        step: impl Fn(&Edge) -> (usize, usize),
    ) -> Vec<u8> {
        let mut next = vec![Vec::new(); self.nodes.len()];
        for edge in &self.edges {
            let (source, target) = step(edge);
            next[source].push(target);
        }
        let mut masks: Vec<u8> = self.nodes.iter().map(seed).collect();
        let mut queue: VecDeque<_> = masks
            .iter()
            .enumerate()
            .filter_map(|(i, &m)| (m != 0).then_some(i))
            .collect();
        while let Some(source) = queue.pop_front() {
            for &target in &next[source] {
                let merged = masks[target] | masks[source];
                if merged != masks[target] {
                    masks[target] = merged;
                    queue.push_back(target);
                }
            }
        }
        masks
    }
    /// Signed layout column per node: a callee sits one column right of the method that
    /// reached it and a caller one column left, so arrows read left to right whichever
    /// direction the graph was built in. Each node takes its column from the edge that
    /// discovered it; a node without edges falls back to its depth.
    pub fn columns(&self) -> Vec<i32> {
        let mut columns: Vec<Option<i32>> = vec![None; self.nodes.len()];
        let mut adjacent: Vec<Vec<(usize, i32)>> = vec![Vec::new(); self.nodes.len()];
        for edge in &self.edges {
            adjacent[edge.from].push((edge.to, 1));
            adjacent[edge.to].push((edge.from, -1));
        }
        let mut queue = VecDeque::new();
        if let Some(root) = columns.first_mut() {
            *root = Some(0);
            queue.push_back(0);
        }
        while let Some(current) = queue.pop_front() {
            let column = columns[current].unwrap_or(0);
            for &(next, step) in &adjacent[current] {
                if columns[next].is_none() {
                    columns[next] = Some(column + step);
                    queue.push_back(next);
                }
            }
        }
        columns
            .iter()
            .zip(&self.nodes)
            .map(|(column, node)| column.unwrap_or(node.depth as i32))
            .collect()
    }
    pub fn component_paths(&self) -> Vec<bool> {
        self.component_reachability()
            .into_iter()
            .enumerate()
            .map(|(i, mask)| i == 0 || mask != 0)
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dispatch_hints_follow_interfaces_without_claiming_call_edges() {
        use crate::native_dex::{DexClass, DexMethod, DexSymbols};
        use std::sync::Arc;
        let make = |name: &str, interfaces: Vec<&str>| {
            let descriptor: Arc<str> = format!("L{name};").into();
            DexClass {
                descriptor: descriptor.clone(),
                superclass: None,
                interfaces: interfaces.into_iter().map(Arc::from).collect(),
                access_flags: 1,
                annotations_offset: 0,
                static_values_offset: 0,
                static_values: vec![],
                fields: vec![],
                symbols: Arc::new(DexSymbols::default()),
                methods: vec![DexMethod {
                    declaring_type: descriptor,
                    name: "run".into(),
                    return_type: "V".into(),
                    parameters: vec![],
                    thrown_types: vec![],
                    access_flags: 1,
                    code: None,
                }],
            }
        };
        let mut classes = HashMap::from([
            ("Concrete", make("Concrete", vec!["LChild;", "LParent;"])),
            ("Child", make("Child", vec!["LParent;"])),
            ("Parent", make("Parent", vec!["LChild;"])),
        ]);
        let cancel = AtomicBool::new(false);
        let (targets, truncated) =
            dispatch_targets_with(|name| classes.get(name), "Concrete.run()V", &cancel).unwrap();
        assert_eq!(targets, ["Child.run()V", "Parent.run()V"]);
        assert!(!truncated);
        assert!(
            dispatch_targets_with(
                |name| classes.get(name),
                "Concrete.run()V",
                &AtomicBool::new(true)
            )
            .is_err()
        );
        classes.get_mut("Concrete").unwrap().methods[0].access_flags = 9;
        assert!(
            dispatch_targets_with(|name| classes.get(name), "Concrete.run()V", &cancel)
                .unwrap()
                .0
                .is_empty()
        );
        assert!(
            dispatch_targets_with(|name| classes.get(name), "Concrete.<init>()V", &cancel)
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[test]
    fn ancestry_and_cycles() {
        for (base, expected) in [
            ("android.app.Activity", Component::Activity),
            ("android.app.Service", Component::Service),
            ("android.content.BroadcastReceiver", Component::Receiver),
            ("android.content.ContentProvider", Component::Provider),
        ] {
            assert_eq!(
                component("app.Child".into(), |n| match n {
                    "app.Child" => Some("app.Base".into()),
                    "app.Base" => Some(base.into()),
                    _ => None,
                }),
                Some(expected)
            );
        }
        assert_eq!(component("app.Cycle".into(), |n| Some(n.into())), None);
        assert_eq!(component("ActivityLikeName".into(), |_| None), None);
    }
    #[test]
    fn exposure_map_merges_declarations_and_propagates_to_loaded_superclasses() {
        use crate::manifest_summary::ManifestComponent;
        let declare = |kind: &str, name: &str, target: &str, exported: &str| ManifestComponent {
            kind: kind.into(),
            name: name.into(),
            target: target.into(),
            exported: exported.into(),
            enabled: "true".into(),
            launcher: false,
            filters: vec![],
            details: vec![],
        };
        let components = [
            declare(
                "activity",
                "app.Main",
                "app.Main",
                "true (inferred from intent filters)",
            ),
            declare("activity", "app.Hidden", "app.Hidden", "false (explicit)"),
            declare(
                "activity-alias",
                "app.Door",
                "app.Hidden",
                "true (explicit)",
            ),
            declare(
                "activity",
                "app.Quiet",
                "app.Quiet",
                "false (inferred from intent filters)",
            ),
            declare(
                "service",
                "app.Legacy",
                "app.Legacy",
                "Unresolved (missing exported on target SDK 31+)",
            ),
            declare("activity-alias", "app.Broken", "", "true (explicit)"),
        ];
        // Loaded classes and their superclasses; framework classes are not loaded.
        let hierarchy = HashMap::from([
            ("app.Main", "app.Base"),
            ("app.Hidden", "app.Base"),
            ("app.Quiet", "lib.Compat"),
            ("app.Base", "lib.Compat"),
            ("lib.Compat", "android.app.Activity"),
            ("app.Legacy", "android.app.Service"),
        ]);
        let map = exposure_map(&components, |name| {
            hierarchy.get(name).map(|parent| (*parent).to_owned())
        });
        assert_eq!(map["app.Main"].exported, Some(true));
        assert_eq!(
            map["app.Hidden"].exported,
            Some(true),
            "alias exports the target"
        );
        assert_eq!(
            map["app.Hidden"].details,
            vec![
                "activity: false (explicit)".to_owned(),
                "alias app.Door: true (explicit)".to_owned()
            ]
        );
        assert_eq!(map["app.Quiet"].exported, Some(false));
        assert_eq!(map["app.Legacy"].exported, None);
        assert_eq!(map["app.Base"].exported, Some(true));
        assert_eq!(map["app.Base"].details.len(), 3);
        assert!(
            map["app.Base"]
                .details
                .iter()
                .all(|line| line.ends_with("(via subclass app.Main)")
                    || line.ends_with("(via subclass app.Hidden)"))
        );
        assert_eq!(map["lib.Compat"].exported, Some(true));
        assert_eq!(map["lib.Compat"].details.len(), 4);
        assert!(
            !map.contains_key("android.app.Activity"),
            "framework classes stay out"
        );
        assert!(!map.contains_key("android.app.Service"));
        assert!(
            !map.contains_key(""),
            "aliases without a target are skipped"
        );
        assert!(!map.contains_key("app.Door"), "alias names are not classes");
        assert_eq!(map.len(), 6);
        // Unresolved beats internal once no exported declaration is present.
        let mixed = [
            declare("provider", "app.P", "app.P", "false (explicit)"),
            declare("provider", "app.P", "app.P", "Unresolved (target SDK)"),
        ];
        assert_eq!(exposure_map(&mixed, |_| None)["app.P"].exported, None);
    }
    #[test]
    fn full_paths_are_marked_without_marking_unrelated_branches() {
        let graph = CallGraph {
            nodes: (0..5)
                .map(|i| Node {
                    method: format!("C.m{i}()V"),
                    depth: i,
                    component: (i == 3).then_some(Component::Service),
                    available: true,
                })
                .collect(),
            edges: vec![
                Edge {
                    from: 0,
                    to: 1,
                    sites: 1,
                    inferred: 0,
                },
                Edge {
                    from: 1,
                    to: 2,
                    sites: 1,
                    inferred: 0,
                },
                Edge {
                    from: 2,
                    to: 3,
                    sites: 1,
                    inferred: 0,
                },
                Edge {
                    from: 1,
                    to: 4,
                    sites: 1,
                    inferred: 0,
                },
                Edge {
                    from: 2,
                    to: 1,
                    sites: 1,
                    inferred: 0,
                },
            ],
            truncated: false,
            dispatch_targets: vec![],
            dispatch_targets_truncated: false,
            direction: Direction::Callees,
        };
        assert_eq!(graph.component_reachability(), vec![2, 2, 2, 2, 0]);
        assert_eq!(graph.nodes.len(), 5); // unrelated node remains in the graph
        assert_eq!(graph.component_paths(), vec![true, true, true, true, false]);
        assert_eq!(owner("a.b.C.m(Ljava/lang/String;)V"), Some("a.b.C"));
    }
    #[test]
    fn columns_and_forward_propagation_follow_discovery_edges() {
        let node = |i: usize, depth: usize, component: Option<Component>| Node {
            method: format!("C.m{i}()V"),
            depth,
            component,
            available: true,
        };
        let edge = |from, to| Edge {
            from,
            to,
            sites: 1,
            inferred: 0,
        };
        // Discovery order of a bidirectional build: node 1 calls the root, the root calls
        // node 2, node 3 also calls node 2, node 4 calls node 1, node 5 was cut off before
        // its edge was recorded.
        let graph = CallGraph {
            nodes: vec![
                node(0, 0, None),
                node(1, 1, Some(Component::Activity)),
                node(2, 1, None),
                node(3, 2, None),
                node(4, 2, Some(Component::Service)),
                node(5, 3, None),
            ],
            edges: vec![edge(1, 0), edge(0, 2), edge(3, 2), edge(4, 1), edge(2, 0)],
            direction: Direction::Both,
            ..Default::default()
        };
        assert_eq!(graph.columns(), vec![0, -1, 1, 0, -2, 3]);
        // Edge order never decides the layout: a chain recorded callee-first still
        // places each caller one column further left.
        let chain = CallGraph {
            nodes: vec![node(0, 0, None), node(1, 1, None), node(2, 2, None)],
            edges: vec![edge(2, 1), edge(1, 0)],
            direction: Direction::Callers,
            ..Default::default()
        };
        assert_eq!(chain.columns(), vec![0, -1, -2]);
        // Forward propagation marks what the components lead to; the cycle 0 → 2 → 0 is
        // covered and the unrelated caller 3 stays clear.
        let seed = |n: &Node| match n.component {
            Some(Component::Activity) => 1,
            Some(Component::Service) => 2,
            _ => 0,
        };
        assert_eq!(graph.reachable_from(seed), vec![3, 3, 3, 0, 2, 0]);
        assert_eq!(graph.reachability(seed), vec![0, 1, 0, 0, 3, 0]);
        assert_eq!(Direction::parse("callers"), Some(Direction::Callers));
        assert_eq!(Direction::parse("sideways"), None);
        for direction in Direction::ALL {
            assert_eq!(Direction::parse(direction.as_str()), Some(direction));
        }
        assert_eq!(Direction::default(), Direction::Callees);
    }
}
