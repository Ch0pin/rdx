use std::collections::HashMap;

use crate::dex::instruction::{DecodedInstruction, InlineBranchTargets, Operands, Payload};
use crate::dex::{CatchHandler, CodeItem, DexFile, EncodedMethod};
use crate::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub source_offset: u32,
    pub target_offset: u32,
    pub block: usize,
}

#[derive(Clone, Debug)]
pub struct BasicBlock {
    pub start: u32,
    pub end: u32,
    pub instructions: std::ops::Range<usize>,
    pub predecessors: Vec<Edge>,
    pub successors: Vec<Edge>,
    pub exception: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ExceptionHandler {
    pub type_idx: Option<u32>,
    pub target: u32,
    pub block: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ExceptionRegion {
    pub start: u32,
    pub end: u32,
    pub handlers: Vec<ExceptionHandler>,
}

#[derive(Debug)]
pub struct MethodAnalysis {
    pub instructions: Vec<DecodedInstruction>,
    pub blocks: Vec<BasicBlock>,
    pub exceptions: Vec<ExceptionRegion>,
}

impl MethodAnalysis {
    pub fn new(dex: &DexFile<'_>, method: &EncodedMethod) -> Result<Option<Self>> {
        let Some(code) = dex.code(method)? else {
            return Ok(None);
        };
        Self::from_code(&code).map(Some)
    }

    pub fn from_code(code: &CodeItem<'_>) -> Result<Self> {
        let instructions = code.decode()?;
        if instructions.is_empty() {
            return Ok(Self {
                instructions,
                blocks: Vec::new(),
                exceptions: build_exceptions(code)?,
            });
        }

        let exceptions = build_exceptions(code)?;
        let mut leaders = Vec::with_capacity(instructions.len());
        leaders.push(0_u32);

        for instruction in &instructions {
            if let Some(targets) = branch_targets(instruction, &instructions)? {
                targets.for_each(|target| leaders.push(target));
            }
        }
        for exception in &exceptions {
            leaders.push(exception.start);
            for handler in &exception.handlers {
                leaders.push(handler.target);
            }
        }
        leaders.sort_unstable();
        leaders.dedup();

        let mut blocks = Vec::new();
        let mut block_start_index = 0;
        let mut leader_index = 0;
        for (index, instruction) in instructions.iter().enumerate() {
            while leaders
                .get(leader_index)
                .is_some_and(|leader| *leader < instruction.offset)
            {
                leader_index += 1;
            }
            let is_leader = leaders
                .get(leader_index)
                .is_some_and(|leader| *leader == instruction.offset);
            leader_index += usize::from(is_leader);
            if index != block_start_index && is_leader {
                blocks.push(make_block(&instructions, block_start_index, index));
                block_start_index = index;
            }
            if is_branch(instruction.opcode) {
                blocks.push(make_block(&instructions, block_start_index, index + 1));
                block_start_index = index + 1;
            }
        }
        if block_start_index < instructions.len() {
            blocks.push(make_block(
                &instructions,
                block_start_index,
                instructions.len(),
            ));
        }

        let block_by_start: HashMap<u32, usize> = blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.start, index))
            .collect();
        for block_index in 0..blocks.len() {
            let last_index = blocks[block_index].instructions.end - 1;
            let instruction = &instructions[last_index];
            let explicit = branch_targets(instruction, &instructions)?;
            let source_offset = instruction.offset;
            let mut successors =
                Vec::with_capacity(explicit.as_ref().map_or(1, BranchTargets::len));
            if let Some(targets) = explicit {
                targets.for_each(|target| {
                    let target_block = block_by_start
                        .get(&target)
                        .copied()
                        .or_else(|| block_containing(&blocks, target));
                    if let Some(target_block) = target_block {
                        successors.push(Edge {
                            source_offset,
                            target_offset: target,
                            block: target_block,
                        });
                    }
                });
            } else {
                let next_probe = blocks[block_index].end.saturating_add(1);
                let target_block = block_containing(&blocks, next_probe);
                if let Some(target_block) = target_block {
                    successors.push(Edge {
                        source_offset,
                        target_offset: blocks[target_block].start,
                        block: target_block,
                    });
                }
            }
            blocks[block_index].successors = successors;
        }

        for source in 0..blocks.len() {
            let edge_count = blocks[source].successors.len();
            for edge_index in 0..edge_count {
                let edge = blocks[source].successors[edge_index];
                blocks[edge.block].predecessors.push(Edge {
                    source_offset: edge.target_offset,
                    target_offset: edge.source_offset,
                    block: source,
                });
            }
        }

        let mut exceptions = exceptions;
        for exception in &mut exceptions {
            for handler in &mut exception.handlers {
                handler.block = block_containing(&blocks, handler.target);
            }
        }
        for (exception_index, exception) in exceptions.iter().enumerate() {
            let first_block =
                blocks.partition_point(|block| block.end.saturating_sub(1) < exception.start);
            let last_block = blocks.partition_point(|block| block.start <= exception.end);
            for block in &mut blocks[first_block..last_block] {
                if block.exception.is_none()
                    && ((exception.start >= block.start
                        && exception.end <= block.end.saturating_sub(1))
                        || (block.end.saturating_sub(1) <= exception.end
                            && block.start >= exception.start))
                {
                    block.exception = Some(exception_index);
                }
            }
        }

        Ok(Self {
            instructions,
            blocks,
            exceptions,
        })
    }
}

fn make_block(
    instructions: &[DecodedInstruction],
    start_index: usize,
    end_index: usize,
) -> BasicBlock {
    let first = &instructions[start_index];
    let last = &instructions[end_index - 1];
    BasicBlock {
        start: first.offset,
        end: last.offset + last.width,
        instructions: start_index..end_index,
        predecessors: Vec::new(),
        successors: Vec::new(),
        exception: None,
    }
}

fn block_containing(blocks: &[BasicBlock], offset: u32) -> Option<usize> {
    let index = blocks
        .partition_point(|block| block.start <= offset)
        .checked_sub(1)?;
    (offset < blocks[index].end).then_some(index)
}

fn is_branch(opcode: u16) -> bool {
    matches!(opcode, 0x0e..=0x11 | 0x27..=0x2c | 0x32..=0x3d)
}

enum BranchTargets<'a> {
    Inline(InlineBranchTargets),
    Switch {
        fallthrough: Option<u32>,
        source_offset: u32,
        targets: &'a [i32],
    },
}

impl BranchTargets<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Inline(targets) => targets.as_slice().len(),
            Self::Switch { targets, .. } => targets.len() + 1,
        }
    }

    fn for_each(&self, mut visit: impl FnMut(u32)) {
        match self {
            Self::Inline(targets) => {
                for target in targets.as_slice().iter().flatten() {
                    visit(*target);
                }
            }
            Self::Switch {
                fallthrough,
                source_offset,
                targets,
            } => {
                if let Some(fallthrough) = fallthrough {
                    visit(*fallthrough);
                }
                for target in *targets {
                    if let Some(target) = relative_target(*source_offset, *target) {
                        visit(target);
                    }
                }
            }
        }
    }
}

fn branch_targets<'a>(
    instruction: &DecodedInstruction,
    instructions: &'a [DecodedInstruction],
) -> Result<Option<BranchTargets<'a>>> {
    if matches!(instruction.opcode, 0x2b | 0x2c) {
        let Operands::PayloadOffset { relative, .. } = instruction.operands else {
            return Err(Error::data(
                instruction.offset as usize,
                "switch has invalid operands",
            ));
        };
        let payload_offset = relative_target(instruction.offset, relative).ok_or_else(|| {
            Error::data(
                instruction.offset as usize,
                "switch payload target overflow",
            )
        })?;
        let payload_index = instructions
            .binary_search_by_key(&payload_offset, |candidate| candidate.offset)
            .map_err(|_| {
                Error::data(
                    instruction.offset as usize,
                    "switch payload target is not an instruction",
                )
            })?;
        let targets = match &instructions[payload_index].operands {
            Operands::Payload(Payload::PackedSwitch { targets, .. })
            | Operands::Payload(Payload::SparseSwitch { targets, .. }) => targets,
            _ => {
                return Err(Error::data(
                    instruction.offset as usize,
                    "switch target is not a switch payload",
                ));
            }
        };
        return Ok(Some(BranchTargets::Switch {
            fallthrough: instruction.offset.checked_add(instruction.width),
            source_offset: instruction.offset,
            targets,
        }));
    }
    Ok(instruction.branch_targets().map(BranchTargets::Inline))
}

fn relative_target(offset: u32, relative_units: i32) -> Option<u32> {
    let target = i64::from(offset) + i64::from(relative_units) * 2;
    u32::try_from(target).ok()
}

fn build_exceptions(code: &CodeItem<'_>) -> Result<Vec<ExceptionRegion>> {
    let handlers_by_offset: HashMap<u32, &CatchHandler> = code
        .handlers
        .iter()
        .map(|handler| (handler.offset, handler))
        .collect();
    let mut regions = Vec::with_capacity(code.tries.len());
    for try_item in &code.tries {
        let handler = handlers_by_offset
            .get(&u32::from(try_item.handler_off))
            .copied()
            .ok_or_else(|| Error::data(0, "try_item references a missing catch handler"))?;
        let start = try_item
            .start_addr
            .checked_mul(2)
            .ok_or_else(|| Error::data(0, "try start offset overflow"))?;
        let byte_count = u32::from(try_item.insn_count)
            .checked_mul(2)
            .ok_or_else(|| Error::data(0, "try instruction count overflow"))?;
        let end = start
            .checked_add(byte_count)
            .and_then(|value| value.checked_sub(1))
            .ok_or_else(|| Error::data(0, "empty or overflowing try range"))?;
        let mut resolved_handlers = Vec::with_capacity(
            handler.handlers.len() + usize::from(handler.catch_all_addr.is_some()),
        );
        for (type_idx, address) in &handler.handlers {
            resolved_handlers.push(ExceptionHandler {
                type_idx: Some(*type_idx),
                target: address
                    .checked_mul(2)
                    .ok_or_else(|| Error::data(0, "catch target overflow"))?,
                block: None,
            });
        }
        if let Some(address) = handler.catch_all_addr {
            resolved_handlers.push(ExceptionHandler {
                type_idx: None,
                target: address
                    .checked_mul(2)
                    .ok_or_else(|| Error::data(0, "catch-all target overflow"))?,
                block: None,
            });
        }
        regions.push(ExceptionRegion {
            start,
            end,
            handlers: resolved_handlers,
        });
    }
    Ok(regions)
}
