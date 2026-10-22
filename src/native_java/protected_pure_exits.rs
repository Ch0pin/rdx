//! Exact normal exits through nonthrowing shared tails outside one DEX try.
use crate::{
    native_cfg::ControlFlowGraph,
    native_dex::{DexCode, DexTryRegion},
    native_ir::DecodedMethod,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub(crate) struct Plan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) join: usize,
    pub(crate) normal: BTreeSet<usize>,
    pub(crate) tails: BTreeMap<usize, Vec<usize>>,
    pub(crate) layout: Vec<(usize, usize, Option<usize>)>,
    frame: (u16, u16, u16, u16),
    words: Vec<u16>,
    regions: Vec<DexTryRegion>,
}
impl Plan {
    pub(crate) fn matches(&self, code: &DexCode) -> bool {
        self.frame == (code.registers, code.ins, code.outs, code.tries)
            && self.words == code.instructions
            && self.regions == code.try_regions
    }
}

pub(crate) fn prove(code: &DexCode, region: &DexTryRegion) -> Option<Plan> {
    if code.instructions.len() > 4096
        || code.registers > 256
        || usize::from(code.tries) != code.try_regions.len()
        || code.try_regions.len() > 16
        || code
            .try_regions
            .iter()
            .map(|r| r.catches.len())
            .sum::<usize>()
            > 64
        || region.catches.len() != 1
        || !code.try_regions.contains(region)
    {
        return None;
    }
    let start = region.start as usize;
    let end = region.end as usize;
    let handler = region.catches[0].1 as usize;
    let ir = DecodedMethod::decode(code).ok()?;
    let at: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
    if start >= end
        || !at.contains_key(&start)
        || !at.contains_key(&end)
        || at.get(&handler)?.opcode != 0x0d
        || code
            .try_regions
            .iter()
            .any(|r| r != region && (r.start as usize) < end && start < r.end as usize)
    {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    if cfg.blocks.len() > 512 {
        return None;
    }
    let mut normal = BTreeSet::new();
    let mut exits = BTreeSet::new();
    let mut pending = vec![start];
    while let Some(pc) = pending.pop() {
        if !(start..end).contains(&pc) {
            exits.insert(pc);
            continue;
        }
        if !normal.insert(pc) {
            continue;
        }
        let i = at.get(&pc)?;
        if matches!(i.opcode, 0x0d..=0x11 | 0x1d..=0x1e | 0x27 | 0x2b..=0x2c)
            || i.payload_target.is_some()
            || i.reads.iter().chain(&i.writes).any(|o| {
                usize::from(o.register) + o.kind.word_count() > usize::from(code.registers)
            })
        {
            return None;
        }
        if let Some(target) = i.branch_target {
            pending.push(target);
        }
        if !matches!(i.opcode, 0x28..=0x2a) {
            pending.push(pc + i.width);
        }
    }
    if exits.len() > 32 || !exits.iter().any(|pc| *pc < start) {
        return None;
    }
    let mut tails = BTreeMap::new();
    let mut all_tail_nodes = BTreeSet::new();
    let mut frontier = None;
    for entry in exits {
        let mut path = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pc = entry;
        loop {
            let i = at.get(&pc)?;
            if normal.contains(&pc) || (start..end).contains(&pc) || !seen.insert(pc) {
                return None;
            }
            let pure = !i.may_throw
                && i.payload_target.is_none()
                && matches!(i.opcode, 0x00..=0x09 | 0x12..=0x19 | 0x28..=0x2a)
                && (i.opcode != 0 || code.instructions[pc] == 0);
            if !pure {
                if pc < end
                    || matches!(i.opcode, 0x0a..=0x0d)
                    || frontier.is_some_and(|other| other != pc)
                {
                    return None;
                }
                frontier = Some(pc);
                break;
            }
            if path.len() == 16
                || code
                    .try_regions
                    .iter()
                    .any(|r| r.start as usize <= pc && pc < r.end as usize)
                || i.reads.iter().chain(&i.writes).any(|o| {
                    usize::from(o.register) + o.kind.word_count() > usize::from(code.registers)
                })
            {
                return None;
            }
            path.push(pc);
            all_tail_nodes.insert(pc);
            pc = i.branch_target.unwrap_or(pc + i.width);
        }
        tails.insert(entry, path);
    }
    let join = frontier?;
    if all_tail_nodes.is_empty()
        || code
            .try_regions
            .iter()
            .flat_map(|r| r.catches.iter())
            .any(|(_, target)| {
                all_tail_nodes.contains(&(*target as usize)) || normal.contains(&(*target as usize))
            })
    {
        return None;
    }
    // Complete original normal and exceptional handler closure may join the
    // continuation, but cannot reenter the normal protected body or its tails.
    let mut pending = vec![*cfg.block_at.get(&handler)?];
    let mut seen = BTreeSet::new();
    let mut work = 16_384usize;
    while let Some(block) = pending.pop() {
        if work == 0 {
            return None;
        }
        work -= 1;
        if !seen.insert(block) {
            continue;
        }
        let block = cfg.blocks.get(block)?;
        if block.instructions.len() > work {
            return None;
        }
        work -= block.instructions.len();
        if block
            .instructions
            .iter()
            .any(|pc| normal.contains(pc) || all_tail_nodes.contains(pc))
        {
            return None;
        }
        pending.extend(block.successors.iter().map(|e| e.target));
    }
    Some(Plan {
        start,
        end,
        join,
        normal,
        tails,
        layout: ir
            .instructions
            .iter()
            .map(|i| (i.pc, i.width, i.branch_target))
            .collect(),
        frame: (code.registers, code.ins, code.outs, code.tries),
        words: code.instructions.clone(),
        regions: code.try_regions.clone(),
    })
}
