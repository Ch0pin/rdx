use std::collections::{HashMap, HashSet, VecDeque};

use crate::graph::{ConditionTree, Graph, LoopKind, NodeId};
use crate::ir::opcode::BlockKind;

pub fn identify_structures(graph: &mut Graph) {
    identify_structures_impl(graph, false);
}

pub(crate) fn identify_structures_debug(graph: &mut Graph) {
    identify_structures_impl(graph, true);
}

fn identify_structures_impl(graph: &mut Graph, debug: bool) {
    graph.compute_rpo();
    let mut catch_dominators = graph.immediate_dominators();
    structure_switches(graph, &catch_dominators);
    mark_loops(graph);
    if debug {
        graph.debug_loop_metadata("after-mark-loops");
    }
    merge_short_circuits(graph, &mut catch_dominators);

    let unresolved_if = structure_ifs(graph, &catch_dominators);
    finish_loop_metadata(graph);
    if debug {
        graph.debug_loop_metadata("after-finish-loop-metadata");
    }
    wrap_loops(graph, &mut catch_dominators);
    graph.compute_rpo();
    refresh_loop_follows(graph);
    if debug {
        graph.debug_loop_metadata("after-wrap-loops");
    }

    for node in unresolved_if {
        if node >= graph.nodes.len() || !graph.nodes[node].active {
            continue;
        }
        graph.nodes[node].if_follow = [
            graph.nodes[node].loop_follow,
            graph.nodes[node].switch_follow,
        ]
        .into_iter()
        .flatten()
        .min_by_key(|follow| graph.nodes[*follow].rpo);
    }

    structure_catches(graph, &catch_dominators);
}

fn mark_loops(graph: &mut Graph) {
    let mut derived = DerivedGraph::from_graph(graph);
    let mut marks = Vec::new();

    loop {
        let partition = derived.intervals();
        collect_interval_loops(graph, &derived, &partition, &mut marks);
        if partition.nodes.len() == 1 || partition.nodes.len() == derived.nodes.len() {
            break;
        }
        derived = derived.collapse(partition);
    }

    let invalid_marks = marks
        .iter()
        .any(|mark| !loop_mark_has_backedge(graph, mark));
    let overlapping_marks = marks.iter().enumerate().any(|(index, mark)| {
        marks[index + 1..]
            .iter()
            .any(|other| mark.nodes.iter().any(|node| other.nodes.contains(node)))
    });
    if invalid_marks && overlapping_marks {
        marks = natural_loop_marks(graph);
    }

    for mark in marks {
        graph.nodes[mark.head].loop_kind = LoopKind::Endless;
        graph.nodes[mark.head].latch = Some(mark.latch);
        graph.nodes[mark.head].loop_nodes = mark.nodes;
    }
}

fn loop_mark_has_backedge(graph: &Graph, mark: &LoopMark) -> bool {
    graph.nodes[mark.latch]
        .successors
        .iter()
        .any(|target| *target == mark.head || mark.nodes.contains(target))
}

fn natural_loop_marks(graph: &Graph) -> Vec<LoopMark> {
    let dominators = graph.immediate_dominators();
    let mut grouped = HashMap::<NodeId, (HashSet<NodeId>, NodeId)>::new();

    for source in graph.active_nodes() {
        for &head in &graph.nodes[source].successors {
            if !dominates(graph, &dominators, head, source) {
                continue;
            }
            let entry = grouped
                .entry(head)
                .or_insert_with(|| (HashSet::from([head]), source));
            entry.0.insert(source);
            let mut pending = vec![source];
            while let Some(node) = pending.pop() {
                if node == head {
                    continue;
                }
                for &predecessor in &graph.nodes[node].predecessors {
                    if entry.0.insert(predecessor) {
                        pending.push(predecessor);
                    }
                }
            }
            entry.1 = source;
        }
    }

    let mut marks: Vec<_> = grouped
        .into_iter()
        .map(|(head, (nodes, latch))| {
            let mut nodes: Vec<_> = nodes.into_iter().collect();
            nodes.sort_by_key(|node| graph.nodes[*node].rpo);
            LoopMark { head, latch, nodes }
        })
        .collect();
    marks.sort_by_key(|mark| graph.nodes[mark.head].rpo);
    marks
}

fn dominates(
    graph: &Graph,
    dominators: &[Option<NodeId>],
    target: NodeId,
    mut node: NodeId,
) -> bool {
    loop {
        if node == target {
            return true;
        }
        let Some(parent) = dominators.get(node).copied().flatten() else {
            return false;
        };
        node = parent;
        if node == graph.entry && node != target {
            return false;
        }
    }
}

#[derive(Clone)]
struct DerivedNode {
    head: NodeId,
    end: NodeId,
    members: Vec<NodeId>,
    rpo: usize,
}

struct IntervalPartition {
    nodes: Vec<DerivedNode>,
    headers: Vec<usize>,
    membership: Vec<usize>,
    successor_heads: Vec<Vec<usize>>,
}

struct DerivedGraph {
    nodes: Vec<DerivedNode>,
    successors: Vec<Vec<usize>>,
    normal_successors: Vec<Vec<usize>>,
    predecessors: Vec<Vec<usize>>,
    entry: usize,
    rpo: Vec<usize>,
}

struct LoopMark {
    head: NodeId,
    latch: NodeId,
    nodes: Vec<NodeId>,
}

impl DerivedGraph {
    fn from_graph(graph: &Graph) -> Self {
        let mut base_nodes: Vec<_> = graph.active_nodes().collect();
        base_nodes.sort_by_key(|node| graph.nodes[*node].insertion_order);
        let mut index = vec![None; graph.nodes.len()];
        for (position, node) in base_nodes.iter().copied().enumerate() {
            index[node] = Some(position);
        }
        let nodes = base_nodes
            .iter()
            .copied()
            .map(|node| DerivedNode {
                head: node,
                end: node,
                members: vec![node],
                rpo: graph.nodes[node].rpo,
            })
            .collect();
        let successors = base_nodes
            .iter()
            .map(|node| {
                graph
                    .all_successors(*node)
                    .filter_map(|successor| index[successor])
                    .collect()
            })
            .collect();
        let normal_successors = base_nodes
            .iter()
            .map(|node| {
                graph.nodes[*node]
                    .successors
                    .iter()
                    .filter_map(|successor| index[*successor])
                    .collect()
            })
            .collect();
        let predecessors = base_nodes
            .iter()
            .map(|node| {
                graph
                    .all_predecessors(*node)
                    .filter_map(|predecessor| index[predecessor])
                    .collect()
            })
            .collect();
        let mut rpo: Vec<_> = (0..base_nodes.len()).collect();
        rpo.sort_by_key(|node| graph.nodes[base_nodes[*node]].rpo);
        Self {
            nodes,
            successors,
            normal_successors,
            predecessors,
            entry: index[graph.entry].expect("active graph entry"),
            rpo,
        }
    }

    fn intervals(&self) -> IntervalPartition {
        let mut heads = VecDeque::from([self.entry]);
        let mut processed = vec![false; self.nodes.len()];
        let mut membership = vec![usize::MAX; self.nodes.len()];
        let mut nodes = Vec::new();
        let mut headers = Vec::new();
        let mut successor_heads = Vec::new();

        while let Some(head) = heads.pop_front() {
            if processed[head] {
                continue;
            }
            processed[head] = true;
            let interval = nodes.len();
            membership[head] = interval;
            let mut content = vec![head];
            let mut changed = true;
            while changed {
                changed = false;
                for &node in self.rpo.iter().skip(1) {
                    if membership[node] == interval {
                        continue;
                    }
                    if self.predecessors[node]
                        .iter()
                        .all(|predecessor| membership[*predecessor] == interval)
                    {
                        membership[node] = interval;
                        content.push(node);
                        changed = true;
                    }
                }
            }

            let mut outgoing_heads = Vec::new();
            for node in 0..self.nodes.len() {
                if membership[node] == interval || heads.contains(&node) {
                    continue;
                }
                if self.predecessors[node]
                    .iter()
                    .any(|predecessor| membership[*predecessor] == interval)
                {
                    outgoing_heads.push(node);
                    heads.push_back(node);
                }
            }

            let mut end = head;
            let mut common_exit_target = None;
            let mut exits_share_target = true;
            let mut max_exit_end = self.nodes[head].end;
            let mut max_exit = head;
            let mut exit_nodes = Vec::new();
            for &node in &content {
                let mut exits = self.normal_successors[node]
                    .iter()
                    .copied()
                    .filter(|successor| membership[*successor] != interval);
                let Some(first_target) = exits.next() else {
                    continue;
                };
                exit_nodes.push(node);
                end = node;
                if self.nodes[node].end > max_exit_end {
                    max_exit_end = self.nodes[node].end;
                    max_exit = node;
                }
                for target in std::iter::once(first_target).chain(exits) {
                    if common_exit_target.is_some_and(|common| common != target) {
                        exits_share_target = false;
                    } else if common_exit_target.is_none() {
                        common_exit_target = Some(target);
                    }
                }
            }
            if exits_share_target && common_exit_target.is_some() {
                let mut deeper_exit = None;
                for &candidate in &exit_nodes {
                    if exit_nodes.iter().any(|source| {
                        *source != candidate
                            && self.normal_successors[*source].contains(&candidate)
                    }) {
                        if deeper_exit.is_some() {
                            deeper_exit = None;
                            break;
                        }
                        deeper_exit = Some(candidate);
                    }
                }
                end = deeper_exit.unwrap_or(max_exit);
            }
            let mut members = Vec::new();
            for node in content {
                members.extend_from_slice(&self.nodes[node].members);
            }
            members.sort_unstable();
            members.dedup();
            nodes.push(DerivedNode {
                head: self.nodes[head].head,
                end: self.nodes[end].end,
                members,
                rpo: 0,
            });
            headers.push(head);
            successor_heads.push(outgoing_heads);
        }

        IntervalPartition {
            nodes,
            headers,
            membership,
            successor_heads,
        }
    }

    fn collapse(&self, partition: IntervalPartition) -> Self {
        let node_count = partition.nodes.len();
        let mut successors = vec![Vec::new(); node_count];
        let mut predecessors = vec![Vec::new(); node_count];
        for (source, heads) in partition.successor_heads.into_iter().enumerate() {
            for head in heads {
                let target = partition.membership[head];
                if !successors[source].contains(&target) {
                    successors[source].push(target);
                }
                if !predecessors[target].contains(&source) {
                    predecessors[target].push(source);
                }
            }
        }
        let entry = partition.membership[self.entry];
        let mut collapsed = Self {
            nodes: partition.nodes,
            normal_successors: successors.clone(),
            successors,
            predecessors,
            entry,
            rpo: Vec::new(),
        };
        collapsed.compute_rpo();
        collapsed
    }

    fn compute_rpo(&mut self) {
        let mut visited = vec![false; self.nodes.len()];
        let mut post_order = Vec::with_capacity(self.nodes.len());
        visited[self.entry] = true;
        let mut stack = vec![(self.entry, 0_usize)];
        while let Some((node, next)) = stack.pop() {
            if next < self.successors[node].len() {
                stack.push((node, next + 1));
                let successor = self.successors[node][next];
                if !visited[successor] {
                    visited[successor] = true;
                    stack.push((successor, 0));
                }
            } else {
                post_order.push(node);
            }
        }
        post_order.reverse();
        for (rpo, node) in post_order.iter().copied().enumerate() {
            self.nodes[node].rpo = rpo;
        }
        self.rpo = post_order;
    }
}

fn collect_interval_loops(
    graph: &Graph,
    derived: &DerivedGraph,
    partition: &IntervalPartition,
    marks: &mut Vec<LoopMark>,
) {
    let mut heads = partition.headers.clone();
    heads.sort_by_key(|head| derived.nodes[*head].rpo);
    for head in heads {
        let interval = partition.membership[head];
        let mut latch = None;
        let mut loop_nodes = Vec::new();
        let mut seen = HashSet::new();
        for &predecessor in &derived.predecessors[head] {
            if partition.membership[predecessor] != interval {
                continue;
            }
            let base_head = derived.nodes[head].head;
            let base_latch = derived.nodes[predecessor].end;
            let mut candidate = vec![base_head];
            let mut candidate_seen = HashSet::from([base_head]);
            collect_loop_nodes(
                graph,
                base_latch,
                base_head,
                base_latch,
                &partition.nodes[interval].members,
                &mut candidate_seen,
                &mut candidate,
            );
            for node in candidate {
                if seen.insert(node) {
                    loop_nodes.push(node);
                }
            }
            latch = Some(base_latch);
        }
        if let Some(latch) = latch {
            marks.push(LoopMark {
                head: derived.nodes[head].head,
                latch,
                nodes: loop_nodes,
            });
        }
    }
}

fn collect_loop_nodes(
    graph: &Graph,
    node: NodeId,
    head: NodeId,
    latch: NodeId,
    interval: &[NodeId],
    seen: &mut HashSet<NodeId>,
    nodes: &mut Vec<NodeId>,
) {
    if !seen.insert(node) {
        return;
    }
    nodes.push(node);
    for predecessor in graph.all_predecessors(node) {
        if graph.nodes[head].rpo < graph.nodes[predecessor].rpo
            && graph.nodes[predecessor].rpo <= graph.nodes[latch].rpo
            && interval.binary_search(&predecessor).is_ok()
        {
            collect_loop_nodes(graph, predecessor, head, latch, interval, seen, nodes);
        }
    }
}

fn classify_loop(graph: &Graph, head: NodeId, latch: NodeId, nodes: &HashSet<NodeId>) -> LoopKind {
    if is_condition_kind(graph.nodes[latch].kind) {
        if is_condition_kind(graph.nodes[head].kind) {
            if branch_inside(graph, head, true, nodes) && branch_inside(graph, head, false, nodes) {
                LoopKind::PostTest
            } else {
                LoopKind::PreTest
            }
        } else {
            LoopKind::PostTest
        }
    } else if is_condition_kind(graph.nodes[head].kind) {
        if branch_inside(graph, head, true, nodes) && branch_inside(graph, head, false, nodes) {
            LoopKind::Endless
        } else {
            LoopKind::PreTest
        }
    } else {
        LoopKind::Endless
    }
}

fn find_loop_follow(
    graph: &Graph,
    head: NodeId,
    latch: NodeId,
    nodes: &HashSet<NodeId>,
) -> Option<NodeId> {
    match graph.nodes[head].loop_kind {
        LoopKind::PreTest => opposite_branch(graph, head, nodes),
        LoopKind::PostTest => opposite_branch(graph, latch, nodes),
        LoopKind::Endless => nodes
            .iter()
            .filter(|node| is_condition_kind(graph.nodes[**node].kind))
            .flat_map(|node| {
                [
                    graph.nodes[*node].branch_true,
                    graph.nodes[*node].branch_false,
                ]
            })
            .flatten()
            .filter(|target| !nodes.contains(target))
            .min_by_key(|target| graph.nodes[*target].rpo),
        LoopKind::None => None,
    }
}

fn is_condition_kind(kind: BlockKind) -> bool {
    matches!(kind, BlockKind::Condition | BlockKind::ShortCircuit)
}

fn branch_inside(graph: &Graph, node: NodeId, true_branch: bool, nodes: &HashSet<NodeId>) -> bool {
    let target = if true_branch {
        graph.nodes[node].branch_true
    } else {
        graph.nodes[node].branch_false
    };
    target.is_some_and(|target| nodes.contains(&target))
}

fn opposite_branch(graph: &Graph, node: NodeId, nodes: &HashSet<NodeId>) -> Option<NodeId> {
    if graph.nodes[node]
        .branch_true
        .is_some_and(|target| nodes.contains(&target))
    {
        graph.nodes[node].branch_false
    } else {
        graph.nodes[node].branch_true
    }
}

fn structure_ifs(graph: &mut Graph, dominators: &[Option<NodeId>]) -> HashSet<NodeId> {
    let mut unresolved = HashSet::<NodeId>::new();
    let post_order: Vec<_> = graph.rpo.iter().rev().copied().collect();
    for node in post_order {
        if !matches!(
            graph.nodes[node].kind,
            BlockKind::Condition | BlockKind::ShortCircuit
        ) {
            continue;
        }
        let follow = graph
            .active_nodes()
            .filter(|candidate| {
                dominators[*candidate] == Some(node)
                    && graph.nodes[*candidate].predecessors.len() > 1
            })
            .max_by_key(|candidate| graph.nodes[*candidate].rpo);
        if let Some(follow) = follow {
            graph.nodes[node].if_follow = Some(follow);
            let resolved: Vec<_> = unresolved
                .iter()
                .copied()
                .filter(|candidate| {
                    graph.nodes[node].rpo < graph.nodes[*candidate].rpo
                        && graph.nodes[*candidate].rpo < graph.nodes[follow].rpo
                })
                .collect();
            for candidate in resolved {
                graph.nodes[candidate].if_follow = Some(follow);
                unresolved.remove(&candidate);
            }
        } else {
            unresolved.insert(node);
        }
    }
    unresolved
}

fn structure_switches(graph: &mut Graph, dominators: &[Option<NodeId>]) {
    let mut unresolved = HashSet::<NodeId>::new();
    let post_order: Vec<_> = graph.rpo.iter().rev().copied().collect();
    for node in post_order {
        if graph.nodes[node].kind != BlockKind::Switch {
            continue;
        }
        let mut common = node;
        let successors = graph.nodes[node].successors.clone();
        for successor in successors {
            if dominators[successor] != Some(node) {
                common = common_dominator(graph, dominators, common, successor);
            }
        }
        let follow = graph
            .active_nodes()
            .filter(|candidate| {
                dominators[*candidate] == Some(common)
                    && graph.all_predecessors(*candidate).count() > 1
            })
            .max_by_key(|candidate| graph.nodes[*candidate].rpo);
        if let Some(follow) = follow {
            graph.nodes[node].switch_follow = Some(follow);
            for candidate in unresolved.drain() {
                graph.nodes[candidate].switch_follow = Some(follow);
            }
        } else {
            unresolved.insert(node);
        }
    }
}

fn merge_short_circuits(graph: &mut Graph, catch_dominators: &mut Vec<Option<NodeId>>) {
    loop {
        graph.compute_rpo();
        let post_order: Vec<_> = graph.rpo.iter().rev().copied().collect();
        let mut merged = false;
        for node in post_order {
            if !graph.nodes[node].active
                || !matches!(
                    graph.nodes[node].kind,
                    BlockKind::Condition | BlockKind::ShortCircuit
                )
            {
                continue;
            }
            let (Some(then_node), Some(else_node)) = (
                graph.nodes[node].branch_true,
                graph.nodes[node].branch_false,
            ) else {
                continue;
            };
            if node == then_node || node == else_node {
                continue;
            }
            let candidate = if matches!(
                graph.nodes[then_node].kind,
                BlockKind::Condition | BlockKind::ShortCircuit
            ) && normal_predecessor_count(graph, then_node) == 1
            {
                let nested = &graph.nodes[then_node];
                if nested.branch_false == Some(else_node) {
                    Some((then_node, true, false, nested.branch_true, Some(else_node)))
                } else if nested.branch_true == Some(else_node) {
                    Some((then_node, false, true, Some(else_node), nested.branch_false))
                } else {
                    None
                }
            } else if matches!(
                graph.nodes[else_node].kind,
                BlockKind::Condition | BlockKind::ShortCircuit
            ) && normal_predecessor_count(graph, else_node) == 1
            {
                let nested = &graph.nodes[else_node];
                if nested.branch_false == Some(then_node) {
                    Some((else_node, true, true, nested.branch_true, Some(then_node)))
                } else if nested.branch_true == Some(then_node) {
                    Some((
                        else_node,
                        false,
                        false,
                        Some(then_node),
                        nested.branch_false,
                    ))
                } else {
                    None
                }
            } else {
                None
            };
            let Some((second, is_and, is_not, new_true, new_false)) = candidate else {
                continue;
            };
            if graph.nodes[second].branch_true == Some(node)
                || graph.nodes[second].branch_false == Some(node)
                || second == new_true.unwrap_or(usize::MAX)
                || second == new_false.unwrap_or(usize::MAX)
            {
                continue;
            }
            merge_conditions(
                graph,
                catch_dominators,
                node,
                second,
                (is_and, is_not, new_true, new_false),
            );
            merged = true;
        }
        if !merged {
            break;
        }
    }
}

fn merge_conditions(
    graph: &mut Graph,
    catch_dominators: &mut Vec<Option<NodeId>>,
    first: NodeId,
    second: NodeId,
    condition: (bool, bool, Option<NodeId>, Option<NodeId>),
) -> NodeId {
    let (is_and, is_not, branch_true, branch_false) = condition;
    let mut predecessors = Vec::new();
    for predecessor in graph.nodes[first]
        .predecessors
        .iter()
        .chain(&graph.nodes[second].predecessors)
        .copied()
        .filter(|node| *node != first && *node != second && !graph.nodes[*node].in_catch)
    {
        if !predecessors.contains(&predecessor) {
            predecessors.push(predecessor);
        }
    }
    let mut successors = Vec::new();
    for successor in graph.nodes[first]
        .successors
        .iter()
        .chain(&graph.nodes[second].successors)
        .copied()
        .filter(|node| *node != first && *node != second)
    {
        if !successors.contains(&successor) {
            successors.push(successor);
        }
    }
    let entry = graph.entry == first || graph.entry == second;
    let left = graph.nodes[first]
        .condition
        .take()
        .unwrap_or(ConditionTree::Leaf(first));
    let right = graph.nodes[second]
        .condition
        .take()
        .unwrap_or(ConditionTree::Leaf(second));
    let merged = graph.append_wrapper(BlockKind::ShortCircuit, first);
    catch_dominators.push(catch_dominators[first]);
    for dominator in &mut *catch_dominators {
        if *dominator == Some(first) || *dominator == Some(second) {
            *dominator = Some(merged);
        }
    }
    catch_dominators[first] = None;
    catch_dominators[second] = None;
    graph.nodes[merged].condition = Some(ConditionTree::Compound {
        left: Box::new(left),
        right: Box::new(right),
        is_and,
        is_not,
    });
    graph.nodes[merged].branch_true = branch_true;
    graph.nodes[merged].branch_false = branch_false;
    graph.replace_structural_references(first, merged);
    graph.replace_structural_references(second, merged);
    graph.remove_node(first);
    graph.remove_node(second);
    for predecessor in predecessors {
        graph.add_edge(predecessor, merged);
    }
    for successor in successors {
        graph.add_edge(merged, successor);
    }
    if entry {
        graph.entry = merged;
    }
    merged
}

fn normal_predecessor_count(graph: &Graph, node: NodeId) -> usize {
    graph.nodes[node]
        .predecessors
        .iter()
        .filter(|predecessor| !graph.nodes[**predecessor].in_catch)
        .count()
}

fn finish_loop_metadata(graph: &mut Graph) {
    let loop_heads: Vec<_> = graph
        .active_nodes()
        .filter(|node| graph.nodes[*node].loop_kind != LoopKind::None)
        .collect();
    for &head in &loop_heads {
        let Some(latch) = graph.nodes[head].latch else {
            continue;
        };
        let nodes: HashSet<_> = graph.nodes[head].loop_nodes.iter().copied().collect();
        graph.nodes[head].loop_kind = classify_loop(graph, head, latch, &nodes);
        let follow = find_loop_follow(graph, head, latch, &nodes);
        graph.nodes[head].loop_follow = follow;
        for node in graph.nodes[head].loop_nodes.clone() {
            graph.nodes[node].loop_follow = follow;
        }
    }
}

fn refresh_loop_follows(graph: &mut Graph) {
    let loop_heads: Vec<_> = graph
        .rpo
        .iter()
        .copied()
        .filter(|node| graph.nodes[*node].kind == BlockKind::Loop)
        .collect();
    for head in loop_heads {
        let Some(latch) = graph.nodes[head].latch else {
            continue;
        };
        let nodes: HashSet<_> = graph.nodes[head].loop_nodes.iter().copied().collect();
        let follow = find_loop_follow(graph, head, latch, &nodes);
        graph.nodes[head].loop_follow = follow;
        for node in graph.nodes[head].loop_nodes.clone() {
            graph.nodes[node].loop_follow = follow;
        }
    }
}

fn wrap_loops(graph: &mut Graph, dominators: &mut [Option<NodeId>]) {
    let loop_heads: Vec<_> = graph
        .rpo
        .iter()
        .copied()
        .filter(|node| graph.nodes[*node].loop_kind != LoopKind::None)
        .collect();
    for &head in &loop_heads {
        if !graph.nodes[head].active {
            continue;
        }
        let predecessors: Vec<_> = graph.nodes[head]
            .predecessors
            .iter()
            .copied()
            .filter(|predecessor| !graph.nodes[*predecessor].in_catch)
            .collect();
        let successors = graph.nodes[head].successors.clone();
        let catch_successors = graph.nodes[head].catch_successors.clone();
        let entry = graph.entry == head;
        let nested_same_follow = graph.nodes[head].loop_follow.is_some_and(|follow| {
            loop_heads.iter().copied().any(|candidate| {
                candidate != head
                    && graph.nodes[candidate].loop_follow == Some(follow)
                    && graph.nodes[candidate].loop_nodes.contains(&head)
            })
        });
        let wrapper = graph.append_wrapper(BlockKind::Loop, head);
        for dominator in &mut *dominators {
            if *dominator == Some(head) {
                *dominator = Some(wrapper);
            }
        }
        graph.replace_structural_references(head, wrapper);
        graph.remove_node(head);
        if nested_same_follow {
            // Keep nested raw traversal aligned with the wrapper graph when
            // both loops intentionally share the same continuation.
            for source in 0..graph.nodes.len() {
                if graph.nodes[source].active || source == head {
                    continue;
                }
                for successor in &mut graph.nodes[source].successors {
                    if *successor == head {
                        *successor = wrapper;
                    }
                }
                for successor in &mut graph.nodes[source].catch_successors {
                    if *successor == head {
                        *successor = wrapper;
                    }
                }
                if graph.nodes[source].branch_true == Some(head) {
                    graph.nodes[source].branch_true = Some(wrapper);
                }
                if graph.nodes[source].branch_false == Some(head) {
                    graph.nodes[source].branch_false = Some(wrapper);
                }
            }
        }
        // The structured writer still visits the inactive wrapped node as the
        // loop body. Preserve its outgoing traversal edges after disconnecting
        // it from the active CFG.
        graph.nodes[head].successors = successors.clone();
        graph.nodes[head].catch_successors = catch_successors;
        for predecessor in predecessors {
            if predecessor != head {
                graph.add_edge(predecessor, wrapper);
            }
        }
        for successor in successors {
            if successor != head {
                graph.add_edge(wrapper, successor);
            }
        }
        if entry {
            graph.entry = wrapper;
        }
    }
}

fn structure_catches(graph: &mut Graph, dominators: &[Option<NodeId>]) {
    let structural_node_count = graph.nodes.len();
    let mut catch_blocks: Vec<_> = graph
        .active_nodes()
        .filter(|node| graph.nodes[*node].catch_target && !graph.nodes[*node].catch_source)
        .collect();
    catch_blocks.sort_by_key(|node| graph.nodes[*node].insertion_order);
    let mut tries = HashMap::<NodeId, NodeId>::new();
    let mut try_order = Vec::new();
    for catch_block in catch_blocks {
        let Some(try_start) = dominators.get(catch_block).copied().flatten() else {
            continue;
        };
        let catch = graph.append_wrapper(BlockKind::Catch, catch_block);
        graph.nodes[catch].active = false;
        graph.nodes[catch].catch_types = graph.nodes[catch_block].catch_types.clone();
        graph.nodes[catch].catch_order = graph.nodes[catch_block].catch_order;
        let try_node = if let Some(existing) = tries.get(&try_start) {
            *existing
        } else {
            let wrapper = graph.append_wrapper(BlockKind::Try, try_start);
            graph.nodes[wrapper].try_follow = match graph.nodes[try_start].kind {
                BlockKind::Statement => graph.nodes[try_start].successors.first().copied(),
                // DAD's LoopBlock inherits CondBlock, so a try rooted at a
                // structured loop stops at its loop follow just like a normal
                // condition. Keeping that follow outside the wrapper preserves
                // the DEX normal continuation after the catch region.
                BlockKind::Loop | BlockKind::Condition | BlockKind::ShortCircuit => graph.nodes[try_start]
                    .loop_follow
                    .or(graph.nodes[try_start].if_follow),
                BlockKind::Switch => graph.nodes[try_start].switch_follow,
                _ => None,
            };
            let predecessors: Vec<_> = graph.all_predecessors(try_start).collect();
            let entry = graph.entry == try_start;
            for predecessor in predecessors {
                if graph.nodes[predecessor].successors.contains(&try_start) {
                    graph.remove_edge(predecessor, try_start);
                }
                graph.add_edge(predecessor, wrapper);
            }
            graph.add_edge(wrapper, try_start);
            graph.nodes[wrapper].wrapped = Some(try_start);
            if entry {
                graph.entry = wrapper;
            }
            tries.insert(try_start, wrapper);
            try_order.push(try_start);
            wrapper
        };
        graph.nodes[try_node].catches.push(catch);
    }

    for try_start in try_order {
        let wrapper = tries[&try_start];
        graph.replace_structural_references_before(try_start, wrapper, structural_node_count);
        // Resolve adjacent, independent statement tries. Shared/nested
        // handlers and values escaping the try need wider region analysis.
        if let Some(follow) = graph.nodes[wrapper].try_follow
            && let Some(next_try) = tries.get(&follow)
            && is_independent_statement_try(graph, try_start)
        {
            graph.nodes[wrapper].try_follow = Some(*next_try);
        }
    }
}

fn is_independent_statement_try(graph: &Graph, start: NodeId) -> bool {
    let block = &graph.nodes[start];
    block.kind == BlockKind::Statement
        && block.successors.len() == 1
        && block
            .statements
            .iter()
            .all(|statement| statement.defined_variable().is_none())
        && !block.catch_successors.is_empty()
        && block.catch_successors.iter().all(|handler| {
            let handler = &graph.nodes[*handler];
            handler.kind == BlockKind::Return
                && handler.catch_successors.is_empty()
                && handler.catch_predecessors == [start]
        })
}

fn common_dominator(
    graph: &Graph,
    dominators: &[Option<NodeId>],
    mut left: NodeId,
    mut right: NodeId,
) -> NodeId {
    while left != right {
        while graph.nodes[left].rpo > graph.nodes[right].rpo {
            left = dominators[left].unwrap_or(graph.entry);
        }
        while graph.nodes[right].rpo > graph.nodes[left].rpo {
            right = dominators[right].unwrap_or(graph.entry);
        }
    }
    left
}
