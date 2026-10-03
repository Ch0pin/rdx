//! Pure terminal tails may be physically inside an address-ordered loop.
//! This proves an original edge and complete terminating path, not a new loop
//! entry. Emission must use the actual incoming typed frame and reject pending
//! result/allocation/constructor state before rendering the terminal path.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_ir::DecodedMethod,
    native_ssa::{DefinitionKind, SsaMethod},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SharedTerminalExit {
    pub from: usize,
    pub target: usize,
    pub interval: Range<usize>,
    pub instructions: Vec<usize>,
    pub edges: Vec<(usize, usize)>,
    pub incoming_edges: Vec<(usize, usize)>,
    pub live_in_registers: Vec<u16>,
    pub raw_words: Vec<u16>,
    pub widths: Vec<(usize, usize)>,
    pub targets: Vec<(usize, Option<usize>)>,
    pub registers: u16,
    pub ins: u16,
}

pub(super) fn validate(code: &DexCode, plan: &SharedTerminalExit) -> bool {
    prove(code, plan.from, plan.target, plan.interval.clone()).as_ref() == Some(plan)
}

pub(super) fn prove(
    code: &DexCode,
    from: usize,
    target: usize,
    interval: Range<usize>,
) -> Option<SharedTerminalExit> {
    if code.tries != 0
        || !code.try_regions.is_empty()
        || code.instructions.len() > 4096
        || target <= interval.start
        || !interval.contains(&target)
        || from == target
    {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let by_pc: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    let source = by_pc.get(&from)?;
    if !matches!(source.opcode, 0x28..=0x2a | 0x32..=0x3d) || source.branch_target != Some(target) {
        return None;
    }
    // Cheap strict opcode admission before canonical CFG/SSA construction.
    let mut nodes = Vec::new();
    let mut pc = target;
    for _ in 0..32 {
        let ins = by_pc.get(&pc)?;
        nodes.push(pc);
        match ins.opcode {
            0x0e..=0x11 => break,
            0x00..=0x09 | 0x12..=0x19 if !ins.may_throw => pc += ins.width,
            0x28..=0x2a => {
                let next = ins.branch_target?;
                if next <= pc {
                    return None;
                }
                pc = next;
            }
            _ => return None,
        }
    }
    if !matches!(by_pc.get(nodes.last()?)?.opcode, 0x0e..=0x11) {
        return None;
    }
    let owned: BTreeSet<_> = nodes.iter().copied().collect();
    if owned.contains(&from) {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 2048 {
        return None;
    }
    let mut normal = BTreeMap::new();
    for block in &cfg.blocks {
        for (index, &pc) in block.instructions.iter().enumerate() {
            let next = if let Some(&next) = block.instructions.get(index + 1) {
                vec![next]
            } else {
                block
                    .successors
                    .iter()
                    .filter(|e| e.kind == EdgeKind::Normal)
                    .map(|e| cfg.blocks[e.target].start)
                    .collect()
            };
            normal.insert(pc, next);
        }
        if block
            .successors
            .iter()
            .any(|e| e.kind == EdgeKind::Exceptional)
        {
            return None;
        }
    }
    if !normal.get(&from)?.contains(&target) {
        return None;
    }
    let mut edges = Vec::new();
    for (index, &pc) in nodes.iter().enumerate() {
        let expected = nodes
            .get(index + 1)
            .copied()
            .into_iter()
            .collect::<Vec<_>>();
        if normal.get(&pc)? != &expected {
            return None;
        }
        edges.extend(expected.into_iter().map(|to| (pc, to)));
    }
    // Other entries into a pure path are harmless only as separately owned
    // terminal copies. Record every exact incoming edge for emission reproof.
    let mut incoming_edges = Vec::new();
    for (&pc, next) in &normal {
        if !owned.contains(&pc) {
            incoming_edges.extend(
                next.iter()
                    .filter(|to| owned.contains(to))
                    .map(|&to| (pc, to)),
            );
        }
    }
    let ssa = SsaMethod::build(code, &ir, &cfg).ok()?;
    let ssa_instructions: BTreeMap<_, _> = ssa
        .instructions
        .iter()
        .map(|instruction| (instruction.pc, instruction))
        .collect();
    let phis: BTreeMap<_, _> = ssa.phis.iter().map(|p| (p.result, p)).collect();
    let mut live_in = BTreeSet::new();
    for instruction in ssa.instructions.iter().filter(|i| owned.contains(&i.pc)) {
        for read in &instruction.reads {
            let mut pending = read.words.clone();
            let mut seen = BTreeSet::new();
            while let Some(value) = pending.pop() {
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
                        if !owned.contains(&pc) {
                            live_in.insert(read.register);
                            pending.extend(
                                ssa_instructions
                                    .get(&pc)?
                                    .reads
                                    .iter()
                                    .flat_map(|read| read.words.iter().copied()),
                            );
                        }
                    }
                    DefinitionKind::Phi { .. } => {
                        pending.extend(phis.get(&value)?.incoming.iter().map(|(_, v)| *v))
                    }
                }
            }
        }
    }
    Some(SharedTerminalExit {
        from,
        target,
        interval,
        instructions: nodes.clone(),
        edges,
        incoming_edges,
        live_in_registers: live_in.into_iter().collect(),
        raw_words: code.instructions.clone(),
        widths: std::iter::once(from)
            .chain(nodes.iter().copied())
            .map(|pc| (pc, by_pc[&pc].width))
            .collect(),
        targets: std::iter::once(from)
            .chain(nodes.iter().copied())
            .map(|pc| (pc, by_pc[&pc].branch_target))
            .collect(),
        registers: code.registers,
        ins: code.ins,
    })
}
