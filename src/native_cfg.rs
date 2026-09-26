//! Bounded native DEX basic-block construction.
//!
//! The split/connect phases follow JADX's Apache-2.0 licensed `BlockSplitter`
//! at commit 28ff15e4ae69950aebea110a13e5ab895d234dfc. Unlike JADX, this module
//! provides a raw DEX builder and validated canonical decoded builders, retains
//! goto instructions, has no synthetic
//! enter/exit blocks or SSA attributes, and conservatively isolates every
//! protected instruction before adding exceptional edges. Payload code units
//! are metadata and never appear in a block. Reachability starts at the method
//! entry and every exception handler (conservative roots); all other unreachable
//! instructions are pruned, including alignment nops before payloads.

use crate::native_dex::DexCode;
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeSet, HashMap, HashSet};

const MAX_CODE_UNITS: usize = 1_000_000;
const MAX_BLOCKS: usize = 250_000;
const MAX_EDGES: usize = 1_000_000;
const MAX_SWITCH_CASES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    Normal,
    Exceptional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    pub target: usize,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    pub start: usize,
    pub end: usize,
    pub instructions: Vec<usize>,
    pub successors: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlFlowGraph {
    pub blocks: Vec<BasicBlock>,
    /// Executable instruction offset to block index. Payload offsets are absent.
    pub block_at: HashMap<usize, usize>,
}

impl ControlFlowGraph {
    pub fn build(code: &DexCode) -> Result<Self> {
        build(code)
    }
}

impl ControlFlowGraph {
    /// Build the bounded forward-only graph directly from canonical decoded
    /// instructions. A separate decoded builder admits selected natural loops;
    /// handlers, payloads and other loop shapes retain the raw CFG builder.
    pub(crate) fn from_decoded(ir: &crate::native_ir::DecodedMethod, len: usize) -> Result<Self> {
        Self::decoded_impl(ir, len, false)
    }
    pub(crate) fn from_decoded_loop(
        ir: &crate::native_ir::DecodedMethod,
        len: usize,
    ) -> Result<Self> {
        Self::decoded_impl(ir, len, true)
    }
    fn decoded_impl(
        ir: &crate::native_ir::DecodedMethod,
        len: usize,
        cyclic: bool,
    ) -> Result<Self> {
        ensure!(
            !ir.instructions.is_empty() && len <= MAX_CODE_UNITS,
            "invalid decoded CFG size"
        );
        let mut leaders = BTreeSet::from([0]);
        let mut end = 0;
        for instruction in &ir.instructions {
            ensure!(
                instruction.pc == end && instruction.width > 0,
                "noncontiguous decoded CFG instructions"
            );
            end = end
                .checked_add(instruction.width)
                .context("decoded CFG width overflow")?;
            ensure!(end <= len, "decoded CFG instruction exceeds method");
            ensure!(
                instruction.payload_target.is_none(),
                "payload in forward decoded CFG"
            );
            if matches!(instruction.opcode, 0x28..=0x2a | 0x32..=0x3d) {
                let target = instruction
                    .branch_target
                    .context("missing decoded CFG target")?;
                ensure!(
                    target != instruction.pc && target < len && (cyclic || target > instruction.pc),
                    "nonforward decoded CFG target"
                );
                leaders.insert(target);
            } else {
                ensure!(
                    instruction.branch_target.is_none(),
                    "target on nonbranch decoded instruction"
                );
            }
            if matches!(instruction.opcode, 0x0e..=0x11 | 0x27..=0x2a | 0x32..=0x3d) && end < len {
                leaders.insert(end);
            }
        }
        ensure!(
            end == len && leaders.len() <= MAX_BLOCKS,
            "decoded CFG coverage/budget mismatch"
        );
        let mut blocks: Vec<BasicBlock> = Vec::with_capacity(leaders.len());
        let mut block_at = HashMap::with_capacity(ir.instructions.len());
        for instruction in &ir.instructions {
            if leaders.contains(&instruction.pc) {
                blocks.push(BasicBlock {
                    start: instruction.pc,
                    end: instruction.pc,
                    instructions: Vec::new(),
                    successors: Vec::new(),
                });
            }
            let index = blocks
                .len()
                .checked_sub(1)
                .context("missing decoded CFG entry")?;
            block_at.insert(instruction.pc, index);
            blocks[index].instructions.push(instruction.pc);
            blocks[index].end = instruction.pc + instruction.width;
        }
        ensure!(
            blocks.len() == leaders.len(),
            "decoded CFG target is not an instruction boundary"
        );
        for block in &mut blocks {
            let last_pc = *block
                .instructions
                .last()
                .context("empty decoded CFG block")?;
            let instruction = &ir.instructions[ir
                .instructions
                .binary_search_by_key(&last_pc, |instruction| instruction.pc)
                .unwrap()];
            if let Some(target) = instruction.branch_target {
                block.successors.push(Edge {
                    target: *block_at
                        .get(&target)
                        .context("decoded CFG target boundary")?,
                    kind: EdgeKind::Normal,
                });
            }
            if !matches!(instruction.opcode, 0x0e..=0x11 | 0x27..=0x2a) {
                block.successors.push(Edge {
                    target: *block_at
                        .get(&block.end)
                        .context("decoded CFG falls off method")?,
                    kind: EdgeKind::Normal,
                });
            }
        }
        let graph = Self { blocks, block_at };
        graph.validate_decoded_impl(ir, len, cyclic)?;
        Ok(graph)
    }

    /// Immediate postdominators for a forward DAG with a virtual terminal exit.
    /// Successor-chain intersection follows the Cooper/Harvey/Kennedy approach
    /// used by JADX's pinned DominatorTree.java; reversing the DAG removes its
    /// iterative fixed point. This is not the full upstream dominator visitor.
    pub(crate) fn forward_postdominators(&self) -> Result<Vec<usize>> {
        let exit = self.blocks.len();
        let mut parents = vec![exit; exit + 1];
        let mut work = 20_000_000usize;
        for index in (0..exit).rev() {
            let mut common = None;
            for edge in &self.blocks[index].successors {
                ensure!(
                    edge.kind == EdgeKind::Normal && edge.target > index && edge.target < exit,
                    "invalid forward postdominator edge"
                );
                let mut left = common.unwrap_or(edge.target);
                let mut right = edge.target;
                while left != right {
                    ensure!(work > 0, "forward postdominator work budget");
                    work -= 1;
                    if left < right {
                        left = parents[left];
                    } else {
                        right = parents[right];
                    }
                }
                common = Some(left);
            }
            parents[index] = common.unwrap_or(exit);
        }
        Ok(parents)
    }

    /// Validate the stored canonical edges against decoded terminators, without
    /// rebuilding blocks or consulting raw branch words.
    pub(crate) fn validate_decoded(
        &self,
        ir: &crate::native_ir::DecodedMethod,
        len: usize,
    ) -> Result<()> {
        self.validate_decoded_impl(ir, len, false)
    }
    pub(crate) fn validate_decoded_loop(
        &self,
        ir: &crate::native_ir::DecodedMethod,
        len: usize,
    ) -> Result<()> {
        self.validate_decoded_impl(ir, len, true)
    }
    fn validate_decoded_impl(
        &self,
        ir: &crate::native_ir::DecodedMethod,
        len: usize,
        cyclic: bool,
    ) -> Result<()> {
        ensure!(
            !self.blocks.is_empty()
                && self.blocks.len() <= MAX_BLOCKS
                && self.block_at.len() == ir.instructions.len(),
            "invalid decoded CFG layout"
        );
        let mut cursor = 0;
        let mut instruction_index = 0;
        for (block_index, block) in self.blocks.iter().enumerate() {
            ensure!(
                block.start == cursor && !block.instructions.is_empty(),
                "noncontiguous decoded CFG blocks"
            );
            for &pc in &block.instructions {
                let instruction = ir
                    .instructions
                    .get(instruction_index)
                    .context("extra decoded CFG instruction")?;
                ensure!(
                    pc == cursor
                        && instruction.pc == pc
                        && instruction.width > 0
                        && self.block_at.get(&pc) == Some(&block_index),
                    "decoded CFG instruction identity mismatch"
                );
                ensure!(
                    instruction.payload_target.is_none(),
                    "payload in forward decoded CFG"
                );
                cursor = cursor
                    .checked_add(instruction.width)
                    .context("decoded CFG width overflow")?;
                instruction_index += 1;
                ensure!(cursor <= len, "decoded CFG instruction exceeds method");
                if cursor < block.end {
                    ensure!(
                        !matches!(instruction.opcode, 0x0e..=0x11 | 0x27..=0x2a | 0x32..=0x3d)
                            && instruction.branch_target.is_none(),
                        "decoded CFG terminator inside block"
                    );
                }
            }
            ensure!(cursor == block.end, "decoded CFG block end mismatch");
            let last = &ir.instructions[instruction_index - 1];
            let mut expected = [None; 2];
            let mut count = 0;
            if matches!(last.opcode, 0x28..=0x2a | 0x32..=0x3d) {
                let target = last.branch_target.context("missing decoded CFG target")?;
                ensure!(
                    target != last.pc && target < len && (cyclic || target > last.pc),
                    "nonforward decoded CFG target"
                );
                let target_index = *self
                    .block_at
                    .get(&target)
                    .context("decoded CFG target boundary")?;
                ensure!(
                    self.blocks
                        .get(target_index)
                        .is_some_and(|block| block.start == target),
                    "decoded CFG target enters block interior"
                );
                expected[count] = Some(target_index);
                count += 1;
            } else {
                ensure!(
                    last.branch_target.is_none(),
                    "target on nonbranch decoded instruction"
                );
            }
            if !matches!(last.opcode, 0x0e..=0x11 | 0x27..=0x2a) {
                let target_index = *self
                    .block_at
                    .get(&cursor)
                    .context("decoded CFG falls off method")?;
                ensure!(
                    self.blocks
                        .get(target_index)
                        .is_some_and(|block| block.start == cursor),
                    "decoded CFG fallthrough enters block interior"
                );
                expected[count] = Some(target_index);
                count += 1;
            }
            ensure!(
                block.successors.len() == count
                    && block
                        .successors
                        .iter()
                        .zip(expected)
                        .all(|(edge, target)| edge.kind == EdgeKind::Normal
                            && Some(edge.target) == target
                            && (cyclic || edge.target > block_index)),
                "decoded CFG successors differ from terminator"
            );
        }
        ensure!(
            cursor == len && instruction_index == ir.instructions.len(),
            "decoded CFG coverage mismatch"
        );
        let mut reachable = vec![false; self.blocks.len()];
        reachable[0] = true;
        if cyclic {
            let mut pending = vec![0];
            while let Some(index) = pending.pop() {
                for edge in &self.blocks[index].successors {
                    if !reachable[edge.target] {
                        reachable[edge.target] = true;
                        pending.push(edge.target);
                    }
                }
            }
            ensure!(
                reachable.iter().all(|seen| *seen),
                "unreachable decoded CFG block"
            );
        } else {
            for (index, block) in self.blocks.iter().enumerate() {
                ensure!(reachable[index], "unreachable decoded CFG block");
                for edge in &block.successors {
                    reachable[edge.target] = true;
                }
            }
        }
        Ok(())
    }
}

fn i32_at(words: &[u16], pos: usize) -> Result<i32> {
    let lo = u32::from(*words.get(pos).context("truncated 32-bit DEX operand")?);
    let hi = u32::from(*words.get(pos + 1).context("truncated 32-bit DEX operand")?);
    Ok((lo | (hi << 16)) as i32)
}

fn target(pc: usize, delta: i32, len: usize) -> Result<usize> {
    let value = pc as i64 + i64::from(delta);
    ensure!(
        value >= 0 && value < len as i64,
        "branch target outside method"
    );
    Ok(value as usize)
}

pub(crate) fn instruction_width(words: &[u16], pc: usize) -> Result<(usize, bool)> {
    let word = *words.get(pc).context("instruction offset outside method")?;
    let op = word as u8;
    let (size, payload) = if op == 0 {
        match word >> 8 {
            0 => (1, false),
            1 => (
                4usize
                    .checked_add(
                        usize::from(
                            *words
                                .get(pc + 1)
                                .context("truncated packed-switch payload")?,
                        )
                        .checked_mul(2)
                        .context("payload size overflow")?,
                    )
                    .context("payload size overflow")?,
                true,
            ),
            2 => (
                2usize
                    .checked_add(
                        usize::from(
                            *words
                                .get(pc + 1)
                                .context("truncated sparse-switch payload")?,
                        )
                        .checked_mul(4)
                        .context("payload size overflow")?,
                    )
                    .context("payload size overflow")?,
                true,
            ),
            3 => {
                let element_width =
                    usize::from(*words.get(pc + 1).context("truncated array payload")?);
                ensure!(
                    matches!(element_width, 1 | 2 | 4 | 8),
                    "invalid array payload element width"
                );
                let count = i32_at(words, pc + 2)? as u32 as usize;
                let bytes = element_width
                    .checked_mul(count)
                    .context("array payload size overflow")?;
                (
                    4usize
                        .checked_add(
                            bytes
                                .checked_add(1)
                                .context("array payload size overflow")?
                                / 2,
                        )
                        .context("array payload size overflow")?,
                    true,
                )
            }
            _ => bail!("unsupported DEX pseudo-instruction 0x{word:04x} at {pc}"),
        }
    } else {
        let size = match op {
            0x18 => 5,
            0xfa | 0xfb => 4,
            0x03
            | 0x06
            | 0x09
            | 0x14
            | 0x17
            | 0x1b
            | 0x24..=0x26
            | 0x2a..=0x2c
            | 0x6e..=0x72
            | 0x74..=0x78
            | 0xfc
            | 0xfd => 3,
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
            | 0xd0..=0xe2
            | 0xfe
            | 0xff => 2,
            0x01
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
            _ => bail!("unsupported or reserved DEX opcode 0x{op:02x} at {pc}"),
        };
        (size, false)
    };
    ensure!(
        pc.checked_add(size).is_some_and(|end| end <= words.len()),
        "truncated instruction or payload at {pc}"
    );
    Ok((size, payload))
}

fn add_edge(
    block: &mut BasicBlock,
    seen: &mut HashSet<Edge>,
    edge: Edge,
    count: &mut usize,
) -> Result<()> {
    if seen.insert(edge) {
        *count = count.checked_add(1).context("edge count overflow")?;
        ensure!(*count <= MAX_EDGES, "CFG exceeds edge budget");
        block.successors.push(edge);
    }
    Ok(())
}

/// Builds a control-flow graph whose offsets and ranges are DEX code units.
pub fn build(code: &DexCode) -> Result<ControlFlowGraph> {
    let words = &code.instructions;
    ensure!(!words.is_empty(), "empty DEX method body");
    ensure!(
        usize::from(code.tries) == code.try_regions.len(),
        "DEX tries count does not match try-region metadata"
    );
    ensure!(
        words.len() <= MAX_CODE_UNITS,
        "DEX method exceeds CFG code-unit budget"
    );

    let mut widths = vec![0usize; words.len()];
    let mut payload_starts = HashMap::new();
    let mut pc = 0;
    while pc < words.len() {
        let (size, payload) = instruction_width(words, pc)?;
        if payload {
            ensure!(pc.is_multiple_of(2), "unaligned DEX payload at {pc}");
            payload_starts.insert(pc, words[pc]);
        } else {
            widths[pc] = size;
        }
        pc += size;
    }
    let boundary = |offset: usize| offset < words.len() && widths[offset] != 0;
    ensure!(boundary(0), "method entry is not executable");

    let mut leaders = BTreeSet::from([0usize]);
    let mut normal: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut invalid_fallthrough = BTreeSet::new();
    let mut switches = 0usize;
    for pc in 0..words.len() {
        let width = widths[pc];
        if width == 0 {
            continue;
        }
        let op = words[pc] as u8;
        let next = pc + width;
        let jump = match op {
            0x28 => Some(target(pc, (words[pc] >> 8) as i8 as i32, words.len())?),
            0x29 | 0x32..=0x3d => Some(target(pc, words[pc + 1] as i16 as i32, words.len())?),
            0x2a => Some(target(pc, i32_at(words, pc + 1)?, words.len())?),
            _ => None,
        };
        if let Some(to) = jump {
            ensure!(
                boundary(to),
                "branch target is not an executable instruction boundary"
            );
            normal.entry(pc).or_default().push(to);
            leaders.insert(to);
        }
        if matches!(op, 0x2b | 0x2c) {
            let table = target(pc, i32_at(words, pc + 1)?, words.len())?;
            let expected = if op == 0x2b { 0x0100 } else { 0x0200 };
            ensure!(
                payload_starts.get(&table) == Some(&expected),
                "switch target is not the expected payload boundary"
            );
            let count = usize::from(words[table + 1]);
            switches = switches
                .checked_add(count)
                .context("switch case count overflow")?;
            ensure!(
                switches <= MAX_SWITCH_CASES,
                "CFG exceeds switch-case budget"
            );
            let mut previous = None;
            for index in 0..count {
                let key = if op == 0x2b {
                    i32_at(words, table + 2)?
                        .checked_add(index as i32)
                        .context("packed-switch key overflow")?
                } else {
                    i32_at(words, table + 2 + index * 2)?
                };
                ensure!(
                    previous.is_none_or(|old| old < key),
                    "switch keys are not strictly ordered"
                );
                previous = Some(key);
                let at = if op == 0x2b {
                    table + 4 + index * 2
                } else {
                    table + 2 + count * 2 + index * 2
                };
                let to = target(pc, i32_at(words, at)?, words.len())?;
                ensure!(
                    boundary(to),
                    "switch case target is not an executable instruction boundary"
                );
                normal.entry(pc).or_default().push(to);
                leaders.insert(to);
            }
        }
        if op == 0x26 {
            let table = target(pc, i32_at(words, pc + 1)?, words.len())?;
            ensure!(
                payload_starts.get(&table) == Some(&0x0300),
                "fill-array-data target is not an array payload boundary"
            );
        }
        let terminal = matches!(op, 0x0e..=0x11 | 0x27..=0x2a);
        if !terminal {
            if next < words.len() && boundary(next) {
                normal.entry(pc).or_default().push(next);
            } else {
                invalid_fallthrough.insert(pc);
            }
        }
        if (terminal || matches!(op, 0x32..=0x3d | 0x2b | 0x2c))
            && next < words.len()
            && boundary(next)
        {
            leaders.insert(next);
        }
    }

    let mut protected_handlers: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut previous_end = 0usize;
    let mut exceptional_edges = 0usize;
    for region in &code.try_regions {
        let start = region.start as usize;
        let end = region.end as usize;
        ensure!(
            start >= previous_end && start < end && end <= words.len(),
            "invalid or overlapping try region"
        );
        ensure!(
            boundary(start) && (end == words.len() || boundary(end)),
            "try range is not aligned to instruction boundaries"
        );
        ensure!(!region.catches.is_empty(), "try region has no handlers");
        ensure!(
            region.catches.len() <= MAX_SWITCH_CASES,
            "try region exceeds handler budget"
        );
        for (index, (ty, _)) in region.catches.iter().enumerate() {
            ensure!(
                ty.as_deref()
                    .is_none_or(|name| name.starts_with('L') && name.ends_with(';')),
                "invalid exception handler type"
            );
            ensure!(
                ty.is_some() || index + 1 == region.catches.len(),
                "catch-all handler is not last"
            );
        }
        let handlers = region
            .catches
            .iter()
            .map(|(_, offset)| *offset as usize)
            .collect::<Vec<_>>();
        for &handler in &handlers {
            ensure!(
                boundary(handler),
                "exception handler is not an executable instruction boundary"
            );
            leaders.insert(handler);
        }
        let mut cursor = start;
        while cursor < end {
            ensure!(
                boundary(cursor),
                "try region crosses payload or instruction interior"
            );
            leaders.insert(cursor);
            let next = cursor + widths[cursor];
            if next < words.len() && boundary(next) {
                leaders.insert(next);
            }
            exceptional_edges = exceptional_edges
                .checked_add(handlers.len())
                .context("exception edge count overflow")?;
            ensure!(
                exceptional_edges <= MAX_EDGES,
                "CFG exceeds exception-edge budget"
            );
            protected_handlers.insert(cursor, handlers.clone());
            cursor = next;
        }
        ensure!(cursor == end, "try region ends inside instruction");
        previous_end = end;
    }

    // DEX payload alignment can leave dead nops after a terminal instruction.
    // Accept those only when no entry/handler path can reach them.
    let mut reachable = BTreeSet::new();
    let mut pending = vec![0usize];
    pending.extend(
        code.try_regions
            .iter()
            .flat_map(|region| region.catches.iter().map(|(_, pc)| *pc as usize)),
    );
    while let Some(offset) = pending.pop() {
        if reachable.insert(offset) {
            pending.extend(normal.get(&offset).into_iter().flatten().copied());
        }
    }
    ensure!(
        invalid_fallthrough.is_disjoint(&reachable),
        "reachable normal control flow enters payload data or falls off method"
    );

    let mut blocks = Vec::new();
    let mut block_at = HashMap::new();
    pc = 0;
    while pc < words.len() {
        if !boundary(pc) || !reachable.contains(&pc) {
            pc += 1;
            continue;
        }
        let start = pc;
        let mut instructions = Vec::new();
        loop {
            instructions.push(pc);
            let next = pc + widths[pc];
            pc = next;
            if pc >= words.len()
                || !boundary(pc)
                || !reachable.contains(&pc)
                || leaders.contains(&pc)
            {
                break;
            }
        }
        ensure!(blocks.len() < MAX_BLOCKS, "CFG exceeds block budget");
        let index = blocks.len();
        for &offset in &instructions {
            block_at.insert(offset, index);
        }
        blocks.push(BasicBlock {
            start,
            end: pc,
            instructions,
            successors: Vec::new(),
        });
    }

    let mut edge_count = 0;
    for block in &mut blocks {
        let mut seen = HashSet::new();
        let offsets = block.instructions.clone();
        let last = *offsets.last().context("empty basic block")?;
        for to in normal.get(&last).into_iter().flatten() {
            let target = *block_at.get(to).context("normal successor has no block")?;
            add_edge(
                block,
                &mut seen,
                Edge {
                    target,
                    kind: EdgeKind::Normal,
                },
                &mut edge_count,
            )?;
        }
        for offset in offsets {
            for to in protected_handlers.get(&offset).into_iter().flatten() {
                let target = *block_at
                    .get(to)
                    .context("exception successor has no block")?;
                add_edge(
                    block,
                    &mut seen,
                    Edge {
                        target,
                        kind: EdgeKind::Exceptional,
                    },
                    &mut edge_count,
                )?;
            }
        }
    }
    Ok(ControlFlowGraph { blocks, block_at })
}
