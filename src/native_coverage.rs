//! Method-level reconstruction coverage, with bounded aggregate diagnostics.
use anyhow::{Context, Result};
use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
use serde::Serialize;
use std::{collections::BTreeMap, io::Write, path::Path};

const MAX_REASONS: usize = 256;
const MAX_REASON_CHARS: usize = 256;
const MAX_SAMPLES_PER_REASON: usize = 3;
const MAX_SAMPLE_CHARS: usize = 1024;
const OTHER_REASON: &str = "Other reasons (diagnostic category limit reached)";

#[derive(Default, Serialize)]
pub(crate) struct Coverage {
    classes_in_input: usize,
    selected_classes: usize,
    total_methods: usize,
    declaration_only_methods: usize,
    concrete_methods: usize,
    reconstructed_methods: usize,
    fallback_methods: usize,
    fallback_reasons: BTreeMap<String, usize>,
    /// Bounded representative method identifiers for each normalized reason.
    /// Counts remain authoritative in `fallback_reasons`.
    fallback_reason_samples: BTreeMap<String, Vec<String>>,
}

impl Coverage {
    fn fallback(&mut self, reason: &str, method_id: &str) {
        self.fallback_methods += 1;
        // Keep opcode categories stable across instruction positions.
        let reason = if reason.starts_with("unsupported opcode ") {
            reason.split(" at ").next().unwrap_or(reason)
        } else {
            reason
        };
        let mut key: String = reason.chars().take(MAX_REASON_CHARS).collect();
        if !self.fallback_reasons.contains_key(&key)
            && self.fallback_reasons.len() >= MAX_REASONS - 1
        {
            key = OTHER_REASON.into();
        }
        *self.fallback_reasons.entry(key.clone()).or_default() += 1;
        let samples = self.fallback_reason_samples.entry(key).or_default();
        let sample: String = method_id.chars().take(MAX_SAMPLE_CHARS).collect();
        if !samples.contains(&sample) {
            samples.push(sample);
            samples.sort();
            samples.truncate(MAX_SAMPLES_PER_REASON);
        }
    }
}

fn collect(path: &Path, prefix: Option<&str>) -> Result<Coverage> {
    let mut engine = NativeDexEngine::default();
    let project = engine.open(path)?;
    let mut coverage = Coverage {
        classes_in_input: project.classes.len(),
        ..Default::default()
    };
    for name in project.classes {
        if prefix.is_some_and(|prefix| !name.starts_with(prefix)) {
            continue;
        }
        coverage.selected_classes += 1;
        let class = engine.class(&name).context("Loaded class disappeared")?;
        for method in &class.methods {
            coverage.total_methods += 1;
            if method.access_flags & (0x400 | 0x100) != 0 {
                coverage.declaration_only_methods += 1;
                continue;
            }
            coverage.concrete_methods += 1;
            // Render only this method, once. Drop source and mappings immediately.
            match rdx::native_java::render_method(&name, class, method) {
                Ok(_) => coverage.reconstructed_methods += 1,
                Err(error) => {
                    let reason = error.to_string();
                    let method_id = format!(
                        "{name}.{}({}){}",
                        method.name,
                        method.parameters.join(""),
                        method.return_type
                    );
                    if reason == "Unsupported method modifiers" {
                        coverage.fallback(
                            &format!("{reason} (access_flags=0x{:08x})", method.access_flags),
                            &method_id,
                        );
                    } else {
                        coverage.fallback(&reason, &method_id);
                    }
                }
            }
        }
    }
    Ok(coverage)
}

pub fn execute(path: &Path, prefix: Option<&str>) -> Result<()> {
    let coverage = collect(path, prefix)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &coverage)?;
    writeln!(stdout)?;
    Ok(())
}

/// Graph construction is measured independently from Java reconstruction.
#[derive(Default, Serialize)]
struct CfgAudit {
    semantics: SemanticAudit,
    selected_classes: usize,
    concrete_methods: usize,
    accepted_graphs: usize,
    rejected_graphs: usize,
    total_blocks: usize,
    normal_edges: usize,
    exceptional_edges: usize,
    accepted_instruction_decodes: usize,
    rejected_instruction_decodes: usize,
    decoded_instructions: usize,
    register_read_operands: usize,
    register_write_operands: usize,
    potentially_throwing_instructions: usize,
    instruction_rejection_reasons: BTreeMap<String, usize>,
    instruction_rejection_samples: BTreeMap<String, Vec<String>>,
    accepted_call_bindings: usize,
    rejected_call_bindings: usize,
    bound_calls: usize,
    bound_receivers: usize,
    bound_arguments: usize,
    bound_results: usize,
    call_binding_rejection_reasons: BTreeMap<String, usize>,
    call_binding_rejection_samples: BTreeMap<String, Vec<String>>,
    accepted_ssa_analyses: usize,
    rejected_ssa_analyses: usize,
    ssa_definitions: usize,
    ssa_phis: usize,
    accepted_ssa_call_links: usize,
    rejected_ssa_call_links: usize,
    ssa_linked_calls: usize,
    ssa_unreachable_calls: usize,
    ssa_call_rejection_reasons: BTreeMap<String, usize>,
    ssa_call_rejection_samples: BTreeMap<String, Vec<String>>,
    ssa_rejection_reasons: BTreeMap<String, usize>,
    ssa_rejection_samples: BTreeMap<String, Vec<String>>,
    accepted_dominator_analyses: usize,
    rejected_dominator_analyses: usize,
    dominance_frontier_entries: usize,
    blocks_unreachable_from_entry: usize,
    dominator_rejection_reasons: BTreeMap<String, usize>,
    dominator_rejection_samples: BTreeMap<String, Vec<String>>,
    rejection_reasons: BTreeMap<String, usize>,
    rejection_samples: BTreeMap<String, Vec<String>>,
}

/// Completion of an analysis is separate from resolution of every value/call.
#[derive(Default, Serialize)]
struct SemanticAudit {
    type_analyses: usize,
    rejected_type_analyses: usize,
    resolved_values: usize,
    unresolved_values: usize,
    conflicting_values: usize,
    wide_pair_issues: usize,
    methods_with_type_conflicts: usize,
    type_conflict_reasons: BTreeMap<String, usize>,
    type_conflict_samples: BTreeMap<String, Vec<String>>,
    unresolved_type_reasons: BTreeMap<String, usize>,
    unresolved_type_samples: BTreeMap<String, Vec<String>>,
    type_rejection_reasons: BTreeMap<String, usize>,
    type_rejection_samples: BTreeMap<String, Vec<String>>,
    constructor_analyses: usize,
    rejected_constructor_analyses: usize,
    constructor_bindings: usize,
    allocation_constructors: usize,
    constructors_requiring_owner_retarget: usize,
    this_constructors: usize,
    super_constructors: usize,
    unresolved_constructors: usize,
    unreachable_constructors: usize,
    constructor_issue_reasons: BTreeMap<String, usize>,
    constructor_issue_samples: BTreeMap<String, Vec<String>>,
    constructor_rejection_reasons: BTreeMap<String, usize>,
    constructor_rejection_samples: BTreeMap<String, Vec<String>>,
}

#[derive(Default)]
struct SemanticDiagnostics {
    types: Coverage,
    type_conflicts: Coverage,
    unresolved_types: Coverage,
    constructors: Coverage,
    constructor_issues: Coverage,
}

#[allow(clippy::too_many_arguments)]
fn audit_semantics(
    audit: &mut SemanticAudit,
    diagnostics: &mut SemanticDiagnostics,
    class: &rdx::native_dex::DexClass,
    method: &rdx::native_dex::DexMethod,
    ir: &rdx::native_ir::DecodedMethod,
    bound: &rdx::native_calls::BoundCalls,
    ssa: &rdx::native_ssa::SsaMethod,
    linked: &rdx::native_call_values::SsaCalls,
) {
    let id = format!(
        "{}.{}({}){}",
        class.descriptor,
        method.name,
        method.parameters.join(""),
        method.return_type
    );
    match rdx::native_types::InferredTypes::infer(method, ir, ssa, linked, &class.symbols) {
        Ok(types) => {
            audit.type_analyses += 1;
            audit.resolved_values += types.resolved_values;
            audit.unresolved_values += types.unresolved_values;
            audit.conflicting_values += types.conflicting_values;
            audit.wide_pair_issues += types.wide_pair_issues;
            audit.methods_with_type_conflicts += usize::from(types.conflicting_values != 0);
            for (value, ty) in types.values.iter().enumerate().filter(|(_, ty)| ty.active) {
                use rdx::native_types::TypeResolution;
                match &ty.resolution {
                    TypeResolution::Conflict(reason) => diagnostics
                        .type_conflicts
                        .fallback(reason, &format!("{id}:value{value}")),
                    TypeResolution::Unresolved(reason) => diagnostics
                        .unresolved_types
                        .fallback(reason, &format!("{id}:value{value}")),
                    TypeResolution::Resolved(_) => {}
                }
            }
        }
        Err(error) => {
            audit.rejected_type_analyses += 1;
            diagnostics.types.fallback(&error.to_string(), &id);
        }
    }
    match rdx::native_constructors::ConstructorAnalysis::analyze(
        class, method, ir, bound, ssa, linked,
    ) {
        Ok(constructors) => {
            audit.constructor_analyses += 1;
            audit.constructor_bindings += constructors.bindings.len();
            for binding in &constructors.bindings {
                audit.constructors_requiring_owner_retarget +=
                    usize::from(binding.owner_retarget_required);
                use rdx::native_constructors::ConstructorOrigin;
                match &binding.origin {
                    ConstructorOrigin::Allocation { .. } => audit.allocation_constructors += 1,
                    ConstructorOrigin::This => audit.this_constructors += 1,
                    ConstructorOrigin::Super => audit.super_constructors += 1,
                }
            }
            audit.unresolved_constructors += constructors.unresolved.len();
            audit.unreachable_constructors += constructors.unreachable_constructors;
            for issue in constructors.unresolved {
                diagnostics
                    .constructor_issues
                    .fallback(issue.reason, &format!("{id}@{}", issue.invoke_pc));
            }
        }
        Err(error) => {
            audit.rejected_constructor_analyses += 1;
            diagnostics.constructors.fallback(&error.to_string(), &id);
        }
    }
}

fn collect_cfg(path: &Path, prefix: Option<&str>) -> Result<CfgAudit> {
    use rdx::native_cfg::{ControlFlowGraph, EdgeKind};
    let mut engine = NativeDexEngine::default();
    let project = engine.open(path)?;
    let mut audit = CfgAudit::default();
    let mut semantic_diagnostics = SemanticDiagnostics::default();
    let mut diagnostics = Coverage::default();
    let mut dominator_diagnostics = Coverage::default();
    let mut instruction_diagnostics = Coverage::default();
    let mut call_diagnostics = Coverage::default();
    let mut ssa_diagnostics = Coverage::default();
    let mut ssa_call_diagnostics = Coverage::default();
    for name in project.classes {
        if prefix.is_some_and(|prefix| !name.starts_with(prefix)) {
            continue;
        }
        audit.selected_classes += 1;
        let class = engine.class(&name).context("Loaded class disappeared")?;
        for method in &class.methods {
            if method.access_flags & (0x400 | 0x100) != 0 {
                continue;
            }
            audit.concrete_methods += 1;
            let mut decoded = None;
            let mut bound_calls = None;
            match method
                .code
                .as_ref()
                .context("method has no code")
                .and_then(rdx::native_ir::DecodedMethod::decode)
            {
                Ok(ir) => {
                    audit.accepted_instruction_decodes += 1;
                    audit.decoded_instructions += ir.instructions.len();
                    for instruction in &ir.instructions {
                        audit.register_read_operands += instruction.reads.len();
                        audit.register_write_operands += instruction.writes.len();
                        audit.potentially_throwing_instructions +=
                            usize::from(instruction.may_throw);
                    }
                    match rdx::native_calls::BoundCalls::bind(
                        method.code.as_ref().expect("decoded method has code"),
                        &ir,
                        &class.symbols,
                    ) {
                        Ok(bound) => {
                            audit.accepted_call_bindings += 1;
                            audit.bound_calls += bound.calls.len();
                            for call in &bound.calls {
                                audit.bound_receivers += usize::from(call.receiver.is_some());
                                audit.bound_arguments += call.arguments.len();
                                audit.bound_results += usize::from(call.result.is_some());
                            }
                            bound_calls = Some(bound);
                        }
                        Err(error) => {
                            audit.rejected_call_bindings += 1;
                            call_diagnostics.fallback(
                                &error.to_string(),
                                &format!(
                                    "{name}.{}({}){}",
                                    method.name,
                                    method.parameters.join(""),
                                    method.return_type
                                ),
                            );
                        }
                    }
                    decoded = Some(ir);
                }
                Err(error) => {
                    audit.rejected_instruction_decodes += 1;
                    instruction_diagnostics.fallback(
                        &error.to_string(),
                        &format!(
                            "{name}.{}({}){}",
                            method.name,
                            method.parameters.join(""),
                            method.return_type
                        ),
                    );
                }
            }
            let graph = method
                .code
                .as_ref()
                .context("method has no code")
                .and_then(ControlFlowGraph::build);
            match graph {
                Ok(graph) => {
                    audit.accepted_graphs += 1;
                    audit.total_blocks += graph.blocks.len();
                    for edge in graph.blocks.iter().flat_map(|block| &block.successors) {
                        match edge.kind {
                            EdgeKind::Normal => audit.normal_edges += 1,
                            EdgeKind::Exceptional => audit.exceptional_edges += 1,
                        }
                    }
                    if let Some(ir) = &decoded {
                        match rdx::native_ssa::SsaMethod::build(
                            method.code.as_ref().expect("decoded method has code"),
                            ir,
                            &graph,
                        ) {
                            Ok(ssa) => {
                                audit.accepted_ssa_analyses += 1;
                                audit.ssa_definitions += ssa.definitions.len();
                                audit.ssa_phis += ssa.phis.len();
                                if let Some(bound) = &bound_calls {
                                    match rdx::native_call_values::SsaCalls::bind(bound, &ssa) {
                                        Ok(linked) => {
                                            audit.accepted_ssa_call_links += 1;
                                            audit.ssa_linked_calls += linked.calls.len();
                                            audit.ssa_unreachable_calls += linked.unreachable_calls;
                                            audit_semantics(
                                                &mut audit.semantics,
                                                &mut semantic_diagnostics,
                                                class,
                                                method,
                                                ir,
                                                bound,
                                                &ssa,
                                                &linked,
                                            );
                                        }
                                        Err(error) => {
                                            audit.rejected_ssa_call_links += 1;
                                            ssa_call_diagnostics.fallback(
                                                &error.to_string(),
                                                &format!(
                                                    "{name}.{}({}){}",
                                                    method.name,
                                                    method.parameters.join(""),
                                                    method.return_type
                                                ),
                                            );
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                audit.rejected_ssa_analyses += 1;
                                ssa_diagnostics.fallback(
                                    &error.to_string(),
                                    &format!(
                                        "{name}.{}({}){}",
                                        method.name,
                                        method.parameters.join(""),
                                        method.return_type
                                    ),
                                );
                            }
                        }
                    }
                    match rdx::native_dominators::DominatorTree::compute(&graph) {
                        Ok(tree) => {
                            audit.accepted_dominator_analyses += 1;
                            audit.dominance_frontier_entries +=
                                tree.frontiers.iter().map(Vec::len).sum::<usize>();
                            audit.blocks_unreachable_from_entry +=
                                tree.reachable.iter().filter(|&&reached| !reached).count();
                        }
                        Err(error) => {
                            audit.rejected_dominator_analyses += 1;
                            dominator_diagnostics.fallback(
                                &error.to_string(),
                                &format!(
                                    "{name}.{}({}){}",
                                    method.name,
                                    method.parameters.join(""),
                                    method.return_type
                                ),
                            );
                        }
                    }
                }
                Err(error) => {
                    audit.rejected_graphs += 1;
                    diagnostics.fallback(
                        &error.to_string(),
                        &format!(
                            "{name}.{}({}){}",
                            method.name,
                            method.parameters.join(""),
                            method.return_type
                        ),
                    );
                }
            }
        }
    }
    audit.semantics.type_conflict_reasons = semantic_diagnostics.type_conflicts.fallback_reasons;
    audit.semantics.type_conflict_samples =
        semantic_diagnostics.type_conflicts.fallback_reason_samples;
    audit.semantics.unresolved_type_reasons =
        semantic_diagnostics.unresolved_types.fallback_reasons;
    audit.semantics.unresolved_type_samples = semantic_diagnostics
        .unresolved_types
        .fallback_reason_samples;
    audit.semantics.type_rejection_reasons = semantic_diagnostics.types.fallback_reasons;
    audit.semantics.type_rejection_samples = semantic_diagnostics.types.fallback_reason_samples;
    audit.semantics.constructor_rejection_reasons =
        semantic_diagnostics.constructors.fallback_reasons;
    audit.semantics.constructor_rejection_samples =
        semantic_diagnostics.constructors.fallback_reason_samples;
    audit.semantics.constructor_issue_reasons =
        semantic_diagnostics.constructor_issues.fallback_reasons;
    audit.semantics.constructor_issue_samples = semantic_diagnostics
        .constructor_issues
        .fallback_reason_samples;
    audit.rejection_reasons = diagnostics.fallback_reasons;
    audit.rejection_samples = diagnostics.fallback_reason_samples;
    audit.dominator_rejection_reasons = dominator_diagnostics.fallback_reasons;
    audit.dominator_rejection_samples = dominator_diagnostics.fallback_reason_samples;
    audit.instruction_rejection_reasons = instruction_diagnostics.fallback_reasons;
    audit.instruction_rejection_samples = instruction_diagnostics.fallback_reason_samples;
    audit.ssa_call_rejection_reasons = ssa_call_diagnostics.fallback_reasons;
    audit.ssa_call_rejection_samples = ssa_call_diagnostics.fallback_reason_samples;
    audit.ssa_rejection_reasons = ssa_diagnostics.fallback_reasons;
    audit.ssa_rejection_samples = ssa_diagnostics.fallback_reason_samples;
    audit.call_binding_rejection_reasons = call_diagnostics.fallback_reasons;
    audit.call_binding_rejection_samples = call_diagnostics.fallback_reason_samples;
    Ok(audit)
}

pub fn execute_cfg(path: &Path, prefix: Option<&str>) -> Result<()> {
    let audit = collect_cfg(path, prefix)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &audit)?;
    writeln!(stdout)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfg_audit_accounts_for_fixture_and_class_filter() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello.dex");
        let audit = collect_cfg(&path, None).unwrap();
        assert!(audit.accepted_graphs > 0);
        assert_eq!(audit.rejected_graphs, 0);
        assert_eq!(
            audit.concrete_methods,
            audit.accepted_graphs + audit.rejected_graphs
        );
        assert!(audit.total_blocks >= audit.accepted_graphs);
        assert_eq!(audit.accepted_dominator_analyses, audit.accepted_graphs);
        assert_eq!(audit.rejected_dominator_analyses, 0);
        assert_eq!(audit.accepted_instruction_decodes, audit.concrete_methods);
        assert_eq!(audit.rejected_instruction_decodes, 0);
        assert!(audit.decoded_instructions >= audit.concrete_methods);
        assert_eq!(
            audit.accepted_call_bindings,
            audit.accepted_instruction_decodes
        );
        assert_eq!(audit.rejected_call_bindings, 0);
        assert_eq!(audit.accepted_ssa_analyses, audit.concrete_methods);
        assert_eq!(audit.rejected_ssa_analyses, 0);
        assert_eq!(audit.accepted_ssa_call_links, audit.concrete_methods);
        assert_eq!(audit.rejected_ssa_call_links, 0);
        assert_eq!(audit.semantics.type_analyses, audit.concrete_methods);
        assert_eq!(audit.semantics.rejected_type_analyses, 0);
        assert_eq!(audit.semantics.constructor_analyses, audit.concrete_methods);
        assert_eq!(audit.semantics.rejected_constructor_analyses, 0);
        assert_eq!(
            audit.semantics.constructor_bindings,
            audit.semantics.allocation_constructors
                + audit.semantics.this_constructors
                + audit.semantics.super_constructors
        );
        assert_eq!(
            audit.semantics.unresolved_constructors,
            audit
                .semantics
                .constructor_issue_reasons
                .values()
                .sum::<usize>()
        );
        let none = collect_cfg(&path, Some("no.such.package.")).unwrap();
        assert_eq!(none.selected_classes, 0);
        assert_eq!(none.concrete_methods, 0);
    }

    #[test]
    fn fixture_coverage_accounts_for_every_method_and_filters_classes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello.dex");
        let all = collect(&path, None).unwrap();
        assert!(all.selected_classes > 0);
        assert!(all.reconstructed_methods > 0);
        assert_eq!(
            all.total_methods,
            all.concrete_methods + all.declaration_only_methods
        );
        assert_eq!(
            all.concrete_methods,
            all.reconstructed_methods + all.fallback_methods
        );
        assert_eq!(
            all.fallback_methods,
            all.fallback_reasons.values().sum::<usize>()
        );
        let none = collect(&path, Some("no.such.package.")).unwrap();
        assert_eq!(none.classes_in_input, all.classes_in_input);
        assert_eq!(none.selected_classes, 0);
        assert_eq!(none.total_methods, 0);
    }

    #[test]
    fn reasons_group_opcode_positions_and_bound_diagnostic_memory() {
        let mut coverage = Coverage::default();
        coverage.fallback("unsupported opcode 0x23 at 8", "z.Method()V");
        coverage.fallback("unsupported opcode 0x23 at 400", "a.Method()V");
        assert_eq!(coverage.fallback_reasons["unsupported opcode 0x23"], 2);
        assert_eq!(
            coverage.fallback_reason_samples["unsupported opcode 0x23"],
            vec!["a.Method()V", "z.Method()V"]
        );
        for index in 0..1_000 {
            coverage.fallback(
                &format!("reason {index} {}", "x".repeat(300)),
                &format!("sample.Method{index}()V"),
            );
        }
        assert!(coverage.fallback_reasons.len() <= MAX_REASONS);
        assert!(
            coverage
                .fallback_reasons
                .keys()
                .all(|key| key.chars().count() <= MAX_REASON_CHARS)
        );
        assert_eq!(coverage.fallback_reasons.values().sum::<usize>(), 1_002);
        assert!(coverage.fallback_reasons[OTHER_REASON] > 0);
        assert!(
            coverage
                .fallback_reason_samples
                .values()
                .all(|samples| samples.len() <= MAX_SAMPLES_PER_REASON)
        );
        let mut bounded = Coverage::default();
        bounded.fallback("oversized identifier", &"é".repeat(2048));
        assert_eq!(
            bounded.fallback_reason_samples["oversized identifier"][0]
                .chars()
                .count(),
            MAX_SAMPLE_CHARS
        );
    }
}
