//! Bounded, conservative DEX register liveness. Unsupported or malformed code
//! returns `None`; callers must then retain every register. Exception edges
//! observe pre-write values, unlike normal successors.
use crate::native_dex::DexCode;
use std::collections::VecDeque;

const MAX_CODE_WORDS: usize = 65_536;
const MAX_STORAGE: usize = 32 * 1024 * 1024;
const MAX_WORK: usize = 16_000_000;
const MAX_EDGES: usize = 262_144;
const MISSING: u32 = u32::MAX;

pub(super) struct LiveRegisters {
    index: Vec<u32>,
    bits: Vec<u64>,
    stride: usize,
    registers: usize,
}
impl LiveRegisters {
    /// Unknown offsets/registers conservatively remain live. Method-end has no
    /// register uses; all other supported offsets are instruction boundaries.
    pub(super) fn contains(&self, pc: usize, register: usize) -> bool {
        if register >= self.registers {
            return true;
        }
        if pc == self.index.len() - 1 {
            return false;
        }
        let Some(&node) = self.index.get(pc) else {
            return true;
        };
        if node == MISSING {
            return true;
        }
        self.bits[node as usize * self.stride + register / 64] & (1 << (register % 64)) != 0
    }
}
struct Node {
    pc: usize,
    width: usize,
    reads: Vec<usize>,
    writes: Vec<usize>,
    normal: Vec<usize>,
    exceptional: Vec<usize>,
    throws: bool,
}
impl Node {
    fn read(&mut self, register: usize, wide: bool, count: usize) -> Option<()> {
        access(&mut self.reads, register, wide, count)
    }
    fn write(&mut self, register: usize, wide: bool, count: usize) -> Option<()> {
        access(&mut self.writes, register, wide, count)
    }
}
fn access(set: &mut Vec<usize>, register: usize, wide: bool, count: usize) -> Option<()> {
    if register >= count || (wide && register + 1 >= count) {
        return None;
    }
    set.push(register);
    if wide {
        set.push(register + 1);
    }
    Some(())
}
fn i32_at(words: &[u16], pc: usize) -> Option<i32> {
    Some((u32::from(*words.get(pc)?) | (u32::from(*words.get(pc + 1)?) << 16)) as i32)
}
fn target(pc: usize, delta: i32, count: usize) -> Option<usize> {
    let value = pc as i64 + i64::from(delta);
    (value >= 0 && value < count as i64).then_some(value as usize)
}
fn unary_widths(op: u8) -> Option<(bool, bool)> {
    Some(match op {
        0x7b | 0x7c | 0x7f | 0x82 | 0x87 | 0x8d..=0x8f => (false, false),
        0x7d | 0x7e | 0x80 | 0x86 | 0x8b => (true, true),
        0x81 | 0x83 | 0x88 | 0x89 => (false, true),
        0x84 | 0x85 | 0x8a | 0x8c => (true, false),
        _ => return None,
    })
}
fn binary_widths(op: u8) -> Option<(bool, bool, bool)> {
    Some(match op {
        0x90..=0x9a | 0xa6..=0xaa => (false, false, false),
        0x9b..=0xa2 | 0xab..=0xaf => (true, true, true),
        0xa3..=0xa5 => (true, false, true),
        _ => return None,
    })
}
fn operand_sets(node: &mut Node, words: &[u16], registers: usize) -> Option<()> {
    let word = words[node.pc];
    let op = word as u8;
    let a = (word >> 8) as usize;
    let pc = node.pc;
    let next = || words[pc + 1];
    match op {
        0x00 | 0x0e | 0x28..=0x2a => {}
        0x01..=0x09 => {
            let (dst, src) = match (op - 1) % 3 {
                0 => (a & 15, a >> 4),
                1 => (a, next() as usize),
                _ => (next() as usize, words[node.pc + 2] as usize),
            };
            let wide = matches!(op, 0x04..=0x06);
            node.read(src, wide, registers)?;
            node.write(dst, wide, registers)?;
        }
        0x0a..=0x0d => node.write(a, op == 0x0b, registers)?,
        0x0f..=0x11 => node.read(a, op == 0x10, registers)?,
        0x12 => node.write(a & 15, false, registers)?,
        0x13..=0x19 => node.write(a, matches!(op, 0x16..=0x19), registers)?,
        0x1a..=0x1c | 0x22 => {
            node.write(a, false, registers)?;
            node.throws = true;
        }
        0x1d..=0x1f | 0x26 | 0x27 => {
            node.read(a, false, registers)?;
            node.throws = true;
        }
        0x20 | 0x21 | 0x23 => {
            node.read(a >> 4, false, registers)?;
            node.write(a & 15, false, registers)?;
            node.throws = true;
        }
        0x24 | 0x6e..=0x72 => {
            let count = a >> 4;
            if count > 5 {
                return None;
            }
            let packed = words[node.pc + 2] as usize;
            let operands = [
                packed & 15,
                (packed >> 4) & 15,
                (packed >> 8) & 15,
                (packed >> 12) & 15,
                a & 15,
            ];
            for register in &operands[..count] {
                node.read(*register, false, registers)?;
            }
            node.throws = true;
        }
        0x25 | 0x74..=0x78 => {
            let start = words[node.pc + 2] as usize;
            for register in start..start.checked_add(a)? {
                node.read(register, false, registers)?;
            }
            node.throws = true;
        }
        0x2b | 0x2c | 0x38..=0x3d => node.read(a, false, registers)?,
        0x2d..=0x31 => {
            node.read((next() & 255) as usize, op >= 0x2f, registers)?;
            node.read((next() >> 8) as usize, op >= 0x2f, registers)?;
            node.write(a, false, registers)?;
        }
        0x32..=0x37 => {
            node.read(a & 15, false, registers)?;
            node.read(a >> 4, false, registers)?;
        }
        0x44..=0x51 => {
            node.read((next() & 255) as usize, false, registers)?;
            node.read((next() >> 8) as usize, false, registers)?;
            if op <= 0x4a {
                node.write(a, op == 0x45, registers)?;
            } else {
                node.read(a, op == 0x4c, registers)?;
            }
            node.throws = true;
        }
        0x52..=0x5f => {
            node.read(a >> 4, false, registers)?;
            if op <= 0x58 {
                node.write(a & 15, op == 0x53, registers)?;
            } else {
                node.read(a & 15, op == 0x5a, registers)?;
            }
            node.throws = true;
        }
        0x60..=0x6d => {
            if op <= 0x66 {
                node.write(a, op == 0x61, registers)?;
            } else {
                node.read(a, op == 0x68, registers)?;
            }
            node.throws = true;
        }
        0x7b..=0x8f => {
            let (input, output) = unary_widths(op)?;
            node.read(a >> 4, input, registers)?;
            node.write(a & 15, output, registers)?;
        }
        0x90..=0xcf => {
            let three = op <= 0xaf;
            let basic = if three { op } else { op - 0x20 };
            let (left, right, output) = binary_widths(basic)?;
            let (dst, lhs, rhs) = if three {
                (a, (next() & 255) as usize, (next() >> 8) as usize)
            } else {
                (a & 15, a & 15, a >> 4)
            };
            node.read(lhs, left, registers)?;
            node.read(rhs, right, registers)?;
            node.write(dst, output, registers)?;
            node.throws = matches!(basic, 0x93 | 0x94 | 0x9e | 0x9f);
        }
        0xd0..=0xe2 => {
            let (dst, src) = if op <= 0xd7 {
                (a & 15, a >> 4)
            } else {
                (a, (next() & 255) as usize)
            };
            node.read(src, false, registers)?;
            node.write(dst, false, registers)?;
            node.throws = matches!(op, 0xd3 | 0xd4 | 0xdb | 0xdc);
        }
        _ => return None,
    }
    Some(())
}
fn width(op: u8) -> Option<usize> {
    Some(match op {
        0x00
        | 0x01
        | 0x04
        | 0x07
        | 0x0a..=0x12
        | 0x1d
        | 0x1e
        | 0x21
        | 0x27
        | 0x28
        | 0x7b..=0x8f
        | 0xb0..=0xcf => 1,
        0x02
        | 0x05
        | 0x08
        | 0x13
        | 0x15
        | 0x16
        | 0x19
        | 0x1a
        | 0x1c
        | 0x1f
        | 0x20
        | 0x22
        | 0x23
        | 0x29
        | 0x2d..=0x3d
        | 0x44..=0x6d
        | 0x90..=0xaf
        | 0xd0..=0xe2 => 2,
        0x03
        | 0x06
        | 0x09
        | 0x14
        | 0x17
        | 0x1b
        | 0x24..=0x26
        | 0x2a..=0x2c
        | 0x6e..=0x72
        | 0x74..=0x78 => 3,
        0x18 => 5,
        _ => return None,
    })
}
/// May return `None` for otherwise valid DEX when the explicit storage/work
/// budget is reached. This is an optimization, never permission to drop state.
/// Registers written anywhere in a protected interval. Unknown instructions
/// decline the proof, so callers retain their conservative snapshot behavior.
pub(super) fn written_in(code: &DexCode, start: usize, end: usize) -> Option<Vec<bool>> {
    if start > end || end > code.instructions.len() || end - start > MAX_CODE_WORDS {
        return None;
    }
    let mut written = vec![false; code.registers as usize];
    let mut pc = start;
    while pc < end {
        let word = code.instructions[pc];
        if matches!(word, 0x0100 | 0x0200 | 0x0300) {
            return None;
        }
        let count = width(word as u8)?;
        if pc.checked_add(count)? > end {
            return None;
        }
        let mut node = Node {
            pc,
            width: count,
            reads: vec![],
            writes: vec![],
            normal: vec![],
            exceptional: vec![],
            throws: false,
        };
        operand_sets(&mut node, &code.instructions, written.len())?;
        for register in node.writes {
            written[register] = true;
        }
        pc += count;
    }
    Some(written)
}

pub(super) fn analyze(code: &DexCode) -> Option<LiveRegisters> {
    let words = &code.instructions;
    let registers = code.registers as usize;
    if words.is_empty()
        || words.len() > MAX_CODE_WORDS
        || usize::from(code.tries) != code.try_regions.len()
    {
        return None;
    }
    let mut index = vec![MISSING; words.len() + 1];
    let mut payloads = vec![0u16; words.len()];
    let mut nodes = Vec::new();
    let mut operand_storage = 0usize;
    let mut operand_work = 0usize;
    let mut pc = 0;
    while pc < words.len() {
        let word = words[pc];
        if matches!(word, 0x0100 | 0x0200 | 0x0300) {
            if !pc.is_multiple_of(2) {
                return None;
            }
            let count = *words.get(pc + 1)? as usize;
            let payload_width = match word {
                0x0100 => 4usize.checked_add(count.checked_mul(2)?)?,
                0x0200 => 2usize.checked_add(count.checked_mul(4)?)?,
                _ => {
                    if !matches!(count, 1 | 2 | 4 | 8) {
                        return None;
                    }
                    let elements = i32_at(words, pc + 2)? as u32 as usize;
                    4usize.checked_add(elements.checked_mul(count)?.div_ceil(2))?
                }
            };
            if pc.checked_add(payload_width)? > words.len() {
                return None;
            }
            payloads[pc] = word;
            pc += payload_width;
            continue;
        }
        if word == 0
            && !pc.is_multiple_of(2)
            && words
                .get(pc + 1)
                .is_some_and(|w| matches!(w, 0x0100 | 0x0200 | 0x0300))
        {
            pc += 1;
            continue;
        }
        let width = width(word as u8)?;
        if pc + width > words.len() || (word as u8 == 0 && word != 0) {
            return None;
        }
        let mut node = Node {
            pc,
            width,
            reads: vec![],
            writes: vec![],
            normal: vec![],
            exceptional: vec![],
            throws: false,
        };
        operand_sets(&mut node, words, registers)?;
        operand_work = operand_work.checked_add(node.reads.len() + node.writes.len())?;
        if operand_work > MAX_WORK {
            return None;
        }
        operand_storage = operand_storage.checked_add(
            (node.reads.capacity() + node.writes.capacity()) * std::mem::size_of::<usize>(),
        )?;
        if operand_storage
            + index.capacity() * 4
            + payloads.capacity() * 2
            + (nodes.len() + 1) * 2 * std::mem::size_of::<Node>()
            > MAX_STORAGE
        {
            return None;
        }
        index[pc] = nodes.len().try_into().ok()?;
        nodes.push(node);
        pc += width;
    }
    if index[0] == MISSING {
        return None;
    }
    let resolve = |pc: usize| -> Option<usize> {
        let node = *index.get(pc)?;
        (node != MISSING).then_some(node as usize)
    };
    let mut previous_end = 0usize;
    let mut metadata_edges = 0usize;
    for region in &code.try_regions {
        metadata_edges = metadata_edges.checked_add(region.catches.len())?;
        if metadata_edges > MAX_EDGES {
            return None;
        }
        let start = region.start as usize;
        let end = region.end as usize;
        if start < previous_end
            || start >= end
            || end > words.len()
            || resolve(start).is_none()
            || (end != words.len() && resolve(end).is_none())
            || (region.catches.is_empty() || region.catches.len() > 4096)
        {
            return None;
        }
        for (i, (ty, handler)) in region.catches.iter().enumerate() {
            resolve(*handler as usize)?;
            if ty
                .as_deref()
                .is_some_and(|ty| !ty.starts_with('L') || !ty.ends_with(';'))
                || (ty.is_none() && i + 1 != region.catches.len())
            {
                return None;
            }
        }
        previous_end = end;
    }
    let mut edges = 0usize;
    let mut exception_region = 0usize;
    let mut edge_storage = 0usize;
    let mut construction_work = words.len() + metadata_edges + operand_work;
    for node in &mut nodes {
        let pc = node.pc;
        let op = words[pc] as u8;
        let jump = match op {
            0x28 => Some((words[pc] >> 8) as i8 as i32),
            0x29 | 0x32..=0x3d => Some(words[pc + 1] as i16 as i32),
            0x2a => Some(i32_at(words, pc + 1)?),
            _ => None,
        };
        if let Some(delta) = jump {
            node.normal.push(resolve(target(pc, delta, words.len())?)?);
        }
        if matches!(op, 0x2b | 0x2c | 0x26) {
            let table = target(pc, i32_at(words, pc + 1)?, words.len())?;
            let expected = match op {
                0x2b => 0x0100,
                0x2c => 0x0200,
                _ => 0x0300,
            };
            if payloads[table] != expected {
                return None;
            }
            if op != 0x26 {
                let count = words[table + 1] as usize;
                if count > 4096 {
                    return None;
                }
                construction_work = construction_work.checked_add(count)?;
                if construction_work > MAX_WORK {
                    return None;
                }
                let mut previous = None;
                for i in 0..count {
                    let key = if op == 0x2b {
                        i32_at(words, table + 2)?.checked_add(i as i32)?
                    } else {
                        i32_at(words, table + 2 + i * 2)?
                    };
                    if previous.is_some_and(|old| old >= key) {
                        return None;
                    }
                    previous = Some(key);
                    let offset = if op == 0x2b {
                        table + 4 + i * 2
                    } else {
                        table + 2 + count * 2 + i * 2
                    };
                    node.normal
                        .push(resolve(target(pc, i32_at(words, offset)?, words.len())?)?);
                }
            }
        }
        if !matches!(op, 0x0e..=0x11 | 0x27..=0x2a) {
            node.normal.push(resolve(pc + node.width)?);
        }
        while code
            .try_regions
            .get(exception_region)
            .is_some_and(|region| region.end as usize <= pc)
        {
            exception_region += 1;
        }
        if node.throws
            && let Some(region) = code.try_regions.get(exception_region)
            && region.start as usize <= pc
        {
            construction_work = construction_work.checked_add(region.catches.len())?;
            if construction_work > MAX_WORK {
                return None;
            }
            for (_, handler) in region.catches.iter() {
                node.exceptional.push(resolve(*handler as usize)?);
            }
        }
        edge_storage = edge_storage.checked_add(
            (node.normal.capacity() + node.exceptional.capacity()) * std::mem::size_of::<usize>(),
        )?;
        if operand_storage
            + edge_storage
            + index.capacity() * 4
            + payloads.capacity() * 2
            + index.len() * 2 * std::mem::size_of::<Node>()
            > MAX_STORAGE
        {
            return None;
        }
        node.normal.sort_unstable();
        node.normal.dedup();
        node.exceptional.sort_unstable();
        node.exceptional.dedup();
        edges = edges.checked_add(node.normal.len() + node.exceptional.len())?;
        if edges > MAX_EDGES {
            return None;
        }
    }
    let stride = registers.div_ceil(64);
    let bit_len = nodes.len().checked_mul(stride)?;
    let fixed = bit_len.checked_mul(8)?.checked_add(
        index.capacity() * 4
            + payloads.capacity() * 2
            + nodes.capacity() * std::mem::size_of::<Node>()
            + nodes.len() * (std::mem::size_of::<Vec<usize>>() + 24)
            + edges * 32
            + stride * std::mem::size_of::<u64>(),
    )?;
    let dynamic: usize = nodes
        .iter()
        .map(|n| {
            (n.reads.capacity()
                + n.writes.capacity()
                + n.normal.capacity()
                + n.exceptional.capacity())
                * std::mem::size_of::<usize>()
        })
        .sum();
    if fixed.checked_add(dynamic)? > MAX_STORAGE {
        return None;
    }
    let mut bits = vec![0u64; bit_len];
    let mut scratch = vec![0u64; stride];
    let mut predecessors = vec![Vec::new(); nodes.len()];
    for (from, node) in nodes.iter().enumerate() {
        for to in node.normal.iter().chain(&node.exceptional) {
            predecessors[*to].push(from);
        }
    }
    let mut queue: VecDeque<_> = (0..nodes.len()).rev().collect();
    let mut queued = vec![true; nodes.len()];
    let mut work = construction_work.checked_add(edges)?;
    while let Some(id) = queue.pop_front() {
        queued[id] = false;
        scratch.fill(0);
        let node = &nodes[id];
        work = work.checked_add(
            stride * (node.normal.len() + node.exceptional.len() + 2)
                + node.reads.len()
                + node.writes.len()
                + predecessors[id].len()
                + 1,
        )?;
        if work > MAX_WORK {
            return None;
        }
        for successor in &node.normal {
            for (to, value) in scratch
                .iter_mut()
                .zip(&bits[*successor * stride..(*successor + 1) * stride])
            {
                *to |= value;
            }
        }
        for register in &node.writes {
            scratch[*register / 64] &= !(1 << (*register % 64));
        }
        for register in &node.reads {
            scratch[*register / 64] |= 1 << (*register % 64);
        }
        // A failed definition leaves the old destination visible to catches.
        for successor in &node.exceptional {
            for (to, value) in scratch
                .iter_mut()
                .zip(&bits[*successor * stride..(*successor + 1) * stride])
            {
                *to |= value;
            }
        }
        let current = &mut bits[id * stride..(id + 1) * stride];
        if current != scratch {
            current.copy_from_slice(&scratch);
            for predecessor in &predecessors[id] {
                if !queued[*predecessor] {
                    queue.push_back(*predecessor);
                    queued[*predecessor] = true;
                }
            }
        }
    }
    Some(LiveRegisters {
        index,
        bits,
        stride,
        registers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::DexTryRegion;
    fn code(instructions: Vec<u16>, registers: u16) -> DexCode {
        DexCode {
            registers,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions,
            offset: 0,
        }
    }
    #[test]
    fn protected_write_proof_tracks_pairs_and_declines_unknown_operands() {
        let wide = code(vec![0x0116, 7, 0x000e], 3);
        assert_eq!(written_in(&wide, 0, 2), Some(vec![false, true, true]));
        assert!(written_in(&wide, 0, 1).is_none());
        let malformed = code(vec![0x0312], 3);
        assert!(written_in(&malformed, 0, 1).is_none());
        let unknown = code(vec![0x00ff], 3);
        assert!(written_in(&unknown, 0, 1).is_none());
    }

    #[test]
    fn writes_kill_old_values_but_reads_and_loop_edges_propagate() {
        let live = analyze(&code(
            vec![0x0012, 0x1035, 5, 0x00d8, 0x0100, 0xfc28, 0x000f],
            2,
        ))
        .unwrap();
        assert!(!live.contains(0, 0));
        assert!(live.contains(0, 1));
        assert!(live.contains(1, 0));
        assert!(live.contains(1, 1));
        assert!(live.contains(6, 0));
        assert!(!live.contains(6, 1));
        assert!(!live.contains(7, 0));
        assert!(live.contains(2, 0));
    }
    #[test]
    fn throwing_write_preserves_old_destination_for_catch() {
        // v0=array[v1]; on failure handler returns old v0; success returns v0.
        let mut input = code(vec![0x0044, 0x0102, 0x0228, 0x000f, 0x000f], 3);
        input.tries = 1;
        input.try_regions.push(DexTryRegion {
            start: 0,
            end: 2,
            catches: vec![(Some("Ljava/lang/Exception;".into()), 3)].into(),
        });
        let live = analyze(&input).unwrap();
        assert!(live.contains(0, 0));
        assert!(live.contains(0, 1));
        assert!(live.contains(0, 2));
        input.tries = 0;
        input.try_regions.clear();
        assert!(!analyze(&input).unwrap().contains(0, 0));
    }
    #[test]
    fn move_exception_kills_old_destination_and_wide_pairs_are_atomic_uses() {
        let mut input = code(vec![0x0060, 0, 0x0310, 0x000d, 0x0310], 5);
        input.tries = 1;
        input.try_regions.push(DexTryRegion {
            start: 0,
            end: 2,
            catches: vec![(None, 3)].into(),
        });
        let live = analyze(&input).unwrap();
        assert!(!live.contains(0, 0));
        assert!(live.contains(0, 3));
        assert!(live.contains(0, 4));
        let live = analyze(&code(vec![0x2104, 0x0110], 4)).unwrap();
        assert!(live.contains(0, 2));
        assert!(live.contains(0, 3));
        assert!(!live.contains(0, 1));
    }
    #[test]
    fn switch_payload_is_data_and_all_targets_contribute() {
        let live = analyze(&code(
            vec![0x002b, 6, 0, 0x010f, 0x020f, 0, 0x0100, 1, 0, 0, 4, 0],
            3,
        ))
        .unwrap();
        assert!(live.contains(0, 0));
        assert!(live.contains(0, 1));
        assert!(live.contains(0, 2));
        assert!(live.contains(6, 0));
    }
    #[test]
    fn numeric_widths_and_throwing_kinds_preserve_exact_inputs() {
        let wide_shift = analyze(&code(vec![0x00a3, 0x0402, 0x0010], 6)).unwrap();
        for register in [2, 3, 4] {
            assert!(wide_shift.contains(0, register));
        }
        for register in [0, 1, 5] {
            assert!(!wide_shift.contains(0, register));
        }
        let narrow = analyze(&code(vec![0x208a, 0x000f], 4)).unwrap();
        assert!(narrow.contains(0, 2));
        assert!(narrow.contains(0, 3));
        assert!(!narrow.contains(0, 0));
        let wide = analyze(&code(vec![0x2081, 0x0010], 4)).unwrap();
        assert!(wide.contains(0, 2));
        assert!(!wide.contains(0, 3));
        for (op, throws) in [(0x0093, true), (0x00a9, false)] {
            let mut input = code(vec![op, 0x0302, 0x0228, 0x000f, 0x000f], 4);
            input.tries = 1;
            input.try_regions.push(DexTryRegion {
                start: 0,
                end: 2,
                catches: vec![(None, 3)].into(),
            });
            assert_eq!(analyze(&input).unwrap().contains(0, 0), throws);
        }
    }
    #[test]
    fn sparse_switch_and_range_invocation_track_all_register_words() {
        let live = analyze(&code(
            vec![
                0x002c, 6, 0, 0x010f, 0x020f, 0, 0x0200, 1, 0xffff, 0x7fff, 4, 0,
            ],
            3,
        ))
        .unwrap();
        for register in 0..3 {
            assert!(live.contains(0, register));
        }
        let range = analyze(&code(vec![0x0374, 0, 1, 0x000e], 5)).unwrap();
        for register in 1..4 {
            assert!(range.contains(0, register));
        }
        assert!(!range.contains(0, 0));
        assert!(!range.contains(0, 4));
        assert!(range.contains(usize::MAX, 0));
    }
    #[test]
    fn malformed_unknown_and_storage_heavy_inputs_decline_optimization() {
        assert!(analyze(&code(vec![0x00ff], 1)).is_none());
        assert!(analyze(&code(vec![0x0110], 2)).is_none());
        assert!(analyze(&code(vec![0x0228, 0x0014, 0, 0, 0x000e], 1)).is_none());
        let mut instructions = vec![0; 8192];
        instructions.push(0x000e);
        assert!(analyze(&code(instructions, 65535)).is_none());
    }
}
