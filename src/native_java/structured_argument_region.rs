//! One immutable constructor-argument region; frame/type proofs stay in emission.
use crate::{
    native_calls::{BoundCall, BoundCalls},
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_constructors::{ConstructorBinding, ConstructorOrigin},
    native_dex::{DexClass, DexCode, DexMethod, DexTryRegion},
    native_ir::{DecodedMethod, PoolKind},
    native_method::MethodAnalysis,
};
use std::{
    cell::{OnceCell, RefCell},
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FirstRead {
    pub pc: usize,
    pub register: usize,
    pub words: usize,
}
type ProtectedDispatch = (u32, u32, Vec<(Option<String>, u32)>);
// Memoized proofs keyed by (allocation, constructor).
type PlanMemo = RefCell<BTreeMap<(usize, usize), Option<Rc<Plan>>>>;
// The one staged method certified by a projection: its words, handler table
// and the certified decoding.
type StagedCertificate = RefCell<Option<(Vec<u16>, Vec<DexTryRegion>, Option<Rc<DecodedMethod>>)>>;
// Generated resource accessors allocate dozens of nested items in one
// straight-line argument region. Every owned instruction is still proven.
const MAX_REGION_NODES: usize = 2048;
const MAX_NESTED_INITIALIZATIONS: usize = 256;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub allocation: usize,
    pub constructor: usize,
    pub start: usize,
    pub owned: Vec<usize>,
    pub terminal_islands: Vec<(usize, usize)>,
    pub alias_moves: Vec<usize>,
    pub constructor_aliases: Vec<usize>,
    pub alias_masks: Vec<(usize, usize, usize)>,
    pub definitely_written: Vec<usize>,
    pub first_reads: Vec<FirstRead>,
    pub calls: Vec<BoundCall>,
    pub nested_initializations: Vec<(usize, usize)>,
    pub semantic_references: Vec<(usize, String)>,
    pub edges: Vec<(usize, Vec<usize>)>,
    pub exception_edges: Vec<(usize, Vec<usize>)>,
    pub words: Vec<u16>,
    pub registers: u16,
    pub ins: u16,
    pub outs: u16,
    pub try_regions: Vec<ProtectedDispatch>,
}
pub(super) fn prove(
    class: &DexClass,
    method: &DexMethod,
    allocation: usize,
    constructor: usize,
) -> Option<Plan> {
    prove_inner(class, method, allocation, constructor, None)
}
/// Uncached reference revalidation for the integration tests; rendering
/// validates through the shared [`ProofContext`] of the method instead.
#[cfg(test)]
#[allow(dead_code)]
pub(super) fn validate(class: &DexClass, method: &DexMethod, plan: &Plan) -> bool {
    prove(class, method, plan.allocation, plan.constructor).as_ref() == Some(plan)
}
// Shared inputs belong to this exact immutable class/method. Emission still
// revalidates the resulting plan independently from original DEX instructions.
pub(super) struct ProofInputs<'a> {
    pub ir: &'a DecodedMethod,
    pub cfg: &'a ControlFlowGraph,
    pub bound: &'a BoundCalls,
    pub bindings: &'a [crate::native_constructors::ConstructorBinding],
}
pub(super) fn prove_cached(
    class: &DexClass,
    method: &DexMethod,
    allocation: usize,
    constructor: usize,
    inputs: ProofInputs<'_>,
) -> Option<Plan> {
    prove_inner(class, method, allocation, constructor, Some(inputs))
}
fn same_header(a: &DexMethod, b: &DexMethod) -> bool {
    a.declaring_type == b.declaring_type
        && a.name == b.name
        && a.return_type == b.return_type
        && a.parameters == b.parameters
        && a.thrown_types == b.thrown_types
        && a.access_flags == b.access_flags
}
fn same_code(a: &DexCode, b: &DexCode) -> bool {
    a.registers == b.registers
        && a.ins == b.ins
        && a.outs == b.outs
        && a.tries == b.tries
        && a.offset == b.offset
        && a.try_regions == b.try_regions
        && a.instructions == b.instructions
}
/// Complete method identity: header, frame, handler table and every word.
pub(super) fn same_method(a: &DexMethod, b: &DexMethod) -> bool {
    same_header(a, b)
        && match (&a.code, &b.code) {
            (Some(a), Some(b)) => same_code(a, b),
            (None, None) => true,
            _ => false,
        }
}
pub(super) fn clone_method(method: &DexMethod) -> Option<DexMethod> {
    let code = method.code.as_ref()?;
    Some(DexMethod {
        declaring_type: method.declaring_type.clone(),
        name: method.name.clone(),
        return_type: method.return_type.clone(),
        parameters: method.parameters.clone(),
        thrown_types: method.thrown_types.clone(),
        access_flags: method.access_flags,
        code: Some(DexCode {
            registers: code.registers,
            ins: code.ins,
            outs: code.outs,
            tries: code.tries,
            try_regions: code.try_regions.clone(),
            instructions: code.instructions.clone(),
            offset: code.offset,
        }),
    })
}
/// Shared inputs of every allocation-region proof over one exact immutable
/// method. Rendering one method proves and revalidates dozens of nested
/// regions: decoding, blocks, call binding and constructor identities are
/// computed once, and each proof is memoized per (allocation, constructor).
/// Every use first compares the complete method identity, so a
/// staged region method never borrows the inputs of the method it was carved
/// from. A memoized plan is the proof of this exact instruction stream; a
/// revalidation compares against it instead of recomputing the same function.
pub(super) struct ProofContext {
    source: Rc<DexMethod>,
    ir: Rc<DecodedMethod>,
    analysis: OnceCell<Option<ProofAnalysis>>,
    plans: PlanMemo,
}
struct ProofAnalysis {
    cfg: ControlFlowGraph,
    bound: BoundCalls,
    bindings: Vec<ConstructorBinding>,
}
impl ProofContext {
    pub(super) fn build(source: Rc<DexMethod>) -> Option<Rc<Self>> {
        let ir = DecodedMethod::decode(source.code.as_ref()?).ok()?;
        Some(Rc::new(Self {
            source,
            ir: Rc::new(ir),
            analysis: OnceCell::new(),
            plans: RefCell::new(BTreeMap::new()),
        }))
    }
    pub(super) fn matches(&self, method: &DexMethod) -> bool {
        same_method(method, &self.source)
    }
    pub(super) fn source(&self) -> &Rc<DexMethod> {
        &self.source
    }
    pub(super) fn ir(&self) -> Rc<DecodedMethod> {
        self.ir.clone()
    }
    fn analysis(&self, class: &DexClass) -> Option<&ProofAnalysis> {
        self.analysis
            .get_or_init(|| {
                let code = self.source.code.as_ref()?;
                if code.instructions.len() > 4096 || code.registers > 256 {
                    return None;
                }
                let analysis = MethodAnalysis::build(class, &self.source).ok()?;
                let bindings = analysis.constructors().ok()?.bindings;
                let (_, cfg, bound) = analysis.into_parts();
                Some(ProofAnalysis {
                    cfg,
                    bound,
                    bindings,
                })
            })
            .as_ref()
    }
    /// Prove one region of this exact method. Any other method is proven
    /// directly without sharing an input.
    pub(super) fn prove(
        &self,
        class: &DexClass,
        method: &DexMethod,
        allocation: usize,
        constructor: usize,
    ) -> Option<Rc<Plan>> {
        if !self.matches(method) {
            return prove(class, method, allocation, constructor).map(Rc::new);
        }
        let key = (allocation, constructor);
        if let Some(plan) = self.plans.borrow().get(&key) {
            return plan.clone();
        }
        let plan = self
            .analysis(class)
            .and_then(|analysis| {
                prove_cached(
                    class,
                    method,
                    allocation,
                    constructor,
                    ProofInputs {
                        ir: &self.ir,
                        cfg: &analysis.cfg,
                        bound: &analysis.bound,
                        bindings: &analysis.bindings,
                    },
                )
            })
            .map(Rc::new);
        self.plans.borrow_mut().insert(key, plan.clone());
        plan
    }
    pub(super) fn validate(&self, class: &DexClass, method: &DexMethod, plan: &Plan) -> bool {
        self.prove(class, method, plan.allocation, plan.constructor)
            .as_deref()
            == Some(plan)
    }
}
/// The proof authority of one allocation region: the enclosing projection for
/// a nested region, otherwise the shared context of the method itself.
pub(super) enum RegionProofs {
    Projection(Rc<Projection>),
    Context(Rc<ProofContext>),
}
impl RegionProofs {
    pub(super) fn context(&self) -> Option<&Rc<ProofContext>> {
        match self {
            Self::Projection(_) => None,
            Self::Context(context) => Some(context),
        }
    }
    pub(super) fn ir(&self, class: &DexClass, method: &DexMethod) -> Option<Rc<DecodedMethod>> {
        match self {
            Self::Projection(projection) => projection.checked_ir(class, method),
            Self::Context(context) => Some(context.ir()),
        }
    }
    pub(super) fn prove(
        &self,
        class: &DexClass,
        method: &DexMethod,
        allocation: usize,
        constructor: usize,
    ) -> Option<Rc<Plan>> {
        match self {
            Self::Projection(projection) => {
                projection.prove(class, method, allocation, constructor)
            }
            Self::Context(context) => context.prove(class, method, allocation, constructor),
        }
    }
    pub(super) fn validate(&self, class: &DexClass, method: &DexMethod, plan: &Plan) -> bool {
        match self {
            Self::Projection(projection) => projection.validate(class, method, plan),
            Self::Context(context) => context.validate(class, method, plan),
        }
    }
    /// The original method the proofs are about, never a staged copy.
    pub(super) fn source(&self) -> &Rc<DexMethod> {
        match self {
            Self::Projection(projection) => projection.context.source(),
            Self::Context(context) => context.source(),
        }
    }
}
fn prove_inner(
    class: &DexClass,
    method: &DexMethod,
    allocation: usize,
    constructor: usize,
    inputs: Option<ProofInputs<'_>>,
) -> Option<Plan> {
    let code = method.code.as_ref()?;
    if code.instructions.len() > 4096 || code.registers > 256 || allocation >= constructor {
        return None;
    }
    let owned_ir;
    let ir = if let Some(inputs) = &inputs {
        inputs.ir
    } else {
        owned_ir = DecodedMethod::decode(code).ok()?;
        &owned_ir
    };
    let owned_cfg;
    let cfg = if let Some(inputs) = &inputs {
        inputs.cfg
    } else {
        owned_cfg = ControlFlowGraph::build(code).ok()?;
        &owned_cfg
    };
    let ins: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let alloc = ins.get(&allocation)?;
    if alloc.opcode != 0x22 {
        return None;
    }
    let ctor = ins.get(&constructor)?;
    if !matches!(ctor.opcode, 0x70 | 0x76) {
        return None;
    }
    let start = allocation + alloc.width;
    if start == constructor {
        return None;
    }
    let mut edges = BTreeMap::new();
    for b in &cfg.blocks {
        for (n, pc) in b.instructions.iter().copied().enumerate() {
            let ts: Vec<_> = if n + 1 < b.instructions.len() {
                vec![b.instructions[n + 1]]
            } else {
                b.successors
                    .iter()
                    .filter(|e| e.kind == EdgeKind::Normal)
                    .map(|e| cfg.blocks[e.target].start)
                    .collect()
            };
            edges.insert(pc, ts);
        }
    }
    // First bounded handler shape: an internal protected computation whose
    // handlers also finish inside the argument interval. Preserve each original
    // dispatch, including conservative raw CFG exception edges.
    let internal: Vec<_> = code
        .try_regions
        .iter()
        .filter(|r| start <= r.start as usize && r.end as usize <= constructor)
        .collect();
    let mut exception_edges: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let handler_heads: BTreeSet<_> = internal
        .iter()
        .flat_map(|r| r.catches.iter().map(|(_, h)| *h as usize))
        .collect();
    if !internal.is_empty() {
        for (index, region) in internal.iter().enumerate() {
            if region.catches.is_empty()
                || region.catches.iter().any(|(ty, h)| {
                    (*h as usize) < start
                        || *h as usize >= constructor
                        || internal.iter().any(|r| {
                            r.start as usize <= *h as usize && (*h as usize) < r.end as usize
                        })
                        || ty.as_ref().is_some_and(|t| {
                            !class.symbols.hierarchy.get().is_some_and(|h| {
                                h.assignable(t, "Ljava/lang/Throwable;")
                                    == crate::native_hierarchy::Relation::Proven
                            })
                        })
                })
            {
                return None;
            }
            if internal.iter().skip(index + 1).any(|r| {
                (region.start < r.end && r.start < region.end)
                    || region
                        .catches
                        .iter()
                        .any(|(_, h)| r.catches.iter().any(|(_, other)| h == other))
            }) {
                return None;
            }
        }
        for block in &cfg.blocks {
            let targets: BTreeSet<_> = block
                .successors
                .iter()
                .filter(|e| e.kind == EdgeKind::Exceptional)
                .map(|e| cfg.blocks[e.target].start)
                .collect();
            for &pc in &block.instructions {
                let expected: BTreeSet<_> = code
                    .try_regions
                    .iter()
                    .filter(|r| r.start as usize <= pc && pc < (r.end as usize))
                    .flat_map(|r| r.catches.iter().map(|(_, h)| *h as usize))
                    .collect();
                if expected != targets {
                    return None;
                }
                let owned_targets: Vec<_> = targets
                    .iter()
                    .copied()
                    .filter(|h| handler_heads.contains(h))
                    .collect();
                if !owned_targets.is_empty() {
                    exception_edges.insert(pc, owned_targets);
                }
            }
        }
    }
    let mut nodes = BTreeSet::new();
    let mut todo = vec![start];
    while let Some(pc) = todo.pop() {
        if pc == constructor || !nodes.insert(pc) {
            continue;
        }
        if pc < start || pc >= constructor {
            return None;
        }
        todo.extend(
            edges
                .get(&pc)?
                .iter()
                .copied()
                .filter(|to| *to < constructor),
        );
        todo.extend(exception_edges.get(&pc).into_iter().flatten().copied());
        if nodes.len() > MAX_REGION_NODES {
            return None;
        }
    }
    if nodes.is_empty() || nodes.len() > MAX_REGION_NODES {
        return None;
    }
    if nodes.iter().any(|pc| !edges.contains_key(pc)) {
        return None;
    }
    let exits: BTreeSet<_> = nodes
        .iter()
        .flat_map(|pc| edges[pc].iter().copied())
        .filter(|to| !nodes.contains(to) && *to != constructor)
        .collect();
    let mut terminal_islands = Vec::new();
    for entry in exits {
        if entry <= constructor {
            return None;
        }
        let mut cursor = entry;
        loop {
            if nodes.contains(&cursor) {
                return None;
            }
            let instruction = ins.get(&cursor)?;
            if matches!(instruction.opcode,0x0e..=0x11|0x1d..=0x1e|0x28..=0x2c|0x32..=0x3d) {
                return None;
            }
            if code.try_regions.iter().any(|region| {
                let owns = |pc: usize| region.start as usize <= pc && pc < region.end as usize;
                owns(cursor) != owns(allocation)
                    || region
                        .catches
                        .iter()
                        .any(|(_, handler)| *handler as usize == cursor)
            }) {
                return None;
            }
            nodes.insert(cursor);
            if nodes.len() > MAX_REGION_NODES {
                return None;
            }
            let next = cursor + instruction.width;
            let successors = edges.get(&cursor)?;
            if instruction.opcode == 0x27 {
                if !successors.is_empty() {
                    return None;
                }
                terminal_islands.push((entry, next));
                break;
            }
            if successors != &vec![next] {
                return None;
            }
            cursor = next;
        }
    }
    for (entry, _) in &terminal_islands {
        if edges
            .values()
            .filter(|targets| targets.contains(entry))
            .count()
            != 1
        {
            return None;
        }
    }
    if nodes.is_empty() || nodes.len() > MAX_REGION_NODES {
        return None;
    }
    // The bounded proof owns this argument region. Unrelated method blocks
    // must not consume its budget; all original external edges remain checked.
    if cfg
        .blocks
        .iter()
        .filter(|block| block.instructions.iter().any(|pc| nodes.contains(pc)))
        .count()
        > 128
    {
        return None;
    }
    if nodes.iter().any(|pc| {
        matches!(ins[pc].opcode, 0x0e..=0x11 | 0x1d | 0x1e)
            || (ins[pc].opcode == 0 && code.instructions[*pc] != 0)
    }) {
        return None;
    }
    // Complete original normal-edge identity, including fallthrough, with one entry and endpoint.
    for (&from, ts) in &edges {
        for &to in ts {
            // A move-exception handler head is entered only by original
            // exceptional dispatch, including from other owned instructions.
            if handler_heads.contains(&to) {
                return None;
            }
            if nodes.contains(&from) {
                if !nodes.contains(&to) && to != constructor {
                    return None;
                }
            } else if (nodes.contains(&to) || to == constructor)
                && (from != allocation || to != start)
            {
                return None;
            }
        }
    }
    for (&from, targets) in &exception_edges {
        for &to in targets {
            if !nodes.contains(&from) || !nodes.contains(&to) {
                return None;
            }
        }
    }
    let mut reachable = BTreeSet::new();
    let mut todo = vec![start];
    while let Some(pc) = todo.pop() {
        if pc != constructor && reachable.insert(pc) {
            if !nodes.contains(&pc) {
                return None;
            }
            todo.extend(&edges[&pc]);
            todo.extend(exception_edges.get(&pc).into_iter().flatten().copied());
        }
    }
    if reachable != nodes {
        return None;
    }
    if nodes
        .iter()
        .any(|pc| edges[pc].is_empty() && ins[pc].opcode != 0x27)
    {
        return None;
    }
    if !nodes.iter().any(|pc| edges[pc].contains(&constructor)) {
        return None;
    }
    for t in &code.try_regions {
        if !(t.end as usize <= allocation
            || t.start as usize > constructor
            || (t.start as usize <= allocation && constructor + ctor.width <= t.end as usize)
            || internal.iter().any(|region| std::ptr::eq(*region, t)))
        {
            return None;
        }
        if t.catches.iter().any(|(_, pc)| {
            (nodes.contains(&(*pc as usize)) && !handler_heads.contains(&(*pc as usize)))
                || *pc as usize == constructor
        }) {
            return None;
        }
    }
    // Constructor identity comes from raw SSA, not address proximity or a Java type guess.
    let owned_bindings;
    let bindings = if let Some(inputs) = &inputs {
        inputs.bindings
    } else {
        let analysis = MethodAnalysis::build(class, method).ok()?;
        owned_bindings = analysis.constructors().ok()?.bindings;
        &owned_bindings
    };
    let exact: Vec<_> = bindings
        .iter()
        .filter(|b| matches!(b.origin,ConstructorOrigin::Allocation{pc,..} if pc==allocation))
        .collect();
    let [binding] = exact[..] else {
        return None;
    };
    if binding.invoke_pc != constructor {
        return None;
    }
    // Nested identities belong to this region and have exactly one original
    // initialization. Their detailed uninitialized-use and event-order checks
    // remain in recursive allocation emission; proximity is never identity.
    let mut nested_initializations = vec![];
    for &pc in &nodes {
        if ins[&pc].opcode != 0x22 {
            continue;
        }
        let matches: Vec<_> = bindings.iter().filter(|binding| {
            matches!(binding.origin, ConstructorOrigin::Allocation { pc: origin, .. } if origin == pc)
        }).collect();
        let [nested] = matches[..] else {
            return None;
        };
        if !nodes.contains(&nested.invoke_pc) || pc >= nested.invoke_pc {
            return None;
        }
        nested_initializations.push((pc, nested.invoke_pc));
    }
    if nested_initializations.len() > MAX_NESTED_INITIALIZATIONS {
        return None;
    }
    let owned_bound;
    let bound = if let Some(inputs) = &inputs {
        inputs.bound
    } else {
        owned_bound = BoundCalls::bind(code, ir, &class.symbols).ok()?;
        &owned_bound
    };
    let calls: BTreeMap<_, _> = bound.calls.iter().map(|c| (c.pc, c)).collect();
    let constructor_call = calls.get(&constructor)?;
    if constructor_call.arguments.is_empty() {
        return None;
    }
    let mut preds: BTreeMap<_, Vec<_>> = ins.keys().map(|pc| (*pc, vec![])).collect();
    for (&from, ts) in &edges {
        for &to in ts {
            preds.get_mut(&to)?.push(from);
        }
    }
    for pc in &nodes {
        if matches!(ins[pc].opcode, 0x0a..=0x0c) {
            let [before] = preds[pc][..] else {
                return None;
            };
            if before + ins[&before].width != *pc || !calls.contains_key(&before) {
                return None;
            }
        }
    }
    let mut exception_preds: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (&from, targets) in &exception_edges {
        for &to in targets {
            exception_preds.entry(to).or_default().push(from);
        }
    }
    // Definite writes are a greatest fixed point over all normal predecessors.
    // Starting internal edges at the full register universe and the allocation
    // edge at empty prevents a backedge from inventing a first-iteration input.
    let universe: BTreeSet<_> = (0..code.registers as usize).collect();
    let mut defined: BTreeMap<_, _> = nodes.iter().map(|pc| (*pc, universe.clone())).collect();
    defined.insert(allocation, BTreeSet::new());
    let mut defined_before: BTreeMap<_, _> = if internal.is_empty() {
        BTreeMap::new()
    } else {
        nodes.iter().map(|pc| (*pc, universe.clone())).collect()
    };
    let mut converged = false;
    for _ in 0..=nodes.len() {
        let mut changed = false;
        for &pc in &nodes {
            let mut incoming = preds[&pc].iter().map(|p| defined.get(p)).chain(
                exception_preds
                    .get(&pc)
                    .into_iter()
                    .flatten()
                    .map(|p| defined_before.get(p)),
            );
            let mut after = incoming.next()??.clone();
            for state in incoming {
                after = after.intersection(state?).copied().collect();
            }
            if !internal.is_empty() && defined_before[&pc] != after {
                defined_before.insert(pc, after.clone());
                changed = true;
            }
            for write in &ins[&pc].writes {
                after.extend(
                    write.register as usize..write.register as usize + write.kind.word_count(),
                );
            }
            if defined[&pc] != after {
                defined.insert(pc, after);
                changed = true;
            }
        }
        if !changed {
            converged = true;
            break;
        }
    }
    if !converged {
        return None;
    }
    let root = alloc.writes.first()?.register as usize;
    let mut possible: BTreeMap<_, BTreeSet<usize>> =
        nodes.iter().map(|pc| (*pc, BTreeSet::new())).collect();
    let mut definite: BTreeMap<_, BTreeSet<usize>> =
        nodes.iter().map(|pc| (*pc, universe.clone())).collect();
    possible.insert(allocation, BTreeSet::from([root]));
    definite.insert(allocation, BTreeSet::from([root]));
    let mut possible_before: BTreeMap<_, _> = if internal.is_empty() {
        BTreeMap::new()
    } else {
        nodes.iter().map(|pc| (*pc, BTreeSet::new())).collect()
    };
    let mut definite_before: BTreeMap<_, _> = if internal.is_empty() {
        BTreeMap::new()
    } else {
        nodes.iter().map(|pc| (*pc, universe.clone())).collect()
    };
    let alias_work = std::cell::Cell::new(2_000_000usize);
    let before_aliases = |pc: usize,
                          states: &BTreeMap<usize, BTreeSet<usize>>,
                          before_states: &BTreeMap<usize, BTreeSet<usize>>,
                          intersection: bool|
     -> Option<BTreeSet<usize>> {
        let mut incoming = preds.get(&pc)?.iter().map(|p| states.get(p)).chain(
            exception_preds
                .get(&pc)
                .into_iter()
                .flatten()
                .map(|p| before_states.get(p)),
        );
        let first = incoming.next()??;
        alias_work.set(alias_work.get().checked_sub(first.len() + 1)?);
        let mut value = first.clone();
        for next in incoming {
            let next = next?;
            alias_work.set(alias_work.get().checked_sub(value.len() + next.len() + 1)?);
            value = if intersection {
                value.intersection(next).copied().collect()
            } else {
                value.union(next).copied().collect()
            };
        }
        Some(value)
    };
    let mut aliases_converged = false;
    for _ in 0..=nodes.len() {
        let mut changed = false;
        for &pc in &nodes {
            let instruction = ins[&pc];
            for (states, before_states, intersection) in [
                (&mut possible, &mut possible_before, false),
                (&mut definite, &mut definite_before, true),
            ] {
                let mut value = before_aliases(pc, states, before_states, intersection)?;
                if !internal.is_empty() && before_states[&pc] != value {
                    before_states.insert(pc, value.clone());
                    changed = true;
                }
                let copy = matches!(instruction.opcode, 0x07..=0x09)
                    && instruction.reads.len() == 1
                    && value.contains(&(instruction.reads[0].register as usize));
                for write in &instruction.writes {
                    for word in
                        write.register as usize..write.register as usize + write.kind.word_count()
                    {
                        value.remove(&word);
                    }
                }
                if copy {
                    value.insert(instruction.writes.first()?.register as usize);
                }
                if states[&pc] != value {
                    states.insert(pc, value);
                    changed = true;
                }
            }
        }
        if !changed {
            aliases_converged = true;
            break;
        }
    }
    if !aliases_converged {
        return None;
    }
    let aliases = before_aliases(constructor, &possible, &possible_before, false)?;
    if aliases != before_aliases(constructor, &definite, &definite_before, true)? {
        return None;
    }
    let mut alias_mask_points = BTreeMap::from([(constructor, aliases.clone())]);
    let mut alias_moves = vec![];
    let mut first_reads = vec![];
    for &pc in &nodes {
        let i = ins[&pc];
        let aliases = before_aliases(pc, &possible, &possible_before, false)?;
        let exact_aliases = before_aliases(pc, &definite, &definite_before, true)?;
        alias_mask_points.insert(pc, aliases.clone());
        let incoming: Vec<_> = preds[&pc]
            .iter()
            .map(|p| defined.get(p))
            .chain(
                exception_preds
                    .get(&pc)
                    .into_iter()
                    .flatten()
                    .map(|p| defined_before.get(p)),
            )
            .collect::<Option<_>>()?;
        let first = incoming.first()?;
        let before = incoming.iter().skip(1).fold((*first).clone(), |acc, s| {
            acc.intersection(s).copied().collect()
        });
        let alias_move = matches!(i.opcode, 0x07..=0x09)
            && i.reads.len() == 1
            && aliases.contains(&(i.reads[0].register as usize));
        if alias_move {
            if !exact_aliases.contains(&(i.reads[0].register as usize)) {
                return None;
            }
            alias_moves.push(pc);
        }
        let reads: Vec<(usize, usize)> = if let Some(call) = calls.get(&pc) {
            let normalized: Vec<_> = call
                .receiver
                .iter()
                .chain(&call.arguments)
                .map(|r| {
                    (
                        r.register as usize,
                        if matches!(r.descriptor.as_ref(), "J" | "D") {
                            2
                        } else {
                            1
                        },
                    )
                })
                .collect();
            let expanded: Vec<_> = normalized.iter().flat_map(|&(r, n)| r..r + n).collect();
            if expanded
                != i.reads
                    .iter()
                    .flat_map(|r| r.register as usize..r.register as usize + r.kind.word_count())
                    .collect::<Vec<_>>()
            {
                return None;
            }
            normalized
        } else {
            i.reads
                .iter()
                .map(|r| (r.register as usize, r.kind.word_count()))
                .collect()
        };
        for (register, words) in reads {
            if words == 2 && before.contains(&register) != before.contains(&(register + 1)) {
                return None;
            }
            if !alias_move && (register..register + words).any(|r| aliases.contains(&r)) {
                return None;
            }
            if !alias_move && !(register..register + words).all(|r| before.contains(&r)) {
                first_reads.push(FirstRead {
                    pc,
                    register,
                    words,
                });
            }
        }
    }
    if !constructor_call
        .receiver
        .as_ref()
        .is_some_and(|r| aliases.contains(&(r.register as usize)))
    {
        return None;
    }
    let registers: BTreeSet<_> = alias_mask_points
        .values()
        .flat_map(|s| s.iter().copied())
        .collect();
    let mut alias_masks = vec![];
    for register in registers {
        let mut active: Option<(usize, usize)> = None;
        for (&pc, possible) in &alias_mask_points {
            alias_work.set(alias_work.get().checked_sub(1)?);
            if possible.contains(&register) {
                match active {
                    Some((from, end)) if end + ins.get(&end)?.width == pc => {
                        active = Some((from, pc))
                    }
                    Some((from, end)) => {
                        alias_masks.push((from, end, register));
                        active = Some((pc, pc));
                    }
                    None => active = Some((pc, pc)),
                }
            } else if let Some((from, end)) = active.take() {
                alias_masks.push((from, end, register));
            }
        }
        if let Some((from, end)) = active {
            alias_masks.push((from, end, register));
        }
    }
    let incoming: Vec<_> = preds[&constructor]
        .iter()
        .filter_map(|p| defined.get(p))
        .collect();
    let first = incoming.first()?;
    let at_constructor = incoming.iter().skip(1).fold((*first).clone(), |acc, s| {
        acc.intersection(s).copied().collect()
    });
    for arg in &constructor_call.arguments {
        let r = arg.register as usize;
        let words = if matches!(arg.descriptor.as_ref(), "J" | "D") {
            2
        } else {
            1
        };
        if words == 2 && at_constructor.contains(&r) != at_constructor.contains(&(r + 1)) {
            return None;
        }
        if (r..r + words).any(|r| aliases.contains(&r)) {
            return None;
        }
        if !(r..r + words).all(|r| at_constructor.contains(&r)) {
            first_reads.push(FirstRead {
                pc: constructor,
                register: r,
                words,
            });
        }
    }
    let mut semantic_references = vec![];
    for &pc in nodes
        .iter()
        .chain(std::iter::once(&allocation))
        .chain(std::iter::once(&constructor))
    {
        if let Some(r) = &ins[&pc].reference {
            let text = match r.kind {
                PoolKind::Type => class.symbols.types.get(r.index as usize)?.to_string(),
                PoolKind::Field => {
                    let &(o, t, n) = class.symbols.fields.get(r.index as usize)?;
                    format!(
                        "{}:{}:{}",
                        class.symbols.types.get(o as usize)?,
                        class.symbols.types.get(t as usize)?,
                        class.symbols.strings.get(n as usize)?
                    )
                }
                PoolKind::Method => format!("{:?}", calls.get(&pc)?),
                PoolKind::String => class.symbols.strings.get(r.index as usize)?.to_string(),
                _ => return None,
            };
            semantic_references.push((pc, text));
        }
    }
    Some(Plan {
        allocation,
        constructor,
        start,
        owned: nodes.iter().copied().collect(),
        terminal_islands,
        alias_moves,
        constructor_aliases: aliases.into_iter().collect(),
        alias_masks,
        definitely_written: at_constructor.into_iter().collect(),
        first_reads,
        calls: bound
            .calls
            .iter()
            .filter(|c| nodes.contains(&c.pc) || c.pc == constructor)
            .cloned()
            .collect(),
        nested_initializations,
        semantic_references,
        edges: edges.into_iter().collect(),
        exception_edges: exception_edges.into_iter().collect(),
        words: code.instructions.clone(),
        registers: code.registers,
        ins: code.ins,
        outs: code.outs,
        try_regions: code
            .try_regions
            .iter()
            .map(|r| {
                (
                    r.start,
                    r.end,
                    r.catches
                        .iter()
                        .map(|(t, pc)| (t.as_ref().map(|s| s.to_string()), *pc))
                        .collect(),
                )
            })
            .collect(),
    })
}

// Every erased instruction is authorized by an independently revalidated
// parent allocation plan. Original instruction widths and edges remain intact.
#[derive(Clone)]
pub(super) struct Projection {
    context: Rc<ProofContext>,
    steps: Vec<Plan>,
    // One projection certifies the one staged method rendered beneath it. The
    // certificate is keyed by the complete staged words and handler table.
    checked: StagedCertificate,
    plans: PlanMemo,
}
impl Projection {
    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) fn original_code(&self) -> &DexCode {
        self.context
            .source()
            .code
            .as_ref()
            .expect("validated projection source code")
    }
    #[cfg(test)]
    pub(super) fn extend(
        class: &DexClass,
        method: &DexMethod,
        previous: Option<&Self>,
        plan: &Plan,
    ) -> Option<Self> {
        Self::extend_with(class, method, previous, plan, None)
    }
    /// A root projection reuses the method's shared proof context when the
    /// caller already holds one for this exact method.
    pub(super) fn extend_with(
        class: &DexClass,
        method: &DexMethod,
        previous: Option<&Self>,
        plan: &Plan,
        context: Option<&Rc<ProofContext>>,
    ) -> Option<Self> {
        let mut projection = if let Some(previous) = previous {
            previous.checked_ir(class, method)?;
            let parent = previous.steps.last()?;
            if !parent.owned.contains(&plan.allocation)
                || !parent.owned.contains(&plan.constructor)
                || plan.owned.iter().any(|pc| !parent.owned.contains(pc))
            {
                return None;
            }
            if previous
                .prove_source(class, plan.allocation, plan.constructor)
                .as_deref()
                != Some(plan)
            {
                return None;
            }
            Self {
                context: previous.context.clone(),
                steps: previous.steps.clone(),
                checked: RefCell::new(None),
                plans: RefCell::new(BTreeMap::new()),
            }
        } else {
            let context = match context.filter(|context| context.matches(method)) {
                Some(context) => context.clone(),
                None => ProofContext::build(Rc::new(clone_method(method)?))?,
            };
            if !context.validate(class, method, plan) {
                return None;
            }
            Self {
                context,
                steps: vec![],
                checked: RefCell::new(None),
                plans: RefCell::new(BTreeMap::new()),
            }
        };
        if projection.steps.len() >= 16 {
            return None;
        }
        projection.steps.push(plan.clone());
        Some(projection)
    }
    fn erased(&self) -> BTreeSet<usize> {
        self.steps
            .iter()
            .flat_map(|plan| plan.alias_moves.iter().copied())
            .collect()
    }
    // The erased set of a projection is fixed, so each (allocation,
    // constructor) proof over its source is computed once.
    fn prove_source(
        &self,
        class: &DexClass,
        allocation: usize,
        constructor: usize,
    ) -> Option<Rc<Plan>> {
        let key = (allocation, constructor);
        if let Some(plan) = self.plans.borrow().get(&key) {
            return plan.clone();
        }
        let plan = self
            .prove_source_with(class, allocation, constructor, &self.erased())
            .map(Rc::new);
        self.plans.borrow_mut().insert(key, plan.clone());
        plan
    }
    fn prove_source_with(
        &self,
        class: &DexClass,
        allocation: usize,
        constructor: usize,
        erased: &BTreeSet<usize>,
    ) -> Option<Plan> {
        let analysis = self.context.analysis(class)?;
        let source = self.context.source();
        let code = source.code.as_ref()?;
        let mut ir = (*self.context.ir).clone();
        Self::erase_ir(&mut ir, erased)?;
        let mut words = code.instructions.clone();
        for &pc in erased {
            let i = ir.instructions.iter().find(|i| i.pc == pc)?;
            words.get_mut(pc..pc.checked_add(i.width)?)?.fill(0);
        }
        let projected = DexMethod {
            declaring_type: source.declaring_type.clone(),
            name: source.name.clone(),
            return_type: source.return_type.clone(),
            parameters: source.parameters.clone(),
            thrown_types: source.thrown_types.clone(),
            access_flags: source.access_flags,
            code: Some(DexCode {
                registers: code.registers,
                ins: code.ins,
                outs: code.outs,
                tries: code.tries,
                try_regions: code.try_regions.clone(),
                instructions: words,
                offset: code.offset,
            }),
        };
        prove_cached(
            class,
            &projected,
            allocation,
            constructor,
            ProofInputs {
                ir: &ir,
                cfg: &analysis.cfg,
                bound: &analysis.bound,
                bindings: &analysis.bindings,
            },
        )
    }
    fn erase_ir(ir: &mut DecodedMethod, erased: &BTreeSet<usize>) -> Option<()> {
        for &pc in erased {
            let i = ir.instructions.iter_mut().find(|i| i.pc == pc)?;
            if !matches!(i.opcode, 0x07..=0x09)
                || i.reads.len() != 1
                || i.writes.len() != 1
                || i.branch_target.is_some()
                || i.payload_target.is_some()
                || i.reference.is_some()
            {
                return None;
            }
            i.opcode = 0;
            i.reads.clear();
            i.writes.clear();
        }
        Some(())
    }
    pub(super) fn checked_ir(
        &self,
        class: &DexClass,
        method: &DexMethod,
    ) -> Option<Rc<DecodedMethod>> {
        if self.steps.is_empty() || self.steps.len() > 16 {
            return None;
        }
        let source = self.context.source();
        if !same_header(method, source) {
            return None;
        }
        let original = source.code.as_ref()?;
        let staged = method.code.as_ref()?;
        if staged.registers != original.registers
            || staged.ins != original.ins
            || staged.outs != original.outs
            || staged.tries != original.tries
            || staged.offset != original.offset
            || staged.instructions.len() != original.instructions.len()
        {
            return None;
        }
        if let Some((words, regions, certificate)) = self.checked.borrow().as_ref()
            && *words == staged.instructions
            && *regions == staged.try_regions
        {
            return certificate.clone();
        }
        let certificate = self.certify(class, original, staged);
        *self.checked.borrow_mut() = Some((
            staged.instructions.clone(),
            staged.try_regions.clone(),
            certificate.clone(),
        ));
        certificate
    }
    fn certify(
        &self,
        class: &DexClass,
        original: &DexCode,
        staged: &DexCode,
    ) -> Option<Rc<DecodedMethod>> {
        let mut erased = BTreeSet::new();
        for plan in &self.steps {
            if self.prove_source_with(class, plan.allocation, plan.constructor, &erased)? != *plan {
                return None;
            }
            erased.extend(plan.alias_moves.iter().copied());
        }
        let mut ir = (*self.context.ir).clone();
        let mut projected_words = original.instructions.clone();
        for &pc in &erased {
            let i = ir.instructions.iter().find(|i| i.pc == pc)?;
            projected_words
                .get_mut(pc..pc.checked_add(i.width)?)?
                .fill(0);
        }
        if projected_words != staged.instructions {
            return None;
        }
        let expected_regions: Vec<_> = original
            .try_regions
            .iter()
            .filter(|region| {
                !self.steps.iter().any(|plan| {
                    let width = ir
                        .instructions
                        .iter()
                        .find(|i| i.pc == plan.constructor)
                        .map(|i| i.width)
                        .unwrap_or(usize::MAX);
                    region.start as usize <= plan.allocation
                        && plan
                            .constructor
                            .checked_add(width)
                            .is_some_and(|end| end <= region.end as usize)
                })
            })
            .cloned()
            .collect();
        if expected_regions != staged.try_regions {
            return None;
        }
        Self::erase_ir(&mut ir, &erased)?;
        Some(Rc::new(ir))
    }
    pub(super) fn prove(
        &self,
        class: &DexClass,
        method: &DexMethod,
        allocation: usize,
        constructor: usize,
    ) -> Option<Rc<Plan>> {
        self.checked_ir(class, method)?;
        self.prove_source(class, allocation, constructor)
    }
    pub(super) fn validate(&self, class: &DexClass, method: &DexMethod, plan: &Plan) -> bool {
        self.prove(class, method, plan.allocation, plan.constructor)
            .as_deref()
            == Some(plan)
    }
}
