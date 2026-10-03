//! One immutable forward constructor-argument region; frame/type proofs stay in emission.
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
    pub definitely_written: Vec<usize>,
    pub first_reads: Vec<FirstRead>,
    pub calls: Vec<BoundCall>,
    pub semantic_references: Vec<(usize, String)>,
    pub edges: Vec<(usize, Vec<usize>)>,
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
    let code = method.code.as_ref()?;
    if code.instructions.len() > 4096 || code.registers > 256 || allocation >= constructor {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 128 {
        return None;
    }
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
    let nodes: BTreeSet<_> = ins
        .keys()
        .copied()
        .filter(|pc| start <= *pc && *pc < constructor)
        .collect();
    if nodes.is_empty() || nodes.len() > 256 {
        return None;
    }
    if nodes.iter().any(|pc| {
        matches!(
            ins[pc].opcode,
            0x0e..=0x11 | 0x1a | 0x1b | 0x1d | 0x1e | 0x22 | 0x2b | 0x2c
        ) || ins[pc].payload_target.is_some()
            || (ins[pc].opcode == 0 && code.instructions[*pc] != 0)
    }) {
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
    // Complete original normal-edge identity, including fallthrough, with one entry and endpoint.
    for (&from, ts) in &edges {
        for &to in ts {
            if nodes.contains(&from) {
                if to <= from || (!nodes.contains(&to) && to != constructor) {
                    return None;
                }
            } else if (nodes.contains(&to) || to == constructor)
                && (from != allocation || to != start)
            {
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
            || (t.start as usize <= allocation && constructor + ctor.width <= t.end as usize))
        {
            return None;
        }
        if t.catches
            .iter()
            .any(|(_, pc)| start <= *pc as usize && *pc as usize <= constructor)
        {
            return None;
        }
    }
    // Constructor identity comes from raw SSA, not address proximity or a Java type guess.
    let analysis = MethodAnalysis::build(class, method).ok()?;
    let constructors = analysis.constructors().ok()?;
    let exact: Vec<_> = constructors
        .bindings
        .iter()
        .filter(|b| matches!(b.origin,ConstructorOrigin::Allocation{pc,..} if pc==allocation))
        .collect();
    let [binding] = exact[..] else {
        return None;
    };
    if binding.invoke_pc != constructor {
        return None;
    }
    let bound = BoundCalls::bind(code, &ir, &class.symbols).ok()?;
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
    let mut defined = BTreeMap::<usize, BTreeSet<usize>>::new();
    let mut first_reads = vec![];
    let mut aliases = BTreeSet::from([alloc.writes.first()?.register as usize]);
    let mut transferred = false;
    for &pc in &nodes {
        let i = ins[&pc];
        if transferred && (i.branch_target.is_some() || edges[&pc].len() != 1) {
            return None;
        }
        let mut before = if pc == start {
            BTreeSet::new()
        } else {
            let incoming: Vec<_> = preds[&pc].iter().filter_map(|p| defined.get(p)).collect();
            let first = incoming.first()?;
            incoming.iter().skip(1).fold((*first).clone(), |acc, s| {
                acc.intersection(s).copied().collect()
            })
        };
        let alias_move = matches!(i.opcode, 0x07..=0x09)
            && i.reads.len() == 1
            && aliases.contains(&(i.reads[0].register as usize));
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
        for w in &i.writes {
            for r in w.register as usize..w.register as usize + w.kind.word_count() {
                before.insert(r);
                aliases.remove(&r);
            }
        }
        if alias_move {
            aliases.insert(i.writes.first()?.register as usize);
            transferred = true;
        }
        defined.insert(pc, before);
    }
    if !constructor_call
        .receiver
        .as_ref()
        .is_some_and(|r| aliases.contains(&(r.register as usize)))
    {
        return None;
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
        definitely_written: at_constructor.into_iter().collect(),
        first_reads,
        calls: bound
            .calls
            .into_iter()
            .filter(|c| nodes.contains(&c.pc) || c.pc == constructor)
            .collect(),
        semantic_references,
        edges: edges.into_iter().collect(),
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
