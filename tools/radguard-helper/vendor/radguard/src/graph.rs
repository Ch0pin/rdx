use std::collections::{HashSet, VecDeque};

use crate::ir::opcode::{BlockKind, LoweredMethod};
use crate::ir::{Statement, VariableId, VariableTable};

pub type NodeId = usize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopKind {
    #[default]
    None,
    PreTest,
    PostTest,
    Endless,
}

#[derive(Clone, Debug)]
pub enum ConditionTree {
    Leaf(NodeId),
    Compound {
        left: Box<ConditionTree>,
        right: Box<ConditionTree>,
        is_and: bool,
        is_not: bool,
    },
}

#[derive(Debug)]
pub struct Node {
    pub active: bool,
    pub insertion_order: usize,
    pub start: u32,
    pub end: u32,
    pub kind: BlockKind,
    pub statements: Vec<Statement>,
    pub successors: Vec<NodeId>,
    pub predecessors: Vec<NodeId>,
    pub catch_successors: Vec<NodeId>,
    pub catch_predecessors: Vec<NodeId>,
    pub catch_source: bool,
    pub catch_target: bool,
    pub catch_order: Option<u64>,
    pub catch_handlers: Vec<(NodeId, Option<String>)>,
    pub branch_true: Option<NodeId>,
    pub branch_false: Option<NodeId>,
    pub switch_cases: Vec<(i32, NodeId)>,
    pub switch_default: Option<NodeId>,
    pub declarations: Vec<VariableId>,
    pub catch_types: Vec<String>,
    pub rpo: usize,
    pub post_order: usize,
    pub loop_kind: LoopKind,
    pub loop_follow: Option<NodeId>,
    pub if_follow: Option<NodeId>,
    pub switch_follow: Option<NodeId>,
    pub latch: Option<NodeId>,
    pub loop_nodes: Vec<NodeId>,
    pub in_catch: bool,
    pub condition: Option<ConditionTree>,
    pub wrapped: Option<NodeId>,
    pub catches: Vec<NodeId>,
    pub try_follow: Option<NodeId>,
}

impl Node {
    fn from_lowered(block: crate::ir::opcode::LoweredBlock) -> Self {
        let catch_source = !block.exception_successors.is_empty();
        let switch_values = block.statements.iter().find_map(|statement| {
            if let Statement::Switch { values, .. } = statement {
                Some(values.as_slice())
            } else {
                None
            }
        });
        let switch_default = switch_values.and_then(|_| block.successors.first().copied());
        let switch_cases = switch_values
            .map(|values| {
                values
                    .iter()
                    .copied()
                    .zip(block.successors.iter().skip(1).copied())
                    .collect()
            })
            .unwrap_or_default();
        Self {
            active: true,
            insertion_order: usize::MAX,
            start: block.start,
            end: block.end,
            kind: block.kind,
            statements: block.statements,
            successors: block.successors,
            predecessors: block.predecessors,
            catch_successors: block.exception_successors,
            catch_predecessors: Vec::new(),
            catch_source,
            catch_target: false,
            catch_order: None,
            catch_handlers: block.exception_handlers,
            branch_true: block.branch_true,
            branch_false: block.branch_false,
            switch_cases,
            switch_default,
            declarations: Vec::new(),
            catch_types: Vec::new(),
            rpo: 0,
            post_order: 0,
            loop_kind: LoopKind::None,
            loop_follow: None,
            if_follow: None,
            switch_follow: None,
            latch: None,
            loop_nodes: Vec::new(),
            in_catch: false,
            condition: None,
            wrapped: None,
            catches: Vec::new(),
            try_follow: None,
        }
    }
}

#[derive(Debug)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub variables: VariableTable,
    pub entry: NodeId,
    pub exit: Option<NodeId>,
    pub rpo: Vec<NodeId>,
    visit_marks: Vec<u32>,
    visit_epoch: u32,
    next_node_order: usize,
}

impl Graph {
    pub fn from_lowered(method: LoweredMethod) -> Self {
        let mut nodes: Vec<_> = method.blocks.into_iter().map(Node::from_lowered).collect();
        for node in &mut nodes {
            deduplicate(&mut node.successors);
            deduplicate(&mut node.catch_successors);
        }
        for source in 0..nodes.len() {
            let handlers = nodes[source].catch_handlers.clone();
            for (target, ty) in handlers {
                let ty = ty.unwrap_or_else(|| "Ljava/lang/Throwable;".to_owned());
                if !nodes[target].catch_types.contains(&ty) {
                    nodes[target].catch_types.push(ty);
                }
            }
        }
        rebuild_predecessors(&mut nodes);
        let visit_marks = vec![0; nodes.len()];

        let mut graph = Self {
            nodes,
            variables: method.variables,
            entry: 0,
            exit: None,
            rpo: Vec::new(),
            visit_marks,
            visit_epoch: 0,
            next_node_order: 0,
        };
        graph.initialize_node_and_catch_order();
        graph.retain_reachable();
        graph.compute_rpo();
        graph.exit = graph
            .rpo
            .iter()
            .copied()
            .filter(|node| graph.nodes[*node].kind == BlockKind::Return)
            .max_by_key(|node| graph.nodes[*node].rpo);
        graph.propagate_catch_membership();
        graph
    }

    pub fn active_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(id, node)| node.active.then_some(id))
    }

    pub(crate) fn debug_dump(&self, label: &str) {
        eprintln!(
            "[radguard-debug] CFG {label}: entry={} active_nodes={} rpo={:?}",
            self.entry,
            self.active_nodes().count(),
            self.rpo
        );
        for (id, node) in self.nodes.iter().enumerate() {
            if !node.active && node.wrapped.is_none() {
                continue;
            }
            eprintln!(
                "[radguard-debug]   node={} active={} range={}..{} kind={:?} rpo={} pred={:?} succ={:?} branch=({:?},{:?}) wrapped={:?} statements={:?}",
                id,
                node.active,
                node.start,
                node.end,
                node.kind,
                node.rpo,
                node.predecessors,
                node.successors,
                node.branch_true,
                node.branch_false,
                node.wrapped,
                node.statements
            );
        }
        for (variable, info) in self.variables.iter() {
            eprintln!(
                "[radguard-debug]   var={:?} name={:?} ty={:?} declared={} this={} super={}",
                variable,
                self.variables.name(variable),
                info.ty,
                self.variables.is_declared(variable),
                self.variables.is_this(variable),
                self.variables.is_super(variable)
            );
        }
    }

    pub(crate) fn debug_loop_metadata(&self, label: &str) {
        eprintln!("[radguard-debug] loops {label}:");
        for (id, node) in self.nodes.iter().enumerate() {
            if node.loop_kind == LoopKind::None && node.kind != BlockKind::Loop {
                continue;
            }
            eprintln!(
                "[radguard-debug]   node={} active={} kind={:?} loop_kind={:?} latch={:?} follow={:?} nodes={:?} branches=({:?},{:?})",
                id,
                node.active,
                node.kind,
                node.loop_kind,
                node.latch,
                node.loop_follow,
                node.loop_nodes,
                node.branch_true,
                node.branch_false
            );
        }
    }

    pub fn successors(&self, node: NodeId) -> &[NodeId] {
        &self.nodes[node].successors
    }

    pub fn all_successors(&self, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes[node]
            .successors
            .iter()
            .chain(&self.nodes[node].catch_successors)
            .copied()
    }

    pub fn predecessors(&self, node: NodeId) -> &[NodeId] {
        &self.nodes[node].predecessors
    }

    pub fn all_predecessors(&self, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes[node]
            .predecessors
            .iter()
            .chain(&self.nodes[node].catch_predecessors)
            .copied()
    }

    pub fn add_edge(&mut self, source: NodeId, target: NodeId) {
        push_unique(&mut self.nodes[source].successors, target);
        push_unique(&mut self.nodes[target].predecessors, source);
    }

    pub fn add_catch_edge(&mut self, source: NodeId, target: NodeId) {
        self.nodes[source].catch_source = true;
        self.nodes[target].catch_target = true;
        let active_type = self.nodes[source].catch_order.or(self.nodes[target].catch_order);
        if active_type.is_some() {
            self.nodes[source].catch_order = active_type;
            self.nodes[target].catch_order = active_type;
        }
        push_unique(&mut self.nodes[source].catch_successors, target);
        push_unique(&mut self.nodes[target].catch_predecessors, source);
    }

    pub fn remove_edge(&mut self, source: NodeId, target: NodeId) {
        remove_value(&mut self.nodes[source].successors, target);
        remove_value(&mut self.nodes[target].predecessors, source);
    }

    pub fn remove_catch_edge(&mut self, source: NodeId, target: NodeId) {
        remove_value(&mut self.nodes[source].catch_successors, target);
        remove_value(&mut self.nodes[target].catch_predecessors, source);
    }

    pub fn remove_node(&mut self, node: NodeId) {
        if !self.nodes[node].active {
            return;
        }
        let predecessors = self.nodes[node].predecessors.clone();
        let successors = self.nodes[node].successors.clone();
        let catch_predecessors = self.nodes[node].catch_predecessors.clone();
        let catch_successors = self.nodes[node].catch_successors.clone();
        for predecessor in predecessors {
            self.remove_edge(predecessor, node);
        }
        for successor in successors {
            self.remove_edge(node, successor);
        }
        for predecessor in catch_predecessors {
            self.remove_catch_edge(predecessor, node);
        }
        for successor in catch_successors {
            self.remove_catch_edge(node, successor);
        }
        self.nodes[node].active = false;
        self.rpo.retain(|candidate| *candidate != node);
    }

    pub(crate) fn append_wrapper(&mut self, kind: BlockKind, wrapped: NodeId) -> NodeId {
        let insertion_order = self.next_node_order;
        self.next_node_order += 1;
        let source = &self.nodes[wrapped];
        let id = self.nodes.len();
        self.nodes.push(Node {
            active: true,
            insertion_order,
            start: source.start,
            end: source.end,
            kind,
            statements: Vec::new(),
            successors: Vec::new(),
            predecessors: Vec::new(),
            catch_successors: Vec::new(),
            catch_predecessors: Vec::new(),
            catch_source: false,
            catch_target: false,
            catch_order: None,
            catch_handlers: Vec::new(),
            branch_true: source.branch_true,
            branch_false: source.branch_false,
            switch_cases: source.switch_cases.clone(),
            switch_default: source.switch_default,
            declarations: Vec::new(),
            catch_types: source.catch_types.clone(),
            rpo: source.rpo,
            post_order: source.post_order,
            loop_kind: source.loop_kind,
            loop_follow: source.loop_follow,
            if_follow: source.if_follow,
            switch_follow: source.switch_follow,
            latch: source.latch,
            loop_nodes: source.loop_nodes.clone(),
            in_catch: source.in_catch,
            condition: None,
            wrapped: Some(wrapped),
            catches: Vec::new(),
            try_follow: None,
        });
        id
    }

    pub(crate) fn replace_structural_references(&mut self, old: NodeId, new: NodeId) {
        self.replace_structural_references_before(old, new, self.nodes.len());
    }

    pub(crate) fn replace_structural_references_before(
        &mut self,
        old: NodeId,
        new: NodeId,
        end: usize,
    ) {
        for node in self.nodes.iter_mut().take(end) {
            if !node.active {
                continue;
            }
            replace_option(&mut node.branch_true, old, new);
            replace_option(&mut node.branch_false, old, new);
            replace_option(&mut node.loop_follow, old, new);
            replace_option(&mut node.if_follow, old, new);
            replace_option(&mut node.switch_follow, old, new);
            replace_option(&mut node.latch, old, new);
            replace_option(&mut node.switch_default, old, new);
            replace_option(&mut node.try_follow, old, new);
            for loop_node in &mut node.loop_nodes {
                if *loop_node == old {
                    *loop_node = new;
                }
            }
            for (_, target) in &mut node.switch_cases {
                if *target == old {
                    *target = new;
                }
            }
        }
    }

    pub fn compute_rpo(&mut self) {
        let mut post_order = std::mem::take(&mut self.rpo);
        post_order.clear();
        if self.nodes.is_empty() || !self.nodes[self.entry].active {
            self.rpo = post_order;
            return;
        }
        self.visit_marks.resize(self.nodes.len(), 0);
        self.visit_epoch = self.visit_epoch.wrapping_add(1);
        if self.visit_epoch == 0 {
            self.visit_marks.fill(0);
            self.visit_epoch = 1;
        }
        self.visit_marks[self.entry] = self.visit_epoch;
        let mut stack = vec![(self.entry, 0_usize)];
        while let Some((node, next)) = stack.pop() {
            let normal_successor_count = self.nodes[node].successors.len();
            let successor_count = normal_successor_count + self.nodes[node].catch_successors.len();
            if next < successor_count {
                stack.push((node, next + 1));
                let successor = if next < normal_successor_count {
                    self.nodes[node].successors[next]
                } else {
                    self.nodes[node].catch_successors[next - normal_successor_count]
                };
                if self.nodes[successor].active
                    && self.visit_marks[successor] != self.visit_epoch
                {
                    self.visit_marks[successor] = self.visit_epoch;
                    stack.push((successor, 0));
                }
            } else {
                post_order.push(node);
            }
        }
        for (index, node) in post_order.iter().copied().enumerate() {
            self.nodes[node].post_order = index + 1;
        }
        post_order.reverse();
        for (index, node) in post_order.iter().copied().enumerate() {
            self.nodes[node].rpo = index;
        }
        self.rpo = post_order;
    }

    pub fn immediate_dominators(&self) -> Vec<Option<NodeId>> {
        let mut idom = vec![None; self.nodes.len()];
        if self.rpo.is_empty() {
            return idom;
        }
        idom[self.entry] = Some(self.entry);
        let mut changed = true;
        while changed {
            changed = false;
            for &node in self.rpo.iter().skip(1) {
                let mut predecessors = self
                    .all_predecessors(node)
                    .filter(|predecessor| idom[*predecessor].is_some());
                let Some(mut new_idom) = predecessors.next() else {
                    continue;
                };
                for predecessor in predecessors {
                    new_idom = intersect(self, &idom, predecessor, new_idom);
                }
                if idom[node] != Some(new_idom) {
                    idom[node] = Some(new_idom);
                    changed = true;
                }
            }
        }
        idom[self.entry] = None;
        idom
    }

    pub fn split_condition_nodes(&mut self) {
        let mut candidates: Vec<_> = self
            .active_nodes()
            .filter(|node| {
                self.nodes[*node].kind == BlockKind::Condition
                    && self.nodes[*node].statements.len() > 1
            })
            .collect();
        candidates.sort_by_key(|node| self.nodes[*node].insertion_order);
        for node in candidates {
            let condition = self.nodes[node]
                .statements
                .pop()
                .expect("condition node is non-empty");
            let predecessors = self.nodes[node].predecessors.clone();
            let catch_predecessors = self.nodes[node].catch_predecessors.clone();
            let old_successors = std::mem::take(&mut self.nodes[node].successors);
            for successor in &old_successors {
                remove_value(&mut self.nodes[*successor].predecessors, node);
            }
            let pre_order = self.next_node_order;
            let condition_order = pre_order + 1;
            self.next_node_order += 2;
            self.nodes[node].insertion_order = pre_order;
            let condition_node = self.nodes.len();
            self.nodes.push(Node {
                active: true,
                insertion_order: condition_order,
                start: self.nodes[node].end,
                end: self.nodes[node].end,
                kind: BlockKind::Condition,
                statements: vec![condition],
                successors: old_successors,
                predecessors: vec![node],
                catch_successors: Vec::new(),
                catch_predecessors: Vec::new(),
                catch_source: false,
                catch_target: false,
                catch_order: None,
                catch_handlers: Vec::new(),
                branch_true: self.nodes[node].branch_true,
                branch_false: self.nodes[node].branch_false,
                switch_cases: self.nodes[node].switch_cases.clone(),
                switch_default: self.nodes[node].switch_default,
                declarations: Vec::new(),
                catch_types: self.nodes[node].catch_types.clone(),
                rpo: 0,
                post_order: 0,
                loop_kind: self.nodes[node].loop_kind,
                loop_follow: self.nodes[node].loop_follow,
                if_follow: self.nodes[node].if_follow,
                switch_follow: self.nodes[node].switch_follow,
                latch: self.nodes[node].latch,
                loop_nodes: self.nodes[node].loop_nodes.clone(),
                in_catch: self.nodes[node].in_catch,
                condition: None,
                wrapped: None,
                catches: Vec::new(),
                try_follow: None,
            });
            let successors = self.nodes[condition_node].successors.clone();
            for successor in successors {
                push_unique(&mut self.nodes[successor].predecessors, condition_node);
            }
            self.nodes[node].kind = BlockKind::Statement;
            self.nodes[node].successors = vec![condition_node];
            self.nodes[node].branch_true = None;
            self.nodes[node].branch_false = None;
            self.nodes[node].catch_order = None;
            for &predecessor in &predecessors {
                if predecessor != node {
                    self.remove_edge(predecessor, node);
                    self.add_edge(predecessor, node);
                }
            }
            for predecessor in catch_predecessors {
                if predecessor != node {
                    self.remove_catch_edge(predecessor, node);
                    if !predecessors.contains(&predecessor) {
                        self.add_catch_edge(predecessor, node);
                    }
                }
            }
            for successor in self.nodes[node].catch_successors.clone() {
                let active_type = self.nodes[node]
                    .catch_order
                    .or(self.nodes[successor].catch_order);
                if active_type.is_some() {
                    self.nodes[node].catch_order = active_type;
                    self.nodes[successor].catch_order = active_type;
                }
            }
        }
        self.compute_rpo();
    }

    pub fn simplify(&mut self) {
        loop {
            let mut changed = false;
            let candidates: Vec<_> = self.active_nodes().collect();
            for node in candidates {
                if !self.nodes[node].active || self.nodes[node].kind != BlockKind::Statement {
                    continue;
                }
                let all_successors: Vec<_> = self.all_successors(node).collect();
                if all_successors.len() != 1 {
                    continue;
                }
                let successor = all_successors[0];
                if self.nodes[node].statements.is_empty() {
                    if node == successor
                        || self
                            .all_predecessors(node)
                            .any(|pred| self.nodes[pred].kind == BlockKind::Switch)
                    {
                        continue;
                    }
                    self.redirect_node(node, successor);
                    if self.entry == node {
                        self.entry = successor;
                    }
                    self.remove_node(node);
                    changed = true;
                    continue;
                }
                if self.nodes[successor].kind == BlockKind::Statement
                    && self.all_predecessors(successor).count() == 1
                    && self.nodes[successor].catch_successors.is_empty()
                    && node != successor
                    && successor != self.entry
                {
                    self.merge_statement_nodes(node, successor);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        self.compute_rpo();
    }

    fn retain_reachable(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        let mut reachable = vec![false; self.nodes.len()];
        let mut queue = VecDeque::from([self.entry]);
        reachable[self.entry] = true;
        while let Some(node) = queue.pop_front() {
            for successor in self.nodes[node]
                .successors
                .iter()
                .chain(&self.nodes[node].catch_successors)
                .copied()
            {
                if !reachable[successor] {
                    reachable[successor] = true;
                    queue.push_back(successor);
                }
            }
        }
        for (id, is_reachable) in reachable.into_iter().enumerate() {
            self.nodes[id].active = is_reachable;
        }
        rebuild_predecessors(&mut self.nodes);
    }

    fn initialize_node_and_catch_order(&mut self) {
        let mut visited = vec![false; self.nodes.len()];
        let mut queue = VecDeque::from([self.entry]);
        visited[self.entry] = true;
        while let Some(node) = queue.pop_front() {
            self.nodes[node].insertion_order = self.next_node_order;
            self.next_node_order += 1;
            let catch_successors = self.nodes[node].catch_successors.clone();
            let mut source_type = None;
            for handler in 0..self.nodes[node].catch_handlers.len() {
                let target = self.nodes[node].catch_handlers[handler].0;
                let token = Some(((node as u64) << 32) | handler as u64);
                self.nodes[target].catch_order = token;
                source_type = token;
            }
            self.nodes[node].catch_order = source_type;
            for target in catch_successors {
                if !visited[target] {
                    self.nodes[target].in_catch = true;
                    visited[target] = true;
                    queue.push_back(target);
                }
            }
            for target in self.nodes[node].successors.clone() {
                if !visited[target] {
                    visited[target] = true;
                    queue.push_back(target);
                }
            }
        }
    }

    fn propagate_catch_membership(&mut self) {
        for &node in &self.rpo {
            let node_rpo = self.nodes[node].rpo;
            let predecessors: Vec<_> = self
                .all_predecessors(node)
                .filter(|predecessor| self.nodes[*predecessor].rpo < node_rpo)
                .collect();
            if !predecessors.is_empty()
                && predecessors
                    .iter()
                    .all(|predecessor| self.nodes[*predecessor].in_catch)
            {
                self.nodes[node].in_catch = true;
            }
        }
    }

    fn redirect_node(&mut self, old: NodeId, new: NodeId) {
        let normal_predecessors = self.nodes[old].predecessors.clone();
        for &predecessor in &normal_predecessors {
            self.remove_edge(predecessor, old);
            self.add_edge(predecessor, new);
            replace_option(&mut self.nodes[predecessor].branch_true, old, new);
            replace_option(&mut self.nodes[predecessor].branch_false, old, new);
        }
        let catch_predecessors = self.nodes[old].catch_predecessors.clone();
        let inherited_catch_order = self.nodes[old].catch_order.or_else(|| {
            let old_type = self.nodes[old].catch_types.first()?;
            catch_predecessors.iter().find_map(|source| {
                self.nodes[*source]
                    .catch_handlers
                    .iter()
                    .position(|(target, ty)| {
                        *target == old && ty.as_deref() == Some(old_type.as_str())
                    })
                    .map(|handler| ((*source as u64) << 32) | handler as u64)
            })
        });
        for predecessor in catch_predecessors {
            self.remove_catch_edge(predecessor, old);
            if !normal_predecessors.contains(&predecessor) {
                self.add_catch_edge(predecessor, new);
            }
        }
        if self.nodes[new].catch_order.is_none() {
            self.nodes[new].catch_order = inherited_catch_order;
        }
    }

    fn merge_statement_nodes(&mut self, node: NodeId, successor: NodeId) {
        self.remove_edge(node, successor);
        let statements = std::mem::take(&mut self.nodes[successor].statements);
        self.nodes[node].statements.extend(statements);
        let declarations = std::mem::take(&mut self.nodes[successor].declarations);
        for declaration in declarations {
            push_unique(&mut self.nodes[node].declarations, declaration);
        }
        let normal_successors = self.nodes[successor].successors.clone();
        for next in normal_successors {
            self.remove_edge(successor, next);
            self.add_edge(node, next);
        }
        let catch_successors = self.nodes[successor].catch_successors.clone();
        for next in catch_successors {
            self.remove_catch_edge(successor, next);
            self.add_catch_edge(node, next);
        }
        self.remove_node(successor);
    }
}

fn intersect(
    graph: &Graph,
    idom: &[Option<NodeId>],
    mut left: NodeId,
    mut right: NodeId,
) -> NodeId {
    while left != right {
        while graph.nodes[left].rpo > graph.nodes[right].rpo {
            left = idom[left].expect("processed dominator");
        }
        while graph.nodes[right].rpo > graph.nodes[left].rpo {
            right = idom[right].expect("processed dominator");
        }
    }
    left
}

fn rebuild_predecessors(nodes: &mut [Node]) {
    for node in nodes.iter_mut() {
        node.predecessors.clear();
        node.catch_predecessors.clear();
    }
    let mut sources: Vec<_> = (0..nodes.len()).collect();
    sources.sort_by_key(|source| (nodes[*source].insertion_order, *source));
    for source in sources {
        if !nodes[source].active {
            continue;
        }
        let successors = nodes[source].successors.clone();
        for target in successors {
            if nodes[target].active {
                push_unique(&mut nodes[target].predecessors, source);
            }
        }
        let successors = nodes[source].catch_successors.clone();
        for target in successors {
            if nodes[target].active {
                nodes[target].catch_target = true;
                push_unique(&mut nodes[target].catch_predecessors, source);
            }
        }
    }
}

fn deduplicate(values: &mut Vec<NodeId>) {
    let mut seen = HashSet::with_capacity(values.len());
    values.retain(|value| seen.insert(*value));
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn remove_value(values: &mut Vec<NodeId>, value: NodeId) {
    values.retain(|candidate| *candidate != value);
}

fn replace_option(value: &mut Option<NodeId>, old: NodeId, new: NodeId) {
    if *value == Some(old) {
        *value = Some(new);
    }
}
