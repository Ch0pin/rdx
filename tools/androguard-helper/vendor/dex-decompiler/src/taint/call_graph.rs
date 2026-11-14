//! Call-graph construction from invoke sites.

use std::collections::HashMap;

use crate::decompile::value_flow::ValueFlowAnalysisOwned;
use crate::detectors::is_library_class;
use crate::error::Result;

use super::index::{MethodId, MethodIndex};

#[derive(Clone, Debug)]
pub struct CallEdge {
    pub caller: MethodId,
    pub callee: MethodId,
    /// Invoke instruction offset (method-relative).
    pub invoke_offset: u32,
    /// Argument registers at the call site (order = Dalvik invoke regs).
    pub arg_regs: Vec<u32>,
    pub method_ref: String,
}

#[derive(Debug, Default)]
pub struct CallGraph {
    /// caller → outgoing edges
    pub outs: HashMap<MethodId, Vec<CallEdge>>,
    /// callee → incoming edges
    pub ins: HashMap<MethodId, Vec<CallEdge>>,
}

impl CallGraph {
    /// Build the call graph from a precomputed value-flow cache (no re-analysis).
    pub fn build_from_vf_cache(
        index: &MethodIndex,
        vf_cache: &HashMap<MethodId, ValueFlowAnalysisOwned>,
        include: impl Fn(MethodId) -> bool,
    ) -> Result<Self> {
        let mut cg = CallGraph::default();
        for mref in &index.methods {
            if !include(mref.id) {
                continue;
            }
            let Some(owned) = vf_cache.get(&mref.id) else {
                continue;
            };
            for (&invoke_offset, method_ref) in &owned.invoke_method_map {
                let callees = index.resolve_callees(method_ref);
                if callees.is_empty() {
                    continue;
                }
                let arg_regs = owned
                    .rw_map
                    .get(&invoke_offset)
                    .map(|(reads, _)| reads.clone())
                    .unwrap_or_default();
                for callee in callees {
                    if !include(callee) {
                        continue;
                    }
                    let edge = CallEdge {
                        caller: mref.id,
                        callee,
                        invoke_offset,
                        arg_regs: arg_regs.clone(),
                        method_ref: method_ref.clone(),
                    };
                    cg.outs.entry(mref.id).or_default().push(edge.clone());
                    cg.ins.entry(callee).or_default().push(edge);
                }
            }
            // Callback shims: resolve concrete listener/runnable allocations only.
            let analysis = owned.analysis();
            for (&dispatch_offset, dispatch_ref) in &owned.invoke_method_map {
                let Some((callback_reg, target_methods)) = callback_dispatch_info(dispatch_ref)
                else {
                    continue;
                };
                let dispatch_args = owned
                    .rw_map
                    .get(&dispatch_offset)
                    .map(|(reads, _)| reads.clone())
                    .unwrap_or_default();
                let Some(&reg) = dispatch_args.get(callback_reg) else {
                    // Fall back to last arg (Executor.execute style).
                    let Some(&reg) = dispatch_args.last() else {
                        continue;
                    };
                    for (callee, captured_args, method_ref) in callback_targets(
                        index,
                        owned,
                        &analysis,
                        dispatch_offset,
                        reg,
                        &target_methods,
                    ) {
                        push_shim_edge(&mut cg, mref.id, callee, dispatch_offset, captured_args, method_ref, &include);
                    }
                    continue;
                };
                for (callee, captured_args, method_ref) in callback_targets(
                    index,
                    owned,
                    &analysis,
                    dispatch_offset,
                    reg,
                    &target_methods,
                ) {
                    push_shim_edge(
                        &mut cg,
                        mref.id,
                        callee,
                        dispatch_offset,
                        captured_args,
                        method_ref,
                        &include,
                    );
                }
            }
        }
        Ok(cg)
    }

    pub fn edge_count(&self) -> usize {
        self.outs.values().map(|v| v.len()).sum()
    }
}

fn push_shim_edge(
    cg: &mut CallGraph,
    caller: MethodId,
    callee: MethodId,
    invoke_offset: u32,
    arg_regs: Vec<u32>,
    method_ref: String,
    include: &impl Fn(MethodId) -> bool,
) {
    if !include(callee) {
        return;
    }
    if cg.outs.get(&caller).is_some_and(|edges| {
        edges
            .iter()
            .any(|edge| edge.invoke_offset == invoke_offset && edge.callee == callee)
    }) {
        return;
    }
    let shim_ref = if method_ref.starts_with("shim:") {
        method_ref
    } else {
        format!("shim:{method_ref}")
    };
    let edge = CallEdge {
        caller,
        callee,
        invoke_offset,
        arg_regs,
        method_ref: shim_ref,
    };
    cg.outs.entry(caller).or_default().push(edge.clone());
    cg.ins.entry(callee).or_default().push(edge);
}

/// Returns (callback arg index, method names to resolve on the concrete class).
fn callback_dispatch_info(method_ref: &str) -> Option<(usize, Vec<&'static str>)> {
    if method_ref.contains("Executor.execute")
        || method_ref.contains("ExecutorService.submit")
        || method_ref.contains("Handler.post")
        || method_ref.contains("Handler.postDelayed")
        || method_ref.contains("ScheduledExecutorService.schedule")
        || method_ref.contains("Timer.schedule")
    {
        return Some((usize::MAX, vec!["run"])); // MAX → last arg
    }
    if method_ref.contains("AsyncTask.execute") {
        return Some((0, vec!["doInBackground", "onPostExecute"]));
    }
    if method_ref.contains("setOnClickListener") {
        return Some((1, vec!["onClick"]));
    }
    if method_ref.contains("setOnLongClickListener") {
        return Some((1, vec!["onLongClick"]));
    }
    if method_ref.contains("setOnCheckedChangeListener") {
        return Some((1, vec!["onCheckedChanged"]));
    }
    if method_ref.contains("LiveData.observe") || method_ref.contains(".observe(") {
        return Some((1, vec!["onChanged"]));
    }
    if method_ref.contains("addObserver") {
        return Some((1, vec!["onChanged", "onCreate", "onStart", "onResume"]));
    }
    None
}

fn callback_targets(
    index: &MethodIndex,
    owned: &ValueFlowAnalysisOwned,
    analysis: &crate::decompile::value_flow::ValueFlowAnalysis<'_>,
    dispatch_offset: u32,
    callback_reg: u32,
    target_methods: &[&str],
) -> Vec<(MethodId, Vec<u32>, String)> {
    let mut out = Vec::new();
    for (def_offset, _) in analysis.use_def(dispatch_offset, callback_reg) {
        let label = owned
            .insn_at
            .get(&def_offset)
            .map(String::as_str)
            .unwrap_or("");
        if label.starts_with("new-instance") {
            if let Some(class_name) = label.rsplit_once(',').map(|(_, class)| class.trim()) {
                let java = class_name
                    .trim_start_matches('L')
                    .trim_end_matches(';')
                    .replace('/', ".");
                if is_library_class(&java) {
                    continue;
                }
                for meth in target_methods {
                    let method_ref = format!("{java}.{meth}");
                    for callee in index.resolve_callees(&method_ref) {
                        out.push((callee, vec![callback_reg], method_ref.clone()));
                    }
                }
            }
            continue;
        }
        if !label.starts_with("move-result") {
            continue;
        }
        for (&custom_offset, method_ref) in &owned.invoke_method_map {
            if !owned
                .insn_at
                .get(&custom_offset)
                .is_some_and(|label| label.starts_with("invoke-custom"))
            {
                continue;
            }
            let next_result = super::solver::move_result_for_invoke(owned, custom_offset);
            if next_result != Some((def_offset, callback_reg)) {
                continue;
            }
            let captures = owned
                .rw_map
                .get(&custom_offset)
                .map(|(reads, _)| reads.clone())
                .unwrap_or_default();
            for callee in index.resolve_callees(method_ref) {
                out.push((callee, captures.clone(), method_ref.clone()));
            }
        }
    }
    out
}
