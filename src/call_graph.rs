//! Bounded static call graph. Component classification follows DEX ancestry.
use crate::{native_engine::NativeDexEngine, native_xrefs};
use anyhow::{Context, Result, ensure};
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
    pub sites: usize,
}
#[derive(Debug, Default)]
pub struct CallGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub truncated: bool,
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
fn parent(engine: &NativeDexEngine, name: &str) -> Option<String> {
    engine
        .class(name)?
        .superclass
        .as_deref()
        .map(crate::native_engine::disassembly::type_name)
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
    if let Some((_, id)) = body(engine, target) {
        return id;
    }
    let Some(original) = owner(target) else {
        return target.into();
    };
    let member = &target[original.len() + 1..];
    if member.starts_with('<') {
        return target.into();
    }
    let mut name = original.to_string();
    let mut seen = HashSet::new();
    while seen.insert(name.clone()) {
        match parent(engine, &name) {
            Some(next) => name = next,
            None => break,
        }
    }
    format!("{name}.{member}")
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
    ensure!(
        ["callers", "callees", "both"].contains(&direction),
        "Invalid direction"
    );
    let root_owner = owner(root).context("Select a method")?;
    let mut incoming: HashMap<String, Vec<native_xrefs::CallSite>> = HashMap::new();
    if direction != "callees" {
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
    let mut graph = CallGraph::default();
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
        let outgoing = if direction != "callers" {
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
                graph.edges.push(Edge { from, to, sites: 1 });
            }
        }
    }
    Ok(graph)
}

impl CallGraph {
    /// All helper nodes that can reach each component kind, including cycles.
    pub fn component_reachability(&self) -> Vec<u8> {
        let mut incoming = vec![Vec::new(); self.nodes.len()];
        for edge in &self.edges {
            incoming[edge.to].push(edge.from);
        }
        let mut masks: Vec<u8> = self
            .nodes
            .iter()
            .map(|n| match n.component {
                Some(Component::Activity) => 1,
                Some(Component::Service) => 2,
                Some(Component::Receiver) => 4,
                Some(Component::Provider) => 8,
                None => 0,
            })
            .collect();
        let mut queue: VecDeque<_> = masks
            .iter()
            .enumerate()
            .filter_map(|(i, &m)| (m != 0).then_some(i))
            .collect();
        while let Some(to) = queue.pop_front() {
            for &from in &incoming[to] {
                let merged = masks[from] | masks[to];
                if merged != masks[from] {
                    masks[from] = merged;
                    queue.push_back(from);
                }
            }
        }
        masks
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
                },
                Edge {
                    from: 1,
                    to: 2,
                    sites: 1,
                },
                Edge {
                    from: 2,
                    to: 3,
                    sites: 1,
                },
                Edge {
                    from: 1,
                    to: 4,
                    sites: 1,
                },
                Edge {
                    from: 2,
                    to: 1,
                    sites: 1,
                },
            ],
            truncated: false,
        };
        assert_eq!(graph.component_reachability(), vec![2, 2, 2, 2, 0]);
        assert_eq!(graph.nodes.len(), 5); // unrelated node remains in the graph
        assert_eq!(graph.component_paths(), vec![true, true, true, true, false]);
        assert_eq!(owner("a.b.C.m(Ljava/lang/String;)V"), Some("a.b.C"));
    }
}
