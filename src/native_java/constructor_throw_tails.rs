//! Exact normal-CFG terminal throw tails used when joining constructor prologues.
//! No receiver read, alias, exception or allocation permission is granted here.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
    native_ir::DecodedMethod,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub start: usize,
    pub end: usize,
    pub owned: BTreeSet<usize>,
    pub layout: Vec<(usize, usize, Option<usize>)>,
}

pub(super) fn select(code: &DexCode) -> Vec<Plan> {
    fn attempt(code: &DexCode) -> Option<Vec<Plan>> {
        if code.instructions.len() > 8192 {
            return None;
        }
        let ir = DecodedMethod::decode(code).ok()?;
        let at: BTreeMap<_, _> = ir.instructions.iter().map(|i| (i.pc, i)).collect();
        let cfg = ControlFlowGraph::build(code).ok()?;
        if cfg.blocks.len() > 512 {
            return None;
        }
        let mut output = Vec::new();
        let mut work = 0;
        let mut total = 0;
        for start in 0..cfg.blocks.len() {
            let mut pending = vec![(start, false)];
            let mut colors = BTreeMap::new();
            let mut blocks = BTreeSet::new();
            let mut valid = true;
            while let Some((b, leaving)) = pending.pop() {
                work += 1;
                if work > 100_000 {
                    return None;
                }
                if leaving {
                    colors.insert(b, 2);
                    continue;
                }
                match colors.get(&b) {
                    Some(1) => {
                        valid = false;
                        break;
                    }
                    Some(2) => continue,
                    _ => {}
                }
                let block = &cfg.blocks[b];
                if block.instructions.iter().any(|pc| {
                    matches!(at[pc].opcode, 0x1d | 0x1e | 0x2b | 0x2c)
                        || code.try_regions.iter().any(|region| {
                            (region.start as usize <= *pc && *pc < region.end as usize)
                                || region
                                    .catches
                                    .iter()
                                    .any(|(_, handler)| *handler as usize == *pc)
                        })
                }) {
                    valid = false;
                    break;
                }
                blocks.insert(b);
                if blocks.len() > 128 {
                    valid = false;
                    break;
                }
                let last = *block.instructions.last()?;
                if block.successors.is_empty() {
                    if at[&last].opcode != 0x27 {
                        valid = false;
                        break;
                    }
                    colors.insert(b, 2);
                    continue;
                }
                colors.insert(b, 1);
                pending.push((b, true));
                for edge in &block.successors {
                    if edge.kind != EdgeKind::Normal {
                        valid = false;
                        break;
                    }
                    pending.push((edge.target, false));
                }
                if !valid {
                    break;
                }
            }
            if !valid {
                continue;
            }
            let owned: BTreeSet<_> = blocks
                .iter()
                .flat_map(|b| cfg.blocks[*b].instructions.iter().copied())
                .collect();
            if owned.len() > 512 {
                continue;
            }
            total += owned.len();
            if total > 8192 {
                return None;
            }
            let end = owned.iter().map(|pc| pc + at[pc].width).max()?;
            let layout = owned
                .iter()
                .map(|pc| (*pc, at[pc].width, at[pc].branch_target))
                .collect();
            output.push(Plan {
                start: cfg.blocks[start].start,
                end,
                owned,
                layout,
            });
        }
        Some(output)
    }
    attempt(code).unwrap_or_default()
}
