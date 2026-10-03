//! Bounded terminal exception tails outside the original loop interval.
//! This proves ownership only; emission must retain original exception-frame
//! snapshots and recheck renderer widths/targets and allocation/result state.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::{DexCode, DexTryRegion},
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DetachedHandlerTail {
    pub handler: usize,
    pub forbidden_loop: Range<usize>,
    pub instructions: Vec<usize>,
    pub raw_words: Vec<(usize, Vec<u16>)>,
    pub normal_edges: Vec<(usize, usize)>,
    pub targets: Vec<(usize, Option<usize>)>,
    pub incoming_exception_edges: Vec<(usize, usize)>,
    pub live_in_registers: Vec<u16>,
    pub owners: Vec<DexTryRegion>,
}

pub(super) fn validate(code: &DexCode, plan: &DetachedHandlerTail) -> bool {
    prove(code, plan.handler, plan.forbidden_loop.clone()).as_ref() == Some(plan)
}

pub(super) fn prove(
    code: &DexCode,
    handler: usize,
    forbidden_loop: Range<usize>,
) -> Option<DetachedHandlerTail> {
    if code.instructions.len() > 4096
        || forbidden_loop.start >= forbidden_loop.end
        || handler < forbidden_loop.end
    {
        return None;
    }
    if *code.instructions.get(handler)? as u8 != 0x0d {
        return None;
    }
    let owners: Vec<_> = code
        .try_regions
        .iter()
        .filter(|region| region.catches.iter().any(|(_, pc)| *pc as usize == handler))
        .cloned()
        .collect();
    if owners.is_empty() {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 2048 {
        return None;
    }
    let by_pc: BTreeMap<_, _> = ir
        .instructions
        .iter()
        .map(|instruction| (instruction.pc, instruction))
        .collect();
    if by_pc.get(&handler)?.opcode != 0x0d {
        return None;
    }
    let mut normal = BTreeMap::new();
    let mut exceptional = vec![];
    for block in &cfg.blocks {
        for (index, &pc) in block.instructions.iter().enumerate() {
            let next = if let Some(&next) = block.instructions.get(index + 1) {
                vec![next]
            } else {
                block
                    .successors
                    .iter()
                    .filter(|edge| edge.kind == EdgeKind::Normal)
                    .map(|edge| cfg.blocks[edge.target].start)
                    .collect()
            };
            normal.insert(pc, next);
        }
        let source = *block.instructions.last()?;
        exceptional.extend(
            block
                .successors
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Exceptional)
                .map(|edge| (source, cfg.blocks[edge.target].start)),
        );
    }
    let mut nodes = BTreeSet::new();
    let mut pending = vec![handler];
    while let Some(pc) = pending.pop() {
        if nodes.contains(&pc) {
            continue;
        }
        if nodes.len() == 32 || forbidden_loop.contains(&pc) {
            return None;
        }
        let instruction = by_pc.get(&pc)?;
        if (pc != handler && instruction.opcode == 0x0d)
            || matches!(instruction.opcode, 0x1d | 0x1e | 0x22..=0x26 | 0x2b | 0x2c)
            || code
                .try_regions
                .iter()
                .any(|region| region.start as usize <= pc && pc < region.end as usize)
        {
            return None;
        }
        nodes.insert(pc);
        let successors = normal.get(&pc)?;
        if successors.is_empty() && !matches!(instruction.opcode, 0x0e..=0x11 | 0x27) {
            return None;
        }
        // Initial tranche needs only forward DAGs. This rejects cycles and
        // address-backward sharing without relying on mutable loop metadata.
        if successors.iter().any(|&to| to <= pc) {
            return None;
        }
        pending.extend(successors.iter().copied());
    }
    // No ordinary path may enter a copied catch tail. Shared exceptional roots
    // are allowed only at its original move-exception, never in its interior.
    if normal
        .iter()
        .any(|(from, next)| !nodes.contains(from) && next.iter().any(|to| nodes.contains(to)))
        || exceptional
            .iter()
            .any(|(from, to)| nodes.contains(from) || (nodes.contains(to) && *to != handler))
    {
        return None;
    }
    for &pc in &nodes {
        if matches!(by_pc[&pc].opcode, 0x0a..=0x0c) {
            let predecessors: Vec<_> = normal
                .iter()
                .filter_map(|(&from, next)| next.contains(&pc).then_some(from))
                .collect();
            if predecessors.len() != 1 {
                return None;
            }
            let producer = by_pc[&predecessors[0]];
            if producer.pc + producer.width != pc
                || !matches!(producer.opcode, 0x6e..=0x72 | 0x74..=0x78 | 0xfa..=0xfd)
            {
                return None;
            }
        }
    }
    let ssa = SsaMethod::build(code, &ir, &cfg).ok()?;
    let ssa_instructions: BTreeMap<_, _> = ssa
        .instructions
        .iter()
        .map(|instruction| (instruction.pc, instruction))
        .collect();
    let mut live_in = BTreeSet::new();
    let phis: BTreeMap<_, _> = ssa.phis.iter().map(|phi| (phi.result, phi)).collect();
    for instruction in ssa
        .instructions
        .iter()
        .filter(|instruction| nodes.contains(&instruction.pc))
    {
        for read in &instruction.reads {
            let mut values = read.words.clone();
            let mut seen = BTreeSet::new();
            while let Some(value) = values.pop() {
                if !seen.insert(value) {
                    continue;
                }
                if seen.len() > 256 {
                    return None;
                }
                match ssa.definitions.get(value)?.kind {
                    DefinitionKind::Undefined => return None,
                    DefinitionKind::Parameter => {
                        live_in.insert(read.register);
                    }
                    DefinitionKind::Instruction { pc, .. } => {
                        if !nodes.contains(&pc) {
                            live_in.insert(read.register);
                            // A definition outside the tail is not sufficient:
                            // an upstream copy/phi can still read Undefined.
                            // Validate its complete local SSA operand roots.
                            values.extend(
                                ssa_instructions
                                    .get(&pc)?
                                    .reads
                                    .iter()
                                    .flat_map(|read| read.words.iter().copied()),
                            );
                        }
                    }
                    DefinitionKind::Phi { .. } => {
                        values.extend(phis.get(&value)?.incoming.iter().map(|(_, value)| *value))
                    }
                }
            }
        }
    }
    let targets = nodes
        .iter()
        .map(|&pc| (pc, by_pc[&pc].branch_target))
        .collect();
    let normal_edges = nodes
        .iter()
        .flat_map(|&pc| normal[&pc].iter().map(move |&to| (pc, to)))
        .collect();
    let incoming_exception_edges = exceptional
        .into_iter()
        .filter(|(_, to)| *to == handler)
        .collect();
    let raw_words = nodes
        .iter()
        .map(|&pc| (pc, code.instructions[pc..pc + by_pc[&pc].width].to_vec()))
        .collect();
    Some(DetachedHandlerTail {
        handler,
        forbidden_loop,
        instructions: nodes.into_iter().collect(),
        raw_words,
        normal_edges,
        targets,
        incoming_exception_edges,
        live_in_registers: live_in.into_iter().collect(),
        owners,
    })
}
