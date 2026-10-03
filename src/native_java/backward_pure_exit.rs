//! Bounded proof for a loop's primary guard exiting through a pure backward tail.
//! This is a proof only: callers still emit the original tail and synchronize
//! its frame at the forward join. At emission, successors must include every
//! normal edge and every possible exceptional handler edge from that join.
use std::{collections::HashSet, ops::Range};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BackwardPureExit {
    pub(super) guard: usize,
    pub(super) target: usize,
    pub(super) join: usize,
    pub(super) path: Vec<usize>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prove(
    words: &[u16],
    widths: &[usize],
    targets: &[Option<usize>],
    protected: &[Range<usize>],
    header: usize,
    latch: usize,
    guard: usize,
    mut successors: impl FnMut(usize) -> Option<Vec<usize>>,
) -> Option<BackwardPureExit> {
    if words.len() != widths.len()
        || words.len() != targets.len()
        || header > guard
        || guard >= latch
        || latch >= words.len()
        || widths[header] == 0
        || !matches!(*words.get(guard)? as u8, 0x32..=0x3d)
        || widths[guard] != 2
        || branch_target(words, guard)? != targets[guard]?
        || !matches!(words[latch] as u8, 0x28..=0x2a)
        || branch_target(words, latch)? != header
        || targets[latch] != Some(header)
    {
        return None;
    }
    let end = latch.checked_add(widths[latch])?;
    if end > words.len() || widths[latch] != pure_width(words[latch] as u8)? {
        return None;
    }
    let target = targets[guard]?;
    if target >= header {
        return None;
    }
    let owned = |pc| {
        protected
            .iter()
            .all(|region| region.contains(&guard) == region.contains(&pc))
    };
    let mut pc = target;
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..=32 {
        if pc >= end {
            // The path must explicitly leave the backward prefix for a forward
            // join. It cannot fall through the loop's header or body.
            if path.is_empty() || pc >= words.len() || widths[pc] == 0 {
                return None;
            }
            let mut pending = vec![pc];
            let mut checked = HashSet::new();
            let mut work = 0usize;
            while let Some(next) = pending.pop() {
                work += 1;
                if work > 65_536
                    || (header..end).contains(&next)
                    || seen.contains(&next)
                    || next >= words.len()
                    || widths[next] == 0
                {
                    return None;
                }
                if checked.insert(next) {
                    pending.extend(successors(next)?);
                }
            }
            return Some(BackwardPureExit {
                guard,
                target,
                join: pc,
                path,
            });
        }
        if path.len() == 32 || pc >= header || !owned(pc) || !seen.insert(pc) {
            return None;
        }
        let op = *words.get(pc)? as u8;
        let width = pure_width(op)?;
        if widths[pc] != width || pc.checked_add(width)? > words.len() {
            return None;
        }
        path.push(pc);
        pc = if matches!(op, 0x28..=0x2a) {
            let next = branch_target(words, pc)?;
            if targets[pc] != Some(next) || next <= pc {
                return None;
            }
            next
        } else {
            if targets[pc].is_some() || (op == 0 && words[pc] != 0) {
                return None;
            }
            pc + width
        };
    }
    None
}

fn pure_width(op: u8) -> Option<usize> {
    Some(match op {
        0x00 | 0x01 | 0x04 | 0x07 | 0x12 | 0x28 => 1,
        0x02 | 0x05 | 0x08 | 0x13 | 0x15 | 0x16 | 0x19 | 0x29 => 2,
        0x03 | 0x06 | 0x09 | 0x14 | 0x17 | 0x2a => 3,
        0x18 => 5,
        _ => return None,
    })
}

fn branch_target(words: &[u16], pc: usize) -> Option<usize> {
    let word = *words.get(pc)?;
    let offset = match word as u8 {
        0x28 => (word >> 8) as i8 as i64,
        0x29 | 0x32..=0x3d => *words.get(pc + 1)? as i16 as i64,
        0x2a => {
            (u32::from(*words.get(pc + 1)?) | (u32::from(*words.get(pc + 2)?) << 16)) as i32 as i64
        }
        _ => return None,
    };
    usize::try_from((pc as i64).checked_add(offset)?).ok()
}
