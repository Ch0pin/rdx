//! One immutable constructor-argument region; frame/type proofs stay in emission.
use crate::{
    native_calls::{BoundCall, BoundCalls},
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_constructors::ConstructorOrigin,
    native_dex::{DexClass, DexMethod},
    native_ir::{DecodedMethod, PoolKind},
    native_method::MethodAnalysis,
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FirstRead {
    pub pc: usize,
    pub register: usize,
    pub words: usize,
}
type ProtectedDispatch = (u32, u32, Vec<(Option<String>, u32)>);
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
pub(super) fn validate(class: &DexClass, method: &DexMethod, plan: &Plan) -> bool {
    prove(class, method, plan.allocation, plan.constructor).as_ref() == Some(plan)
}
pub(super) fn prove(
    class: &DexClass,
    method: &DexMethod,
    allocation: usize,
    constructor: usize,
) -> Option<Plan> {
    prove_inner(class, method, allocation, constructor, None)
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
        if nodes.len() > 256 {
            return None;
        }
    }
    if nodes.is_empty() || nodes.len() > 256 {
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
            if nodes.len() > 256 {
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
    if nodes.is_empty() || nodes.len() > 256 {
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
    if nested_initializations.len() > 16 {
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
    source: std::rc::Rc<DexMethod>,
    steps: Vec<Plan>,
}
impl Projection {
    pub(super) fn original_code(&self) -> &crate::native_dex::DexCode {
        self.source
            .code
            .as_ref()
            .expect("validated projection source code")
    }

    pub(super) fn extend(
        class: &DexClass,
        method: &DexMethod,
        previous: Option<&Self>,
        plan: &Plan,
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
                .prove_source(class, plan.allocation, plan.constructor, &previous.erased())?
                .as_ref()
                != plan
            {
                return None;
            }
            previous.clone()
        } else {
            if !validate(class, method, plan) {
                return None;
            }
            let code = method.code.as_ref()?;
            Self {
                source: std::rc::Rc::new(DexMethod {
                    declaring_type: method.declaring_type.clone(),
                    name: method.name.clone(),
                    return_type: method.return_type.clone(),
                    parameters: method.parameters.clone(),
                    thrown_types: method.thrown_types.clone(),
                    access_flags: method.access_flags,
                    code: Some(crate::native_dex::DexCode {
                        registers: code.registers,
                        ins: code.ins,
                        outs: code.outs,
                        tries: code.tries,
                        try_regions: code.try_regions.clone(),
                        instructions: code.instructions.clone(),
                        offset: code.offset,
                    }),
                }),
                steps: vec![],
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
    fn original_inputs(&self, class: &DexClass) -> Option<ProjectionInputs> {
        let code = self.source.code.as_ref()?;
        if code.instructions.len() > 4096 || code.registers > 256 {
            return None;
        }
        let analysis = crate::native_method::MethodAnalysis::build(class, &self.source).ok()?;
        let constructors = analysis.constructors().ok()?;
        let bound = BoundCalls::bind(code, analysis.instructions(), &class.symbols).ok()?;
        Some(ProjectionInputs {
            ir: analysis.instructions().clone(),
            cfg: analysis.blocks().clone(),
            bound,
            bindings: constructors.bindings,
        })
    }
    fn prove_source(
        &self,
        class: &DexClass,
        allocation: usize,
        constructor: usize,
        erased: &BTreeSet<usize>,
    ) -> Option<Box<Plan>> {
        self.prove_source_with(
            class,
            allocation,
            constructor,
            erased,
            &self.original_inputs(class)?,
        )
    }
    fn prove_source_with(
        &self,
        class: &DexClass,
        allocation: usize,
        constructor: usize,
        erased: &BTreeSet<usize>,
        inputs: &ProjectionInputs,
    ) -> Option<Box<Plan>> {
        let code = self.source.code.as_ref()?;
        let mut ir = inputs.ir.clone();
        Self::erase_ir(&mut ir, erased)?;
        let mut words = code.instructions.clone();
        for &pc in erased {
            let i = ir.instructions.iter().find(|i| i.pc == pc)?;
            words.get_mut(pc..pc.checked_add(i.width)?)?.fill(0);
        }
        let source = &self.source;
        let projected = DexMethod {
            declaring_type: source.declaring_type.clone(),
            name: source.name.clone(),
            return_type: source.return_type.clone(),
            parameters: source.parameters.clone(),
            thrown_types: source.thrown_types.clone(),
            access_flags: source.access_flags,
            code: Some(crate::native_dex::DexCode {
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
                cfg: &inputs.cfg,
                bound: &inputs.bound,
                bindings: &inputs.bindings,
            },
        )
        .map(Box::new)
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
    pub(super) fn checked_ir(&self, class: &DexClass, method: &DexMethod) -> Option<DecodedMethod> {
        if self.steps.is_empty() || self.steps.len() > 16 {
            return None;
        }
        let source = &self.source;
        if method.declaring_type != source.declaring_type
            || method.name != source.name
            || method.return_type != source.return_type
            || method.parameters != source.parameters
            || method.thrown_types != source.thrown_types
            || method.access_flags != source.access_flags
        {
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
        let inputs = self.original_inputs(class)?;
        let mut erased = BTreeSet::new();
        for plan in &self.steps {
            if self
                .prove_source_with(class, plan.allocation, plan.constructor, &erased, &inputs)?
                .as_ref()
                != plan
            {
                return None;
            }
            erased.extend(plan.alias_moves.iter().copied());
        }
        let mut ir = inputs.ir;
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
        Some(ir)
    }
    pub(super) fn prove(
        &self,
        class: &DexClass,
        method: &DexMethod,
        allocation: usize,
        constructor: usize,
    ) -> Option<Plan> {
        self.checked_ir(class, method)?;
        self.prove_source(class, allocation, constructor, &self.erased())
            .map(|p| *p)
    }
    pub(super) fn validate(&self, class: &DexClass, method: &DexMethod, plan: &Plan) -> bool {
        self.prove(class, method, plan.allocation, plan.constructor)
            .as_ref()
            == Some(plan)
    }
}

struct ProjectionInputs {
    ir: DecodedMethod,
    cfg: ControlFlowGraph,
    bound: BoundCalls,
    bindings: Vec<crate::native_constructors::ConstructorBinding>,
}
