//! Proofs for shared move/constant exit tails placed before their users.
use crate::{
    native_cfg::{ControlFlowGraph, EdgeKind},
    native_dex::DexCode,
};
use std::{collections::BTreeSet, ops::Range};

fn pure(op: u8) -> bool {
    matches!(op, 0x00..=0x09 | 0x12..=0x19 | 0x28..=0x2a)
}

fn bounded_cfg(code: &DexCode) -> Option<ControlFlowGraph> {
    if code.instructions.len() > 4096 {
        return None;
    }
    let cfg = ControlFlowGraph::build(code).ok()?;
    (cfg.blocks.len() <= 512).then_some(cfg)
}

/// A protected suffix made only of nonthrowing operations needs no new Java
/// catch scope. Recheck at every instruction so later effects keep ownership.
pub(crate) fn pure_protected_tail(code: &DexCode, protected: Range<usize>, entry: usize) -> bool {
    if !protected.contains(&entry)
        || !code
            .instructions
            .get(entry)
            .is_some_and(|word| pure(*word as u8))
    {
        return false;
    }
    let Some(cfg) = bounded_cfg(code) else {
        return false;
    };
    let Some(&root) = cfg.block_at.get(&entry) else {
        return false;
    };
    let mut cursor = root;
    let mut seen = BTreeSet::new();
    loop {
        let block = &cfg.blocks[cursor];
        if !protected.contains(&block.start) {
            return true;
        }
        if !seen.insert(cursor)
            || seen.len() > 32
            || block
                .instructions
                .iter()
                .any(|&pc| !protected.contains(&pc) || !pure(code.instructions[pc] as u8))
        {
            return false;
        }
        let next: Vec<_> = block
            .successors
            .iter()
            .filter(|e| e.kind == EdgeKind::Normal)
            .collect();
        if next.len() != 1 {
            return false;
        }
        cursor = next[0].target;
    }
}

/// Reentering the protected address range is harmless only where every
/// reachable protected instruction is nonthrowing. The ordinary renderer
/// still owns handler snapshots, join placement, and catch dispatch.
pub(crate) fn handler_reentry(code: &DexCode, protected: Range<usize>, handler: usize) -> bool {
    let Some(cfg) = bounded_cfg(code) else {
        return false;
    };
    if handler < protected.end || protected.start >= protected.end {
        return false;
    }
    let Some(&root) = cfg.block_at.get(&handler) else {
        return false;
    };
    let mut pending = vec![root];
    let mut seen = BTreeSet::new();
    let mut reenters = false;
    while let Some(block) = pending.pop() {
        if !seen.insert(block) {
            continue;
        }
        for &pc in &cfg.blocks[block].instructions {
            if protected.contains(&pc) {
                reenters = true;
                if !pure(code.instructions[pc] as u8) {
                    return false;
                }
            }
        }
        // Protected moves/constants have no throwing effect. Outside that
        // range, follow exceptional edges too, including indirect reentry.
        let protected_block = cfg.blocks[block]
            .instructions
            .iter()
            .any(|pc| protected.contains(pc));
        pending.extend(
            cfg.blocks[block]
                .successors
                .iter()
                .filter(|e| e.kind == EdgeKind::Normal || !protected_block)
                .map(|e| e.target),
        );
    }
    if !reenters {
        return false;
    }
    // Every reentered protected path must leave the range, with no cycle.
    for &block in &seen {
        if !cfg.blocks[block]
            .instructions
            .iter()
            .any(|pc| protected.contains(pc))
        {
            continue;
        }
        let mut path = BTreeSet::new();
        let mut cursor = block;
        loop {
            if !cfg.blocks[cursor]
                .instructions
                .iter()
                .any(|pc| protected.contains(pc))
            {
                break;
            }
            if !path.insert(cursor) {
                return false;
            }
            let next: Vec<_> = cfg.blocks[cursor]
                .successors
                .iter()
                .filter(|e| e.kind == EdgeKind::Normal)
                .collect();
            if next.len() != 1 {
                return false;
            }
            cursor = next[0].target;
        }
    }
    true
}

/// A loop exit can reach its own protected continuation, optionally after a
/// backward pure assignment. Follow normal and exceptional edges to prove
/// the continuation never returns to the original loop.
pub(crate) fn loop_exit(
    code: &DexCode,
    interval: Range<usize>,
    from: usize,
    target: usize,
) -> bool {
    let Some(cfg) = bounded_cfg(code) else {
        return false;
    };
    if !interval.contains(&from)
        || interval.contains(&target)
        || code.try_regions.iter().any(|r| {
            (r.start as usize <= from && from < r.end as usize)
                || ((r.start as usize) < target && target < r.end as usize)
        })
    {
        return false;
    }
    let Some(&source) = cfg.block_at.get(&from) else {
        return false;
    };
    let Some(&root) = cfg.block_at.get(&target) else {
        return false;
    };
    if cfg.blocks[source].instructions.last() != Some(&from)
        || !cfg.blocks[source]
            .successors
            .iter()
            .any(|e| e.kind == EdgeKind::Normal && e.target == root)
    {
        return false;
    }
    let mut cursor = root;
    let mut prefix = BTreeSet::new();
    loop {
        let block = &cfg.blocks[cursor];
        if block.start >= interval.end {
            break;
        }
        if !prefix.insert(cursor)
            || prefix.len() > 32
            || block.instructions.iter().any(|&pc| {
                interval.contains(&pc)
                    || !pure(code.instructions[pc] as u8)
                    || code
                        .try_regions
                        .iter()
                        .any(|r| r.start as usize <= pc && pc < r.end as usize)
            })
        {
            return false;
        }
        let next: Vec<_> = block
            .successors
            .iter()
            .filter(|e| e.kind == EdgeKind::Normal)
            .collect();
        if next.len() != 1 {
            return false;
        }
        cursor = next[0].target;
    }
    let mut pending = vec![cursor];
    let mut seen = BTreeSet::new();
    while let Some(block) = pending.pop() {
        if !seen.insert(block) {
            continue;
        }
        let b = &cfg.blocks[block];
        if prefix.contains(&block) || b.instructions.iter().any(|pc| interval.contains(pc)) {
            return false;
        }
        for edge in &b.successors {
            // Entering the middle of a protected body needs another proof.
            if edge.kind == EdgeKind::Normal
                && code.try_regions.iter().any(|r| {
                    let to = cfg.blocks[edge.target].start;
                    (r.start as usize) < to
                        && to < r.end as usize
                        && !(r.start as usize <= b.start && b.start < r.end as usize)
                })
            {
                return false;
            }
            pending.push(edge.target);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::DexTryRegion;
    fn code(words: Vec<u16>) -> DexCode {
        DexCode {
            registers: 3,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }
    }
    #[test]
    fn handler_can_share_only_nonthrowing_acyclic_exit_in_protected_range() {
        let mut c = code(vec![0x0012, 0x0328, 0x000e, 0x000e, 0x000e, 0xfb28]);
        c.tries = 1;
        c.try_regions = vec![DexTryRegion {
            start: 0,
            end: 2,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 5)].into(),
        }];
        assert!(handler_reentry(&c, 0..2, 5));
        assert!(pure_protected_tail(&c, 0..2, 0));
        c.instructions[0] = 0x0027; // throwing reentry
        assert!(!handler_reentry(&c, 0..2, 5));
        assert!(!pure_protected_tail(&c, 0..2, 0));
        c.instructions[0] = 0x0028; // cyclic reentry
        assert!(!handler_reentry(&c, 0..2, 5));
        assert!(!pure_protected_tail(&c, 0..2, 0));
    }
    #[test]
    fn backward_shared_assignment_leaves_loop_before_effectful_protected_continuation() {
        let mut c = code(vec![
            0x0328, 0x0012, 0x0628, 0x0038, 4, 0xfc28, 0xfd28, 0x000e, 0x0012, 0x000e, 0x000d,
            0x000e,
        ]);
        c.tries = 1;
        c.try_regions = vec![DexTryRegion {
            start: 8,
            end: 9,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 10)].into(),
        }];
        assert!(loop_exit(&c, 3..7, 5, 1));
        c.instructions[11] = 0xf828; // exception handler reenters loop
        assert!(!loop_exit(&c, 3..7, 5, 1));
    }

    #[test]
    fn forward_protected_exit_rejects_exception_reentry_and_partial_try_entry() {
        let mut c = code(vec![
            0x0012, 0x1035, 7, 0x00d8, 0x0100, 0x2033, 0xfffc, 0x000f, 0x1071, 0, 0, 0x000a,
            0x0428, 0x000d, 0xf012, 0x0128, 0x000f,
        ]);
        c.tries = 1;
        c.try_regions = vec![DexTryRegion {
            start: 8,
            end: 12,
            catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 13)].into(),
        }];
        assert!(loop_exit(&c, 1..7, 1, 8));
        c.instructions[15] = 0xf228; // catch returns to the original loop
        assert!(!loop_exit(&c, 1..7, 1, 8));
        c.instructions[15] = 0x0128;
        c.instructions[2] = 10; // pc1 -> pc11, protected interior
        assert!(!loop_exit(&c, 1..7, 1, 11));
    }
}
