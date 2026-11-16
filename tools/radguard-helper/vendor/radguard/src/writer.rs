use crate::graph::{ConditionTree, Graph, LoopKind, NodeId};
use crate::ir::opcode::BlockKind;
use crate::ir::{
    AssignKind, BinaryOp, Constant, ConstantValue, FieldReference, InvokeKind, MethodReference,
    Statement, UnaryOp, Value, VariableId,
};
use crate::types::{descriptor_to_java, descriptor_to_java_enhanced};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MemberKind {
    Method,
    Field,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberReference {
    pub start: usize,
    pub end: usize,
    pub kind: MemberKind,
    pub class: String,
    pub name: String,
    pub descriptor: String,
    pub declaration: bool,
    pub synthetic: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SourceOutput {
    pub source: String,
    pub member_references: Vec<MemberReference>,
}

impl SourceOutput {
    pub fn append(&mut self, mut other: Self) {
        let base = self.source.len();
        for reference in &mut other.member_references {
            reference.start += base;
            reference.end += base;
        }
        self.source.push_str(&other.source);
        self.member_references.extend(other.member_references);
    }
}

pub struct MethodWriter<'a> {
    graph: &'a Graph,
    debug: bool,
    enhanced: bool,
    output: SourceOutput,
    declared: Vec<bool>,
    visited: Vec<bool>,
    conditions: Vec<Option<ConditionTree>>,
    negated_condition_leaves: Vec<bool>,
    indentation: usize,
    loop_follow: Vec<Option<NodeId>>,
    if_follow: Vec<Option<NodeId>>,
    switch_follow: Vec<Option<NodeId>>,
    latch_node: Vec<Option<NodeId>>,
    try_follow: Vec<Option<NodeId>>,
    next_case: Option<NodeId>,
    need_break: bool,
    constructor: bool,
    current_class: &'a str,
    this_is_super: bool,
    pending_for_init: Option<(NodeId, VariableId, Value)>,
    skipped_for_step: Option<(NodeId, usize)>,
    pending_foreach_init: Option<(NodeId, VariableId, Value)>,
    in_else_if: bool,
}

impl<'a> MethodWriter<'a> {
    pub fn new(graph: &'a Graph, current_class: &'a str, constructor: bool) -> Self {
        let conditions = graph
            .nodes
            .iter()
            .map(|node| node.condition.clone())
            .collect();
        Self::with_conditions(
            graph,
            current_class,
            constructor,
            conditions,
            SourceOutput::default(),
            false,
            false,
        )
    }

    pub(crate) fn new_moving_conditions(
        graph: &'a mut Graph,
        current_class: &'a str,
        constructor: bool,
        output: SourceOutput,
        debug: bool,
        enhanced: bool,
    ) -> Self {
        let conditions = graph
            .nodes
            .iter_mut()
            .map(|node| node.condition.take())
            .collect();
        Self::with_conditions(
            graph,
            current_class,
            constructor,
            conditions,
            output,
            debug,
            enhanced,
        )
    }

    fn with_conditions(
        graph: &'a Graph,
        current_class: &'a str,
        constructor: bool,
        conditions: Vec<Option<ConditionTree>>,
        output: SourceOutput,
        debug: bool,
        enhanced: bool,
    ) -> Self {
        Self {
            graph,
            debug,
            enhanced,
            output,
            declared: {
                let mut declared = vec![false; graph.variables.dense_len()];
                for variable in graph.variables.parameter_ids() {
                    declared[graph.variables.dense_index(variable).expect("known parameter")] = true;
                }
                declared
            },
            visited: vec![false; graph.nodes.len()],
            conditions,
            negated_condition_leaves: vec![false; graph.nodes.len()],
            indentation: 4,
            loop_follow: vec![None],
            if_follow: vec![None],
            switch_follow: vec![None],
            latch_node: vec![None],
            try_follow: vec![None],
            next_case: None,
            need_break: true,
            constructor,
            current_class,
            this_is_super: false,
            pending_for_init: None,
            skipped_for_step: None,
            pending_foreach_init: None,
            in_else_if: false,
        }
    }

    pub fn write_body(mut self) -> SourceOutput {
        self.write("\n");
        self.write_indent();
        self.write("{\n");
        self.indentation += 4;
        self.visit_node(self.graph.entry);
        self.indentation -= 4;
        self.write_indent();
        self.write("}\n");
        self.output
    }

    fn visit_node(&mut self, node: NodeId) {
        if self
            .if_follow
            .last()
            .into_iter()
            .chain(self.switch_follow.last())
            .chain(self.loop_follow.last())
            .chain(self.latch_node.last())
            .chain(self.try_follow.last())
            .any(|follow| *follow == Some(node))
        {
            return;
        }
        if self.graph.nodes[node].kind != BlockKind::Return && self.visited[node] {
            return;
        }
        self.visited[node] = true;
        let declarations = self.graph.nodes[node].declarations.clone();
        for variable in declarations {
            if self.enhanced && self.should_defer_declaration(node, variable) {
                continue;
            }
            self.write_declaration(variable);
        }

        match self.graph.nodes[node].kind {
            BlockKind::Statement => self.visit_statement_node(node),
            BlockKind::Return => {
                self.need_break = false;
                self.visit_statements(node);
            }
            BlockKind::Throw => self.visit_statements(node),
            BlockKind::Condition | BlockKind::ShortCircuit => self.visit_condition_node(node),
            BlockKind::Switch => self.visit_switch_node(node),
            BlockKind::Loop => self.visit_loop_node(node),
            BlockKind::Try => self.visit_try_node(node),
            BlockKind::Catch => self.visit_catch_node(node),
        }
    }

    fn should_defer_declaration(&self, node: NodeId, variable: VariableId) -> bool {
        let block = &self.graph.nodes[node];
        if block.kind == BlockKind::Statement
            && let Some(Statement::Assign {
                lhs: Some(init_var),
                kind,
                ..
            }) = block.statements.last()
            && *init_var == variable
            && *kind != AssignKind::MoveResult
            && let [loop_succ] = block.successors.as_slice()
            && !self.visited[*loop_succ]
            && self.graph.nodes[*loop_succ].kind == BlockKind::Loop
            && self.graph.nodes[*loop_succ].loop_kind == LoopKind::PreTest
            && self.graph.nodes[*loop_succ].declarations.is_empty()
            && let Some(wrapped) = self.graph.nodes[*loop_succ].wrapped
            && self
                .detect_for_loop_step(*loop_succ, wrapped, *init_var)
                .is_some()
            && !block.statements[..block.statements.len() - 1]
                .iter()
                .any(|s| {
                    let mut v = Vec::new();
                    s.collect_used_variables_vec(&mut v);
                    v.contains(&variable) || s.defined_variable() == Some(variable)
                })
        {
            return true;
        }
        if block.kind == BlockKind::Statement
            && let [cond_succ] = block.successors.as_slice()
            && !self.visited[*cond_succ]
            && self.graph.nodes[*cond_succ].declarations.is_empty()
            && self.is_ternary_condition_for_var(*cond_succ, variable)
            && !block.statements.iter().any(|s| {
                let mut v = Vec::new();
                s.collect_used_variables_vec(&mut v);
                v.contains(&variable) || s.defined_variable() == Some(variable)
            })
        {
            return true;
        }
        if self.is_ternary_condition_for_var(node, variable)
            && !block.statements.iter().any(|s| {
                let mut v = Vec::new();
                s.collect_used_variables_vec(&mut v);
                v.contains(&variable) || s.defined_variable() == Some(variable)
            })
        {
            return true;
        }
        false
    }

    fn is_ternary_condition_for_var(&self, node: NodeId, variable: VariableId) -> bool {
        let block = &self.graph.nodes[node];
        if !matches!(block.kind, BlockKind::Condition | BlockKind::ShortCircuit) {
            return false;
        }
        let Some(follow) = block.if_follow else {
            return false;
        };
        let mut true_branch = block.branch_true;
        let mut false_branch = block.branch_false;
        if true_branch == Some(follow)
            || true_branch == self.next_case
            || true_branch.is_some_and(|target| block.rpo > self.graph.nodes[target].rpo)
        {
            std::mem::swap(&mut true_branch, &mut false_branch);
        }
        if let (Some(t_node), Some(f_node)) = (true_branch, false_branch)
            && t_node != follow
            && f_node != follow
            && !self.visited[t_node]
            && !self.visited[f_node]
            && self.ternary_assign_target(t_node, f_node, follow) == Some(variable)
        {
            return true;
        }
        false
    }

    fn ternary_assign_target(
        &self,
        true_node: NodeId,
        false_node: NodeId,
        follow: NodeId,
    ) -> Option<VariableId> {
        let t_block = &self.graph.nodes[true_node];
        let f_block = &self.graph.nodes[false_node];
        if t_block.kind != BlockKind::Statement
            || f_block.kind != BlockKind::Statement
            || t_block.successors.as_slice() != [follow]
            || f_block.successors.as_slice() != [follow]
            || !t_block.declarations.is_empty()
            || !f_block.declarations.is_empty()
            || t_block.statements.len() != 1
            || f_block.statements.len() != 1
        {
            return None;
        }
        let (
            Statement::Assign {
                lhs: Some(t_lhs),
                rhs: t_rhs,
                ..
            },
            Statement::Assign {
                lhs: Some(f_lhs),
                rhs: f_rhs,
                ..
            },
        ) = (&t_block.statements[0], &f_block.statements[0])
        else {
            return None;
        };
        if t_lhs != f_lhs
            || matches!(t_rhs, Value::Variable(VariableId::Temporary(_)))
            || matches!(f_rhs, Value::Variable(VariableId::Temporary(_)))
            || self.should_skip_constructor(t_rhs)
            || self.should_skip_constructor(f_rhs)
            || is_void_super_invoke(t_rhs)
            || is_void_super_invoke(f_rhs)
        {
            return None;
        }
        Some(*t_lhs)
    }

    fn visit_loop_node(&mut self, node: NodeId) {
        let loop_node = &self.graph.nodes[node];
        if self.debug {
            eprintln!(
                "[radguard-debug] writer loop node={} kind={:?} wrapped={:?} latch={:?} follow={:?} branches=({:?},{:?})",
                node,
                loop_node.loop_kind,
                loop_node.wrapped,
                loop_node.latch,
                loop_node.loop_follow,
                loop_node.branch_true,
                loop_node.branch_false
            );
        }
        let follow = loop_node.loop_follow;
        let wrapped = loop_node.wrapped.expect("loop wrapper has a condition");
        let mut true_branch = loop_node.branch_true;
        let mut false_branch = loop_node.branch_false;
        let mut negate = false;

        let mut saved_skipped_step = None;
        match loop_node.loop_kind {
            LoopKind::PreTest => {
                if true_branch == follow {
                    negate = true;
                    std::mem::swap(&mut true_branch, &mut false_branch);
                }
                let foreach_header = if self.enhanced {
                    self.pending_foreach_init
                        .take()
                        .and_then(|(target_loop, iter_var, iterable_val)| {
                            if target_loop == node
                                && let Some(body_node) = true_branch
                                && let Some((elem_var, elem_ty_desc)) = self.detect_foreach_loop(
                                    node,
                                    wrapped,
                                    negate,
                                    body_node,
                                    iter_var,
                                )
                            {
                                Some((body_node, elem_var, elem_ty_desc, iterable_val))
                            } else {
                                None
                            }
                        })
                } else {
                    None
                };
                let for_header = if self.enhanced && foreach_header.is_none() {
                    self.pending_for_init.take().and_then(|(target_loop, init_var, init_rhs)| {
                        if target_loop == node {
                            self.detect_for_loop_step(node, wrapped, init_var)
                                .map(|(latch, step_idx, step_rhs)| {
                                    (init_var, init_rhs, latch, step_idx, step_rhs)
                                })
                        } else {
                            None
                        }
                    })
                } else {
                    self.pending_for_init = None;
                    None
                };
                if let Some((body_node, elem_var, elem_ty_desc, iterable_val)) = foreach_header {
                    self.write_indent();
                    self.write("for (");
                    self.mark_declared(elem_var);
                    self.write(&self.format_descriptor(&elem_ty_desc));
                    self.write(" ");
                    self.write_variable_name(elem_var);
                    self.write(" : ");
                    self.write_value(&iterable_val);
                    self.write(") {\n");
                    saved_skipped_step = self.skipped_for_step.replace((body_node, 0));
                } else if let Some((init_var, init_rhs, latch, step_idx, step_rhs)) = for_header {
                    self.write_indent();
                    self.write("for (");
                    self.write_variable(init_var);
                    self.write(" = ");
                    self.write_value(&init_rhs);
                    self.write("; ");
                    self.write_condition(wrapped, negate);
                    self.write("; ");
                    self.write_for_step(init_var, &step_rhs);
                    self.write(") {\n");
                    saved_skipped_step = self.skipped_for_step.replace((latch, step_idx));
                } else {
                    self.write_indent();
                    self.write("while (");
                    self.write_condition(wrapped, negate);
                    self.write(") {\n");
                }
            }
            LoopKind::PostTest => {
                self.write_indent();
                self.write("do {\n");
                self.latch_node.push(loop_node.latch);
            }
            LoopKind::Endless | LoopKind::None => {
                self.write_indent();
                self.write("while(true) {\n");
            }
        }

        self.indentation += 4;
        self.loop_follow.push(follow);
        if loop_node.loop_kind == LoopKind::PreTest {
            if let Some(target) = true_branch {
                self.visit_node(target);
            }
        } else {
            self.visit_node(wrapped);
        }
        self.loop_follow.pop();
        self.indentation -= 4;
        if saved_skipped_step.is_some() {
            self.skipped_for_step = saved_skipped_step;
        } else {
            self.skipped_for_step = None;
        }

        match loop_node.loop_kind {
            LoopKind::PreTest => {
                self.write_indent();
                self.write("}\n");
            }
            LoopKind::PostTest => {
                self.latch_node.pop();
                self.write_indent();
                self.write("} while(");
                if let Some(latch) = loop_node.latch {
                    self.write_wrapped_condition(latch, false);
                }
                self.write(");\n");
            }
            LoopKind::Endless | LoopKind::None => {
                if let Some(latch) = loop_node.latch {
                    self.indentation += 4;
                    self.visit_node(latch);
                    self.indentation -= 4;
                }
                self.write_indent();
                self.write("}\n");
            }
        }
        if let Some(follow) = follow {
            self.visit_node(follow);
        }
    }

    fn visit_condition_node(&mut self, node: NodeId) {
        let condition = &self.graph.nodes[node];
        if self.debug {
            eprintln!(
                "[radguard-debug] writer condition node={} follow={:?} branches=({:?},{:?}) statements={:?}",
                node,
                condition.if_follow,
                condition.branch_true,
                condition.branch_false,
                condition.statements
            );
        }
        let follow = condition.if_follow;
        let mut true_branch = condition.branch_true;
        let mut false_branch = condition.branch_false;
        let mut negate = false;

        let is_else_if = self.in_else_if;
        self.in_else_if = false;
        let write_if_prefix = |writer: &mut Self| {
            if !is_else_if {
                writer.write_indent();
            }
            writer.write("if (");
        };

        if true_branch == false_branch {
            if is_else_if {
                self.write("{\n");
                self.indentation += 4;
            }
            self.write_indent();
            self.write("// Both branches of the condition point to the same code.\n");
            self.write_indent();
            self.write("// if (");
            self.write_condition(node, false);
            self.write(") {\n");
            self.indentation += 4;
            if let Some(target) = true_branch {
                self.visit_node(target);
            }
            self.indentation -= 4;
            self.write_indent();
            self.write("// }\n");
            if is_else_if {
                self.indentation -= 4;
                self.write_indent();
                self.write("}\n");
            }
            return;
        }
        if false_branch == *self.loop_follow.last().unwrap_or(&None) {
            negate = true;
            std::mem::swap(&mut true_branch, &mut false_branch);
        }
        let loop_follow = *self.loop_follow.last().unwrap_or(&None);
        if loop_follow == true_branch || loop_follow == false_branch {
            write_if_prefix(self);
            self.write_condition(node, negate);
            self.write(") {\n");
            self.indentation += 4;
            self.write_indent();
            self.write("break;\n");
            self.indentation -= 4;
            self.write_indent();
            self.write("}\n");
            if let Some(target) = false_branch {
                self.visit_node(target);
            }
            return;
        }

        if let Some(follow) = follow {
            if true_branch == Some(follow)
                || true_branch == self.next_case
                || true_branch.is_some_and(|target| condition.rpo > self.graph.nodes[target].rpo)
            {
                negate = !negate;
                std::mem::swap(&mut true_branch, &mut false_branch);
            }
            if self.enhanced
                && !is_else_if
                && let (Some(t_node), Some(f_node)) = (true_branch, false_branch)
                && t_node != follow
                && f_node != follow
                && !self.visited[t_node]
                && !self.visited[f_node]
                && self.try_write_ternary_assign(node, negate, t_node, f_node, follow)
            {
                self.visited[t_node] = true;
                self.visited[f_node] = true;
                self.visit_node(follow);
                return;
            }
            self.if_follow.push(Some(follow));
            if let Some(target) = true_branch {
                write_if_prefix(self);
                self.write_condition(node, negate);
                self.write(") {\n");
                self.indentation += 4;
                self.visit_node(target);
                self.indentation -= 4;
            }
            let is_else = true_branch != Some(follow) && false_branch != Some(follow);
            if is_else
                && false_branch.is_some_and(|target| !self.visited[target])
            {
                let f_target = false_branch.expect("checked branch");
                if self.enhanced && self.can_chain_else_if(f_target, Some(follow)) {
                    self.write_indent();
                    self.write("} else ");
                    self.in_else_if = true;
                    self.visit_node(f_target);
                    self.if_follow.pop();
                    if !self.visited[follow] {
                        self.visit_node(follow);
                    }
                    return;
                }
                self.write_indent();
                self.write("} else {\n");
                self.indentation += 4;
                self.visit_node(f_target);
                self.indentation -= 4;
            }
            self.if_follow.pop();
            self.write_indent();
            self.write("}\n");
            self.visit_node(follow);
        } else {
            if self.enhanced
                && !is_else_if
                && let (Some(t_node), Some(f_node)) = (true_branch, false_branch)
                && self.try_write_ternary_return(node, negate, t_node, f_node)
            {
                self.visited[t_node] = true;
                self.visited[f_node] = true;
                return;
            }
            write_if_prefix(self);
            self.write_condition(node, negate);
            self.write(") {\n");
            self.indentation += 4;
            if let Some(target) = true_branch {
                self.visit_node(target);
            }
            self.indentation -= 4;
            if let Some(f_target) = false_branch
                && self.enhanced
                && self.can_chain_else_if(f_target, None)
            {
                self.write_indent();
                self.write("} else ");
                self.in_else_if = true;
                self.visit_node(f_target);
                return;
            }
            self.write_indent();
            self.write("} else {\n");
            self.indentation += 4;
            if let Some(target) = false_branch {
                self.visit_node(target);
            }
            self.indentation -= 4;
            self.write_indent();
            self.write("}\n");
        }
    }

    fn can_chain_else_if(&self, target: NodeId, outer_follow: Option<NodeId>) -> bool {
        let block = &self.graph.nodes[target];
        if !matches!(block.kind, BlockKind::Condition | BlockKind::ShortCircuit)
            || !block.declarations.is_empty()
            || block.if_follow != outer_follow
            || block.branch_true == block.branch_false
        {
            return false;
        }
        let loop_follow = *self.loop_follow.last().unwrap_or(&None);
        if loop_follow.is_some()
            && (block.branch_true == loop_follow || block.branch_false == loop_follow)
        {
            return false;
        }
        true
    }

    fn try_write_ternary_assign(
        &mut self,
        condition_node: NodeId,
        negate: bool,
        true_node: NodeId,
        false_node: NodeId,
        follow: NodeId,
    ) -> bool {
        let Some(t_lhs) = self.ternary_assign_target(true_node, false_node, follow) else {
            return false;
        };
        let (
            Statement::Assign { rhs: t_rhs, .. },
            Statement::Assign { rhs: f_rhs, .. },
        ) = (
            &self.graph.nodes[true_node].statements[0],
            &self.graph.nodes[false_node].statements[0],
        )
        else {
            return false;
        };
        let lhs_is_bool = self
            .graph
            .variables
            .get(t_lhs)
            .and_then(|v| v.ty.as_deref())
            == Some("Z");
        let t_bool = match t_rhs {
            Value::Constant(Constant {
                value: ConstantValue::Integer { value, ty: "Z" | "I" },
            }) if lhs_is_bool && (*value == 0 || *value == 1) => Some(*value != 0),
            _ => None,
        };
        let f_bool = match f_rhs {
            Value::Constant(Constant {
                value: ConstantValue::Integer { value, ty: "Z" | "I" },
            }) if lhs_is_bool && (*value == 0 || *value == 1) => Some(*value != 0),
            _ => None,
        };
        self.write_indent();
        self.write_variable(t_lhs);
        if let (Some(tb), Some(fb)) = (t_bool, f_bool)
            && tb != fb
        {
            self.write(" = ");
            self.write_condition(condition_node, if tb { negate } else { !negate });
            self.write(";\n");
            return true;
        }
        self.write(" = (");
        self.write_condition(condition_node, negate);
        self.write(") ? ");
        self.write_value(t_rhs);
        self.write(" : ");
        self.write_value(f_rhs);
        self.write(";\n");
        true
    }

    fn try_write_ternary_return(
        &mut self,
        condition_node: NodeId,
        negate: bool,
        true_node: NodeId,
        false_node: NodeId,
    ) -> bool {
        let t_block = &self.graph.nodes[true_node];
        let f_block = &self.graph.nodes[false_node];
        if t_block.kind != BlockKind::Return
            || f_block.kind != BlockKind::Return
            || !t_block.declarations.is_empty()
            || !f_block.declarations.is_empty()
            || t_block.statements.len() != 1
            || f_block.statements.len() != 1
        {
            return false;
        }
        let (Statement::Return(Some(t_val)), Statement::Return(Some(f_val))) =
            (&t_block.statements[0], &f_block.statements[0])
        else {
            return false;
        };
        let t_bool = match t_val {
            Value::Constant(Constant {
                value: ConstantValue::Integer { value, ty: "Z" | "I" },
            }) if *value == 0 || *value == 1 => Some(*value != 0),
            _ => None,
        };
        let f_bool = match f_val {
            Value::Constant(Constant {
                value: ConstantValue::Integer { value, ty: "Z" | "I" },
            }) if *value == 0 || *value == 1 => Some(*value != 0),
            _ => None,
        };
        self.need_break = false;
        self.write_indent();
        if let (Some(tb), Some(fb)) = (t_bool, f_bool)
            && tb != fb
        {
            self.write("return ");
            self.write_condition(condition_node, if tb { negate } else { !negate });
            self.write(";\n");
            return true;
        }
        self.write("return (");
        self.write_condition(condition_node, negate);
        self.write(") ? ");
        self.write_value(t_val);
        self.write(" : ");
        self.write_value(f_val);
        self.write(";\n");
        true
    }

    fn visit_switch_node(&mut self, node: NodeId) {
        let switch = &self.graph.nodes[node];
        for statement in switch.statements.iter().take(switch.statements.len().saturating_sub(1)) {
            self.write_statement(statement);
        }
        self.write_indent();
        self.write("switch (");
        if let Some(Statement::Switch { value, .. }) = switch.statements.last() {
            self.write_value(value);
        }
        self.write(") {\n");
        let follow = switch.switch_follow;
        self.switch_follow.push(follow);

        let mut default = switch.switch_default;
        for (index, (_, target)) in switch.switch_cases.iter().enumerate() {
            let target = *target;
            if self.visited[target] {
                continue;
            }
            self.indentation += 4;
            for (value, case_target) in &switch.switch_cases {
                if *case_target == target {
                    self.write_indent();
                    self.write("case ");
                    self.write_i64(i64::from(*value));
                    self.write(":\n");
                }
            }
            self.next_case = switch.switch_cases.get(index + 1).map(|(_, target)| *target);
            if default == Some(target) {
                self.write_indent();
                self.write("default:\n");
                default = None;
            }
            self.indentation += 4;
            self.visit_node(target);
            if self.need_break {
                self.write_indent();
                self.write("break;\n");
            } else {
                self.need_break = true;
            }
            self.indentation -= 8;
        }
        if let Some(default) = default
            && Some(default) != follow
        {
            self.indentation += 4;
            self.write_indent();
            self.write("default:\n");
            self.indentation += 4;
            self.visit_node(default);
            self.indentation -= 8;
        }
        self.write_indent();
        self.write("}\n");
        self.switch_follow.pop();
        if let Some(follow) = follow {
            self.visit_node(follow);
        }
    }

    fn visit_statement_node(&mut self, node: NodeId) {
        self.visit_statements(node);
        let successors = &self.graph.nodes[node].successors;
        if let [successor] = successors.as_slice() {
            if Some(*successor) == *self.loop_follow.last().unwrap_or(&None) {
                self.write_indent();
                self.write("break;\n");
            } else if Some(*successor) == self.next_case {
                self.need_break = false;
            } else {
                self.visit_node(*successor);
            }
        }
    }

    fn visit_try_node(&mut self, node: NodeId) {
        let try_node = &self.graph.nodes[node];
        if self.enhanced
            && let Some(start) = try_node.wrapped
        {
            let declarations = self.graph.nodes[start].declarations.clone();
            for variable in declarations {
                if self.should_defer_declaration(start, variable) {
                    continue;
                }
                self.write_declaration(variable);
            }
        }
        self.write_indent();
        self.write("try {\n");
        self.indentation += 4;
        self.try_follow.push(try_node.try_follow);
        if let Some(start) = try_node.wrapped {
            self.visit_node(start);
        }
        self.indentation -= 4;
        self.write_indent();
        self.write("}");
        for catch in &try_node.catches {
            self.visit_node(*catch);
        }
        self.write("\n");
        let follow = self.try_follow.pop().flatten();
        if let Some(follow) = follow {
            self.visit_node(follow);
        }
    }

    fn visit_catch_node(&mut self, node: NodeId) {
        let catch = &self.graph.nodes[node];
        self.write(" catch (");
        let start = catch.wrapped.expect("catch wrapper has a start");
        let statements = self.graph.nodes[start].statements.as_slice();
        if catch.catch_types.len() > 1 {
            let types = catch
                .catch_types
                .iter()
                .map(|ty| self.format_descriptor(ty))
                .collect::<Vec<_>>()
                .join(" | ");
            self.write(&types);
            if let Some(Statement::MoveException { lhs, .. }) = statements.first() {
                self.write(" ");
                self.write_variable_name(*lhs);
            } else if self.enhanced {
                self.write(" e");
            }
        } else if let Some(Statement::MoveException { lhs, ty }) = statements.first() {
            let variable_type = self
                .graph
                .variables
                .get(*lhs)
                .and_then(|variable| variable.ty.as_deref());
            let ty = catch
                .catch_types
                .first()
                .map(String::as_str)
                .or(ty.as_deref())
                .or(variable_type)
                .unwrap_or("Ljava/lang/Throwable;");
            self.write(&self.format_descriptor(ty));
            self.write(" ");
            self.write_variable_name(*lhs);
        } else {
            let provenance_type = catch.catch_order.and_then(|token| {
                let source = (token >> 32) as usize;
                let handler = token as u32 as usize;
                self.graph.nodes.get(source).and_then(|node| {
                    node.catch_handlers
                        .get(handler)
                        .and_then(|(_, ty)| ty.as_deref())
                })
            });
            self.write(&self.format_descriptor(
                provenance_type
                    .or_else(|| catch.catch_types.first().map(String::as_str))
                    .unwrap_or("Ljava/lang/Throwable;"),
            ));
            if self.enhanced {
                self.write(" e");
            }
        }
        self.write(") {\n");
        self.indentation += 4;
        self.visit_node(start);
        self.indentation -= 4;
        self.write_indent();
        self.write("}");
    }

    fn visit_statements(&mut self, node: NodeId) {
        let statements = &self.graph.nodes[node].statements;
        let first = usize::from(
            self.graph.nodes[node].catch_target
                && !self.graph.nodes[node].catch_source
                && matches!(statements.first(), Some(Statement::MoveException { .. })),
        );
        let slice = &statements[first..];
        let mut idx = 0;
        while idx < slice.len() {
            let orig_idx = first + idx;
            if self.skipped_for_step == Some((node, orig_idx)) {
                idx += 1;
                continue;
            }
            if self.enhanced
                && idx + 1 == slice.len()
                && let [loop_succ] = self.graph.nodes[node].successors.as_slice()
                && !self.visited[*loop_succ]
                && self.graph.nodes[*loop_succ].kind == BlockKind::Loop
                && self.graph.nodes[*loop_succ].loop_kind == LoopKind::PreTest
                && self.graph.nodes[*loop_succ].declarations.is_empty()
                && let Statement::Assign {
                    lhs: Some(init_var),
                    rhs: init_rhs,
                    kind,
                } = &slice[idx]
                && !self.is_declared(*init_var)
                && !self.graph.variables.is_declared(*init_var)
                && let Some(wrapped) = self.graph.nodes[*loop_succ].wrapped
            {
                if let Value::Invoke(invoke) = init_rhs
                    && invoke.method.name == "iterator"
                    && invoke.arguments.is_empty()
                    && invoke.method.return_type == "Ljava/util/Iterator;"
                    && !matches!(&invoke.base, Value::BaseClass { .. })
                {
                    let loop_node = &self.graph.nodes[*loop_succ];
                    let mut true_branch = loop_node.branch_true;
                    let mut false_branch = loop_node.branch_false;
                    let mut negate = false;
                    if true_branch == loop_node.loop_follow {
                        negate = true;
                        std::mem::swap(&mut true_branch, &mut false_branch);
                    }
                    if let Some(body_node) = true_branch
                        && self
                            .detect_foreach_loop(*loop_succ, wrapped, negate, body_node, *init_var)
                            .is_some()
                    {
                        self.pending_foreach_init =
                            Some((*loop_succ, *init_var, invoke.base.clone()));
                        idx += 1;
                        continue;
                    }
                }
                if *kind != AssignKind::MoveResult
                    && self
                        .detect_for_loop_step(*loop_succ, wrapped, *init_var)
                        .is_some()
                {
                    self.pending_for_init = Some((*loop_succ, *init_var, init_rhs.clone()));
                    idx += 1;
                    continue;
                }
            }
            if self.enhanced
                && idx + 1 < slice.len()
                && let Statement::Assign {
                    lhs: Some(lhs),
                    rhs: Value::NewArray(new_array),
                    ..
                } = &slice[idx]
                && let Statement::FillArray {
                    array: Value::Variable(fill_var),
                    element_width,
                    size,
                    data,
                } = &slice[idx + 1]
                && lhs == fill_var
            {
                self.write_indent();
                self.write_variable(*lhs);
                self.write(" = new ");
                self.write(&self.format_descriptor(&new_array.ty));
                self.write(" {");
                self.write(&format_array_data(*element_width, *size, data));
                self.write("};\n");
                idx += 2;
                continue;
            }
            self.write_statement(&slice[idx]);
            idx += 1;
        }
    }

    fn detect_for_loop_step(
        &self,
        loop_node: NodeId,
        wrapped: NodeId,
        init_var: VariableId,
    ) -> Option<(NodeId, usize, Value)> {
        let latch = self.graph.nodes[loop_node].latch?;
        let latch_block = &self.graph.nodes[latch];
        if latch_block.kind != BlockKind::Statement || latch_block.statements.is_empty() {
            return None;
        }
        let step_idx = latch_block.statements.len() - 1;
        let Statement::Assign {
            lhs: Some(step_lhs),
            rhs: step_rhs @ Value::Binary(binary),
            kind,
        } = &latch_block.statements[step_idx]
        else {
            return None;
        };
        if *step_lhs != init_var
            || *kind == AssignKind::MoveResult
            || !matches!(&binary.left, Value::Variable(v) if *v == init_var)
        {
            return None;
        }
        let cond_stmt = self.graph.nodes[wrapped]
            .statements
            .iter()
            .rev()
            .find(|s| matches!(s, Statement::Condition { .. }))?;
        let mut cond_vars = Vec::new();
        cond_stmt.collect_used_variables_vec(&mut cond_vars);
        if !cond_vars.contains(&init_var) {
            return None;
        }
        if let Some(follow) = self.graph.nodes[loop_node].loop_follow {
            let mut used = Vec::new();
            for (nid, nblock) in self.graph.nodes.iter().enumerate() {
                if !nblock.active
                    || self.graph.nodes[loop_node].loop_nodes.contains(&nid)
                    || self.graph.nodes[nid].rpo < self.graph.nodes[follow].rpo
                {
                    continue;
                }
                for stmt in &nblock.statements {
                    used.clear();
                    stmt.collect_used_variables_vec(&mut used);
                    if used.contains(&init_var) {
                        return None;
                    }
                }
            }
        }
        Some((latch, step_idx, step_rhs.clone()))
    }

    fn detect_foreach_loop(
        &self,
        _loop_node: NodeId,
        wrapped: NodeId,
        negate: bool,
        body_node: NodeId,
        iter_var: VariableId,
    ) -> Option<(VariableId, String)> {
        if self.conditions[wrapped].is_some() {
            return None;
        }
        let cond_stmt = self.graph.nodes[wrapped]
            .statements
            .iter()
            .rev()
            .find(|s| matches!(s, Statement::Condition { .. }))?;
        let Statement::Condition { op, left, right } = cond_stmt else {
            return None;
        };
        if right.is_some() {
            return None;
        }
        let mut effective_op = *op;
        if self.negated_condition_leaves[wrapped] {
            effective_op = effective_op.negate();
        }
        if negate {
            effective_op = effective_op.negate();
        }
        if effective_op != BinaryOp::NotEqual {
            return None;
        }
        let Value::Invoke(has_next) = left else {
            return None;
        };
        if has_next.method.name != "hasNext"
            || !has_next.arguments.is_empty()
            || !matches!(&has_next.base, Value::Variable(v) if *v == iter_var)
        {
            return None;
        }
        let body_block = &self.graph.nodes[body_node];
        if (body_block.catch_target && !body_block.catch_source)
            || !body_block.declarations.is_empty()
            || body_block.statements.is_empty()
        {
            return None;
        }
        let Statement::Assign {
            lhs: Some(elem_var),
            rhs: elem_rhs,
            ..
        } = &body_block.statements[0]
        else {
            return None;
        };
        if self.is_declared(*elem_var) || self.graph.variables.is_declared(*elem_var) {
            return None;
        }
        let elem_ty = match elem_rhs {
            Value::CheckCast(cast) => {
                let Value::Invoke(next_call) = &cast.value else {
                    return None;
                };
                if next_call.method.name != "next"
                    || !next_call.arguments.is_empty()
                    || !matches!(&next_call.base, Value::Variable(v) if *v == iter_var)
                {
                    return None;
                }
                cast.descriptor.clone()
            }
            Value::Invoke(next_call)
                if next_call.method.name == "next"
                    && next_call.arguments.is_empty()
                    && matches!(&next_call.base, Value::Variable(v) if *v == iter_var) =>
            {
                self.resolved_var_descriptor(
                    self.graph
                        .variables
                        .get(*elem_var)
                        .and_then(|v| v.ty.as_deref()),
                )
            }
            _ => return None,
        };
        let mut used = Vec::new();
        for (nid, nblock) in self.graph.nodes.iter().enumerate() {
            if nid == wrapped {
                continue;
            }
            for (sidx, stmt) in nblock.statements.iter().enumerate() {
                if nid == body_node && sidx == 0 {
                    continue;
                }
                used.clear();
                stmt.collect_used_variables_vec(&mut used);
                if used.contains(&iter_var) {
                    if let Statement::Assign {
                        lhs: Some(lhs),
                        rhs: Value::Invoke(inv),
                        ..
                    } = stmt
                        && *lhs == iter_var
                        && inv.method.name == "iterator"
                    {
                        continue;
                    }
                    return None;
                }
            }
        }
        Some((*elem_var, elem_ty))
    }

    fn write_for_step(&mut self, lhs: VariableId, rhs: &Value) {
        if let Value::Binary(binary) = rhs
            && matches!(&binary.left, Value::Variable(v) if *v == lhs)
        {
            self.write_variable(lhs);
            if matches!(binary.op, BinaryOp::Add | BinaryOp::Subtract)
                && matches!(
                    &binary.right,
                    Value::Constant(Constant {
                        value: ConstantValue::Integer { value: 1, .. },
                    })
                )
            {
                self.write(if binary.op == BinaryOp::Add { "++" } else { "--" });
            } else {
                self.write(" ");
                self.write(self.binary_operator_str(binary.op));
                self.write("= ");
                self.write_value(&binary.right);
            }
        }
    }

    fn write_statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Assign { lhs, rhs, kind } => {
                if let Some(lhs) = lhs {
                    if *kind == AssignKind::Move && matches!(rhs, Value::Variable(rhs) if rhs == lhs)
                    {
                        return;
                    }
                    if *kind != AssignKind::MoveResult && self.write_in_place(*lhs, rhs) {
                        return;
                    }
                    if self.should_skip_constructor(rhs) {
                        return;
                    }
                    self.write_indent();
                    if is_void_super_invoke(rhs) {
                        self.mark_declared(*lhs);
                    } else {
                        self.write_variable(*lhs);
                        self.write(" = ");
                    }
                    self.write_value(rhs);
                    self.write(";\n");
                } else if !self.should_skip_constructor(rhs) {
                    self.write_indent();
                    self.write_value(rhs);
                    self.write(";\n");
                }
            }
            Statement::ArrayStore(store) => {
                self.write_indent();
                self.write_value(&store.array);
                self.write("[");
                self.write_value(&store.index);
                self.write("] = ");
                self.write_value(&store.value);
                self.write(";\n");
            }
            Statement::StaticPut { value, field } => {
                self.write_indent();
                self.write_static_field(field, false);
                self.write(" = ");
                self.write_value(value);
                self.write(";\n");
            }
            Statement::InstancePut(store) => {
                self.write_indent();
                self.write_value(&store.object);
                self.write(".");
                self.write_field_name(&store.field, false);
                self.write(" = ");
                self.write_value(&store.value);
                self.write(";\n");
            }
            Statement::Return(value) => {
                self.write_indent();
                self.write("return");
                if let Some(value) = value {
                    self.write(" ");
                    self.write_value(value);
                }
                self.write(";\n");
            }
            Statement::Nop | Statement::Switch { .. } | Statement::Condition { .. } => {}
            Statement::FillArray {
                array,
                element_width,
                size,
                data,
            } => {
                self.write_indent();
                self.write_value(array);
                self.write(" = {");
                self.write(&format_array_data(*element_width, *size, data));
                self.write("};\n");
            }
            Statement::MoveException { lhs, ty } => {
                self.write(
                    &descriptor_to_java(ty.as_deref().unwrap_or("Ljava/lang/Throwable;")),
                );
                self.write(" ");
                self.write_variable_name(*lhs);
            }
            Statement::MonitorEnter(value) => {
                self.write_indent();
                self.write("synchronized(");
                self.write_value(value);
                self.write(") {\n");
                self.indentation += 4;
            }
            Statement::MonitorExit(_) => {
                self.indentation = self.indentation.saturating_sub(4);
                self.write_indent();
                self.write("}\n");
            }
            Statement::Throw(value) => {
                self.write_indent();
                self.write("throw ");
                self.write_value(value);
                self.write(";\n");
            }
        }
    }

    fn write_in_place(&mut self, lhs: VariableId, rhs: &Value) -> bool {
        let Value::Binary(binary) = rhs else {
            return false;
        };
        if !matches!(&binary.left, Value::Variable(variable) if *variable == lhs) {
            return false;
        }
        self.write_indent();
        self.write_variable(lhs);
        if matches!(binary.op, BinaryOp::Add | BinaryOp::Subtract)
            && matches!(
                &binary.right,
                Value::Constant(Constant {
                    value: ConstantValue::Integer { value: 1, .. },
                })
            )
        {
            self.write(if binary.op == BinaryOp::Add { "++;\n" } else { "--;\n" });
        } else {
            self.write(" ");
            self.write(self.binary_operator_str(binary.op));
            self.write("= ");
            self.write_value(&binary.right);
            self.write(";\n");
        }
        true
    }

    fn write_wrapped_condition(&mut self, mut node: NodeId, negate: bool) {
        while self.conditions[node].is_none()
            && !self.graph.nodes[node]
                .statements
                .iter()
                .any(|statement| matches!(statement, Statement::Condition { .. }))
        {
            let Some(wrapped) = self.graph.nodes[node].wrapped else {
                break;
            };
            node = wrapped;
        }
        self.write_condition(node, negate);
    }

    fn write_condition(&mut self, node: NodeId, negate: bool) {
        if let Some(mut condition) = self.conditions[node].take() {
            if negate {
                self.negate_condition_tree(&mut condition);
            }
            self.write_condition_tree(&mut condition);
            self.conditions[node] = Some(condition);
            return;
        }
        if negate {
            self.toggle_condition_leaf(node);
        }
        self.write_condition_leaf(node);
    }

    fn write_condition_leaf(&mut self, node: NodeId) {
        let condition = self.graph.nodes[node]
            .statements
            .iter()
            .rev()
            .find(|statement| matches!(statement, Statement::Condition { .. }));
        let Some(Statement::Condition { op, left, right }) = condition else {
            self.write("false");
            return;
        };
        let op = if self.negated_condition_leaves[node] {
            op.negate()
        } else {
            *op
        };
        if let Some(right) = right {
            if self.enhanced
                && let Value::Binary(binary) = left
                && binary.op == BinaryOp::Compare
                && matches!(
                    right,
                    Value::Constant(Constant {
                        value: ConstantValue::Integer { value: 0, .. },
                    })
                )
            {
                self.write_value(&binary.left);
                self.write(" ");
                self.write(self.binary_operator_str(op));
                self.write(" ");
                self.write_value(&binary.right);
            } else {
                self.write_value(left);
                self.write(" ");
                self.write(self.binary_operator_str(op));
                self.write(" ");
                self.write_value(right);
            }
        } else if let Value::Binary(binary) = left
            && binary.op == BinaryOp::Compare
        {
            self.write_value(&binary.left);
            self.write(" ");
            self.write(self.binary_operator_str(op));
            self.write(" ");
            self.write_value(&binary.right);
        } else if value_type(left, self.graph).is_some_and(|ty| ty == "Z") {
            if op == BinaryOp::Equal {
                self.write("!");
            }
            self.write_value(left);
        } else {
            self.write_value(left);
            self.write(" ");
            self.write(self.binary_operator_str(op));
            self.write(" ");
            if value_type(left, self.graph)
                .is_some_and(|ty| matches!(ty, "V" | "B" | "S" | "C" | "I" | "J" | "F" | "D"))
            {
                self.write("0");
            } else {
                self.write("null");
            }
        }
    }

    fn negate_condition_tree(&mut self, condition: &mut ConditionTree) {
        match condition {
            ConditionTree::Leaf(node) => self.toggle_condition_leaf(*node),
            ConditionTree::Compound {
                left,
                right,
                is_and,
                ..
            } => {
                *is_and = !*is_and;
                self.negate_condition_tree(left);
                self.negate_condition_tree(right);
            }
        }
    }

    fn toggle_condition_leaf(&mut self, node: NodeId) {
        self.negated_condition_leaves[node] = !self.negated_condition_leaves[node];
    }

    fn write_condition_tree(&mut self, condition: &mut ConditionTree) {
        match condition {
            ConditionTree::Leaf(node) => self.write_condition_leaf(*node),
            ConditionTree::Compound {
                left,
                right,
                is_and,
                is_not,
            } => {
                if *is_not {
                    self.negate_condition_tree(left);
                }
                self.write("(");
                self.write_condition_tree(left);
                self.write(if *is_and { ") && (" } else { ") || (" });
                self.write_condition_tree(right);
                self.write(")");
            }
        }
    }

    fn write_value(&mut self, value: &Value) {
        match value {
            Value::Variable(variable) => self.write_variable(*variable),
            Value::Constant(constant) => self.write_constant(constant),
            Value::BaseClass { name } => {
                self.write(name);
                if self.enhanced && name != "super" {
                    self.write(".class");
                }
            }
            Value::NewInstance { ty } => {
                self.write("new ");
                self.write(&self.format_descriptor(ty));
            }
            Value::Invoke(invoke) => self.write_invoke(
                invoke.kind,
                &invoke.method,
                &invoke.base,
                &invoke.arguments,
            ),
            Value::CheckCast(cast) => {
                if self.enhanced
                    && value_type(&cast.value, self.graph) == Some(cast.descriptor.as_str())
                {
                    self.write_value(&cast.value);
                } else {
                    self.write("((");
                    self.write(&cast.ty);
                    self.write(") ");
                    self.write_value(&cast.value);
                    self.write(")");
                }
            }
            Value::ArrayLoad(load) => {
                self.write_value(&load.array);
                self.write("[");
                self.write_value(&load.index);
                self.write("]");
            }
            Value::ArrayLength(array) => {
                self.write_value(array);
                self.write(".length");
            }
            Value::NewArray(array) => {
                self.write("new ");
                self.write(&self.format_descriptor(
                    array.ty.strip_prefix('[').unwrap_or(&array.ty),
                ));
                self.write("[");
                self.write_value(&array.size);
                self.write("]");
            }
            Value::FilledArray(array) => {
                self.write("new ");
                self.write(&self.format_descriptor(&array.ty));
                self.write(" {");
                for (index, argument) in array.arguments.iter().enumerate() {
                    if index != 0 {
                        self.write(", ");
                    }
                    self.write_value(argument);
                }
                if self.enhanced {
                    self.write("}");
                } else {
                    self.write("})");
                }
            }
            Value::Binary(binary) => {
                let parenthesized = binary.op != BinaryOp::Compare;
                if parenthesized {
                    self.write("(");
                }
                self.write_value(&binary.left);
                self.write(" ");
                self.write(self.binary_operator_str(binary.op));
                self.write(" ");
                if binary.op == BinaryOp::InstanceOf
                    && let Value::BaseClass { name } = &binary.right
                {
                    self.write(name);
                } else {
                    self.write_value(&binary.right);
                }
                if parenthesized {
                    self.write(")");
                }
            }
            Value::Unary(unary) => {
                if self.enhanced
                    && unary.op == UnaryOp::Cast
                    && value_type(&unary.value, self.graph) == Some(unary.ty)
                {
                    self.write_value(&unary.value);
                } else {
                    self.write("(");
                    match unary.op {
                        UnaryOp::Negate => self.write("- "),
                        UnaryOp::Not => self.write("~ "),
                        UnaryOp::Cast => {
                            self.write(unary.cast.unwrap_or(""));
                            self.write(" ");
                        }
                    }
                    self.write_value(&unary.value);
                    self.write(")");
                }
            }
            Value::InstanceField(field) => {
                self.write_value(&field.object);
                self.write(".");
                self.write_field_name(&field.field, false);
            }
            Value::StaticField(field) => self.write_static_field(field, false),
        }
    }

    fn write_invoke(
        &mut self,
        kind: InvokeKind,
        method: &MethodReference,
        base: &Value,
        arguments: &[Value],
    ) {
        if self.enhanced
            && method.class == "Ljava/lang/StringBuilder;"
            && method.name == "toString"
            && arguments.is_empty()
            && let Some(parts) = Self::extract_string_builder_parts(base)
        {
            self.write("(");
            for (index, part) in parts.iter().enumerate() {
                if index != 0 {
                    self.write(" + ");
                }
                self.write_value(part);
            }
            self.write(")");
            return;
        }
        let constructor = method.name == "<init>";
        if constructor && self.is_this(base) && method.class != self.current_class {
            self.this_is_super = true;
            self.write("super");
        } else if let Value::BaseClass { name } = base
            && matches!(kind, InvokeKind::Static | InvokeKind::Super)
        {
            self.write(name);
        } else {
            self.write_value(base);
        }
        if !constructor {
            self.write(".");
            let start = self.output.source.len();
            self.write(&method.name);
            self.output.member_references.push(MemberReference {
                start,
                end: self.output.source.len(),
                kind: MemberKind::Method,
                class: method.class.clone(),
                name: method.name.clone(),
                descriptor: method_descriptor(method),
                declaration: false,
                synthetic: false,
            });
        }
        self.write("(");
        for (index, argument) in arguments.iter().enumerate() {
            if index != 0 {
                self.write(", ");
            }
            self.write_value(argument);
        }
        self.write(")");
    }

    fn should_skip_constructor(&self, value: &Value) -> bool {
        matches!(
            value,
            Value::Invoke(invoke) if self.constructor
                && invoke.method.name == "<init>"
                && invoke.arguments.is_empty()
                && self.is_this(&invoke.base)
        )
    }

    fn extract_string_builder_parts(value: &Value) -> Option<Vec<&Value>> {
        let Value::Invoke(invoke) = value else {
            return None;
        };
        if invoke.method.class != "Ljava/lang/StringBuilder;" {
            return None;
        }
        if invoke.method.name == "<init>" {
            if !matches!(&invoke.base, Value::NewInstance { ty } if ty == "Ljava/lang/StringBuilder;") {
                return None;
            }
            if invoke.arguments.is_empty() {
                return Some(Vec::new());
            }
            if invoke.arguments.len() == 1 {
                return match invoke.method.parameters.first().map(String::as_str) {
                    Some("I") => Some(Vec::new()),
                    Some("Ljava/lang/String;") => Some(vec![&invoke.arguments[0]]),
                    _ => None,
                };
            }
            return None;
        }
        if invoke.method.name == "append" && invoke.arguments.len() == 1 {
            let mut parts = Self::extract_string_builder_parts(&invoke.base)?;
            parts.push(&invoke.arguments[0]);
            return Some(parts);
        }
        None
    }

    fn is_this(&self, value: &Value) -> bool {
        matches!(
            value,
            Value::Variable(variable) if self.graph.variables.is_this(*variable)
        )
    }

    fn write_constant(&mut self, constant: &Constant) {
        match &constant.value {
            ConstantValue::String(value) => self.write(&escape_java_string(value)),
            ConstantValue::Integer { value, ty: "Z" } => {
                self.write(if *value == 0 { "false" } else { "true" });
            }
            ConstantValue::Integer { ty: "L", .. } if self.enhanced => {
                self.write("null");
            }
            ConstantValue::Integer { value, ty: "C" } if self.enhanced => {
                self.write(&format_java_char(*value as u16));
            }
            ConstantValue::Integer { value, ty: "F" } if self.enhanced => {
                let bits = (*value as i32) as u32;
                self.write(&format_java_f32(f32::from_bits(bits)));
            }
            ConstantValue::Integer { value, ty: "D" } if self.enhanced => {
                let bits = *value as u64;
                self.write(&format_java_f64(f64::from_bits(bits)));
            }
            ConstantValue::Integer { value, ty: "J" }
                if self.enhanced && (*value > i64::from(i32::MAX) || *value < i64::from(i32::MIN)) =>
            {
                self.write_i64(*value);
                self.write("L");
            }
            ConstantValue::Integer { value, .. } => self.write_i64(*value),
        }
    }

    fn write_variable(&mut self, variable: VariableId) {
        let info = self
            .graph
            .variables
            .get(variable)
            .expect("IR references a registered variable");
        if self.graph.variables.is_this(variable) {
            self.write(if self.this_is_super { "super" } else { "this" });
            return;
        }
        if self.graph.variables.is_super(variable) {
            self.write("super");
            return;
        }
        if self.graph.variables.is_declared(variable) {
            self.write("p");
            match self.graph.variables.name(variable) {
                Some(name) => self.write(name),
                None => {
                    if let VariableId::Register(register) = variable {
                        self.write_u64(u64::from(register));
                    }
                }
            }
            return;
        }
        if !self.is_declared(variable) {
            self.mark_declared(variable);
            self.write(&self.format_descriptor(&self.resolved_var_descriptor(info.ty.as_deref())));
            self.write(" ");
        }
        self.write_variable_name(variable);
    }

    fn format_descriptor(&self, descriptor: &str) -> String {
        if self.enhanced {
            descriptor_to_java_enhanced(descriptor)
        } else {
            descriptor_to_java(descriptor)
        }
    }

    fn resolved_var_descriptor(&self, ty: Option<&str>) -> String {
        if self.enhanced {
            match ty {
                None | Some("V" | "U" | "W" | "O" | "unknownType") => "Object".to_owned(),
                Some(other) => other.to_owned(),
            }
        } else {
            ty.unwrap_or("unknownType").to_owned()
        }
    }

    fn binary_operator_str(&self, op: BinaryOp) -> &'static str {
        if self.enhanced && op == BinaryOp::UnsignedShiftRight {
            ">>>"
        } else {
            binary_operator(op)
        }
    }

    fn is_declared(&self, variable: VariableId) -> bool {
        self.graph
            .variables
            .dense_index(variable)
            .is_some_and(|index| self.declared[index])
    }

    fn mark_declared(&mut self, variable: VariableId) {
        if let Some(index) = self.graph.variables.dense_index(variable) {
            self.declared[index] = true;
        }
    }

    fn write_variable_name(&mut self, variable: VariableId) {
        self.output.source.push('v');
        match self.graph.variables.name(variable) {
            Some(name) => self.output.source.push_str(name),
            None => self.output.source.push_str("unknown"),
        }
        self.mark_declared(variable);
    }

    fn write_declaration(&mut self, variable: VariableId) {
        if self.is_declared(variable) {
            return;
        }
        let info = self
            .graph
            .variables
            .get(variable)
            .expect("declaration references a registered variable");
        self.write_indent();
        self.write(&self.format_descriptor(&self.resolved_var_descriptor(info.ty.as_deref())));
        self.write(" ");
        self.write_variable_name(variable);
        self.write(";\n");
    }

    fn write_static_field(&mut self, field: &FieldReference, declaration: bool) {
        self.write(&self.format_descriptor(field.class.as_ref()));
        self.write(".");
        self.write_field_name(field, declaration);
    }

    fn write_field_name(&mut self, field: &FieldReference, declaration: bool) {
        let start = self.output.source.len();
        self.write(field.name.as_ref());
        self.output.member_references.push(MemberReference {
            start,
            end: self.output.source.len(),
            kind: MemberKind::Field,
            class: field.class.to_string(),
            name: field.name.to_string(),
            descriptor: field.ty.to_string(),
            declaration,
            synthetic: false,
        });
    }

    fn write_indent(&mut self) {
        self.output
            .source
            .extend(std::iter::repeat_n(' ', self.indentation));
    }

    fn write(&mut self, value: &str) {
        self.output.source.push_str(value);
    }

    fn write_u64(&mut self, mut value: u64) {
        let mut digits = [0_u8; 20];
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        // Decimal digits are always valid UTF-8.
        self.write(unsafe { std::str::from_utf8_unchecked(&digits[start..]) });
    }

    fn write_i64(&mut self, value: i64) {
        if value < 0 {
            self.write("-");
        }
        self.write_u64(value.unsigned_abs());
    }
}

pub fn binary_operator(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Compare => "cmp",
        BinaryOp::Add => "+",
        BinaryOp::Subtract => "-",
        BinaryOp::Multiply => "*",
        BinaryOp::Divide => "/",
        BinaryOp::Remainder => "%",
        BinaryOp::And => "&",
        BinaryOp::Or => "|",
        BinaryOp::Xor => "^",
        BinaryOp::ShiftLeft => "<<",
        BinaryOp::ShiftRight | BinaryOp::UnsignedShiftRight => ">>",
        BinaryOp::Equal => "==",
        BinaryOp::NotEqual => "!=",
        BinaryOp::Less => "<",
        BinaryOp::GreaterEqual => ">=",
        BinaryOp::Greater => ">",
        BinaryOp::LessEqual => "<=",
        BinaryOp::InstanceOf => "instanceof",
    }
}

pub fn escape_java_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '\'' | '"' | '\\' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '\r' => escaped.push_str("\\r"),
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            ' '..='~' => escaped.push(character),
            character => escaped.push_str(&format!("\\u{:04x}", u32::from(character))),
        }
    }
    escaped.push('"');
    escaped
}

fn value_type<'a>(value: &'a Value, graph: &'a Graph) -> Option<&'a str> {
    match value {
        Value::Variable(variable) => graph
            .variables
            .get(*variable)
            .and_then(|variable| variable.ty.as_deref()),
        Value::Constant(constant) => Some(constant.ty()),
        Value::NewInstance { ty } => Some(ty),
        Value::NewArray(array) => Some(&array.ty),
        Value::FilledArray(array) => Some(&array.ty),
        Value::CheckCast(cast) => Some(&cast.descriptor),
        Value::Binary(binary) => Some(binary.ty),
        Value::Unary(unary) => Some(unary.ty),
        Value::Invoke(invoke) => Some(&invoke.method.return_type),
        Value::ArrayLength(_) => Some("I"),
        Value::ArrayLoad(load) => value_type(&load.array, graph)
            .and_then(|ty| ty.strip_prefix('['))
            .or(load.ty),
        Value::InstanceField(field) => Some(field.field.ty.as_ref()),
        Value::StaticField(field) => Some(field.ty.as_ref()),
        Value::BaseClass { .. } => None,
    }
}

fn is_void_super_invoke(value: &Value) -> bool {
    matches!(
        value,
        Value::Invoke(invoke)
            if invoke.kind == InvokeKind::Super && invoke.method.return_type == "V"
    )
}

fn method_descriptor(method: &MethodReference) -> String {
    format!(
        "({}){}",
        method.parameters.join(" "),
        method.return_type
    )
}

fn format_array_data(element_width: u16, size: u32, data: &[u8]) -> String {
    let width = match element_width {
        1 | 2 | 4 | 8 => usize::from(element_width),
        _ => 1,
    };
    let count = usize::try_from(size).unwrap_or(usize::MAX);
    data.chunks_exact(width)
        .take(count)
        .map(|bytes| match width {
            1 => i8::from_le_bytes([bytes[0]]).to_string(),
            2 => i16::from_le_bytes([bytes[0], bytes[1]]).to_string(),
            4 => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).to_string(),
            8 => format_dad_f64(f64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_dad_f64(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value == f64::INFINITY {
        return "inf".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-inf".to_owned();
    }
    let mut rendered = format!("{value:?}");
    if let Some(exponent) = rendered.find('e') {
        let has_sign = matches!(rendered.as_bytes()[exponent + 1], b'+' | b'-');
        let exponent_digits = rendered.len() - exponent - 1 - usize::from(has_sign);
        if !has_sign {
            rendered.insert(exponent + 1, '+');
        }
        if exponent_digits < 2 {
            rendered.insert(exponent + 2, '0');
        }
    }
    rendered
}

pub fn format_java_f32(value: f32) -> String {
    if value.is_nan() {
        return "Float.NaN".to_owned();
    }
    if value == f32::INFINITY {
        return "Float.POSITIVE_INFINITY".to_owned();
    }
    if value == f32::NEG_INFINITY {
        return "Float.NEGATIVE_INFINITY".to_owned();
    }
    format!("{value:?}f")
}

pub fn format_java_f64(value: f64) -> String {
    if value.is_nan() {
        return "Double.NaN".to_owned();
    }
    if value == f64::INFINITY {
        return "Double.POSITIVE_INFINITY".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "Double.NEGATIVE_INFINITY".to_owned();
    }
    format!("{value:?}")
}

pub fn format_java_char(unit: u16) -> String {
    match unit {
        0x08 => "'\\b'".to_owned(),
        0x09 => "'\\t'".to_owned(),
        0x0a => "'\\n'".to_owned(),
        0x0c => "'\\f'".to_owned(),
        0x0d => "'\\r'".to_owned(),
        0x27 => "'\\''".to_owned(),
        0x5c => "'\\\\'".to_owned(),
        0x20..=0x7e => format!("'{}'", char::from(unit as u8)),
        _ => format!("'\\u{unit:04x}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::{format_dad_f64, format_java_char, format_java_f32, format_java_f64};
    use crate::types::descriptor_to_java_enhanced;

    #[test]
    fn dad_f64_format_preserves_float_identity_bits_ut() {
        assert_eq!(format_dad_f64(-90.0), "-90.0");
        assert_eq!(format_dad_f64(1228.8), "1228.8");
        assert_eq!(format_dad_f64(7.411e-320), "7.411e-320");
        assert_eq!(format_dad_f64(1.0e16), "1e+16");
        assert_eq!(format_dad_f64(1.0e-5), "1e-05");
        assert_eq!(format_dad_f64(-0.0), "-0.0");
        assert_eq!(format_dad_f64(f64::NAN), "nan");
        assert_eq!(format_dad_f64(f64::INFINITY), "inf");
        assert_eq!(format_dad_f64(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn java_float_and_double_format_renders_valid_literals_ut() {
        assert_eq!(format_java_f32(1.0), "1.0f");
        assert_eq!(format_java_f32(-1.0), "-1.0f");
        assert_eq!(format_java_f32(0.1), "0.1f");
        assert_eq!(format_java_f32(f32::NAN), "Float.NaN");
        assert_eq!(format_java_f32(f32::INFINITY), "Float.POSITIVE_INFINITY");
        assert_eq!(
            format_java_f32(f32::NEG_INFINITY),
            "Float.NEGATIVE_INFINITY"
        );
        assert_eq!(format_java_f64(1000.0), "1000.0");
        assert_eq!(format_java_f64(f64::NAN), "Double.NaN");
        assert_eq!(format_java_f64(f64::INFINITY), "Double.POSITIVE_INFINITY");
        assert_eq!(
            format_java_f64(f64::NEG_INFINITY),
            "Double.NEGATIVE_INFINITY"
        );
        assert_eq!(format_java_char(47), "'/'");
        assert_eq!(format_java_char(10), "'\\n'");
        assert_eq!(format_java_char(39), "'\\''");
        assert_eq!(descriptor_to_java_enhanced("Ljava/lang/String;"), "String");
        assert_eq!(
            descriptor_to_java_enhanced("Ljava/lang/ref/SoftReference;"),
            "java.lang.ref.SoftReference"
        );
    }
}
