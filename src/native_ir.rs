//! Typed register and operand decoding for raw DEX instructions.
//!
//! The instruction-to-operand mapping is adapted from JADX's Apache-2.0
//! licensed `InsnDecoder` at commit 28ff15e4ae69950aebea110a13e5ab895d234dfc.
//! Unlike JADX, this layer has no resolved symbols, type lattice, synthetic
//! instructions, or SSA state: invoke arguments remain raw DEX words and pool
//! references remain indices. Consequently this is not a complete typed
//! semantic IR. It decodes DEX only; JVM bytecode is unsupported.

use crate::native_cfg::instruction_width;
use crate::native_dex::DexCode;
use anyhow::{Context, Result, bail, ensure};
use std::cell::Cell;
use std::collections::HashMap;

const MAX_CODE_UNITS: usize = 1_000_000;
const MAX_REGISTER_OPERANDS: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    Bits32,
    Wide64,
    Reference,
    Unknown32,
}

impl ValueKind {
    pub const fn word_count(self) -> usize {
        match self {
            Self::Wide64 => 2,
            Self::Bits32 | Self::Reference | Self::Unknown32 => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegisterOperand {
    pub register: u16,
    pub kind: ValueKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PoolKind {
    String,
    Type,
    Field,
    Method,
    Proto,
    CallSite,
    MethodHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PoolReference {
    pub kind: PoolKind,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub pc: usize,
    pub width: usize,
    pub opcode: u8,
    pub reads: Vec<RegisterOperand>,
    pub writes: Vec<RegisterOperand>,
    pub may_throw: bool,
    pub literal: Option<i64>,
    pub reference: Option<PoolReference>,
    pub prototype: Option<u16>,
    pub branch_target: Option<usize>,
    pub payload_target: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedMethod {
    pub instructions: Vec<Instruction>,
}

fn i32_at(words: &[u16], pos: usize) -> Result<i32> {
    let lo = u32::from(*words.get(pos).context("truncated 32-bit DEX operand")?);
    let hi = u32::from(*words.get(pos + 1).context("truncated 32-bit DEX operand")?);
    Ok((lo | hi << 16) as i32)
}

fn i64_at(words: &[u16], pos: usize) -> Result<i64> {
    let mut value = 0u64;
    for shift in 0..4 {
        value |= u64::from(
            *words
                .get(pos + shift)
                .context("truncated 64-bit DEX operand")?,
        ) << (shift * 16);
    }
    Ok(value as i64)
}

fn relative_target(pc: usize, delta: i32, len: usize) -> Result<usize> {
    let target = pc as i64 + i64::from(delta);
    ensure!(
        target >= 0 && target < len as i64,
        "target outside method at {pc}"
    );
    Ok(target as usize)
}

fn operand(register: usize, kind: ValueKind, registers: usize) -> Result<RegisterOperand> {
    let end = register
        .checked_add(kind.word_count())
        .context("register range overflow")?;
    ensure!(
        end <= registers,
        "register v{register} is outside method register file"
    );
    Ok(RegisterOperand {
        register: register.try_into().context("register number exceeds u16")?,
        kind,
    })
}

fn kind_for_array(op: u8) -> ValueKind {
    match op {
        0x45 | 0x4c => ValueKind::Wide64,
        0x46 | 0x4d => ValueKind::Reference,
        _ => ValueKind::Bits32,
    }
}

fn kind_for_instance_field(op: u8) -> ValueKind {
    match op {
        0x53 | 0x5a => ValueKind::Wide64,
        0x54 | 0x5b => ValueKind::Reference,
        _ => ValueKind::Bits32,
    }
}

fn kind_for_static_field(op: u8) -> ValueKind {
    match op {
        0x61 | 0x68 => ValueKind::Wide64,
        0x62 | 0x69 => ValueKind::Reference,
        _ => ValueKind::Bits32,
    }
}

fn unary_kinds(op: u8) -> (ValueKind, ValueKind) {
    use ValueKind::{Bits32 as B, Wide64 as W};
    match op {
        0x7d | 0x7e | 0x80 | 0x86 | 0x8b => (W, W),
        0x81 | 0x83 | 0x88 | 0x89 => (B, W),
        0x84 | 0x85 | 0x8a | 0x8c => (W, B),
        _ => (B, B),
    }
}

fn binary_kinds(op: u8) -> (ValueKind, ValueKind, ValueKind) {
    use ValueKind::{Bits32 as B, Wide64 as W};
    match op {
        0x9b..=0xa2 | 0xab..=0xaf => (W, W, W),
        0xa3..=0xa5 => (W, B, W),
        _ => (B, B, B),
    }
}

impl DecodedMethod {
    pub fn decode(code: &DexCode) -> Result<Self> {
        let words = &code.instructions;
        ensure!(!words.is_empty(), "empty DEX method body");
        ensure!(
            words.len() <= MAX_CODE_UNITS,
            "DEX method exceeds IR code-unit budget"
        );
        let registers = usize::from(code.registers);
        let mut boundaries = vec![false; words.len()];
        let mut payloads = HashMap::<usize, u16>::new();
        let mut layout = Vec::new();
        let mut pc = 0usize;
        while pc < words.len() {
            let (width, payload) = instruction_width(words, pc)?;
            if payload {
                ensure!(pc.is_multiple_of(2), "unaligned DEX payload at {pc}");
                payloads.insert(pc, words[pc]);
            } else {
                boundaries[pc] = true;
                layout.push((pc, width));
            }
            pc = pc
                .checked_add(width)
                .context("instruction offset overflow")?;
        }
        ensure!(boundaries[0], "method entry is not an instruction boundary");

        let mut instructions = Vec::with_capacity(layout.len());
        let operand_count = Cell::new(0usize);
        for (pc, width) in layout {
            let word = words[pc];
            let op = word as u8;
            let a = usize::from(word >> 8);
            let mut insn = Instruction {
                pc,
                width,
                opcode: op,
                reads: Vec::new(),
                writes: Vec::new(),
                may_throw: false,
                literal: None,
                reference: None,
                prototype: None,
                branch_target: None,
                payload_target: None,
            };
            let mut read = |reg, kind| -> Result<()> {
                let count = operand_count
                    .get()
                    .checked_add(1)
                    .context("operand count overflow")?;
                ensure!(
                    count <= MAX_REGISTER_OPERANDS,
                    "DEX method exceeds IR operand budget"
                );
                insn.reads.push(operand(reg, kind, registers)?);
                operand_count.set(count);
                Ok(())
            };
            let mut write = |reg, kind| -> Result<()> {
                let count = operand_count
                    .get()
                    .checked_add(1)
                    .context("operand count overflow")?;
                ensure!(
                    count <= MAX_REGISTER_OPERANDS,
                    "DEX method exceeds IR operand budget"
                );
                insn.writes.push(operand(reg, kind, registers)?);
                operand_count.set(count);
                Ok(())
            };
            let next = || usize::from(words[pc + 1]);
            match op {
                0x00 | 0x0e => {}
                0x01..=0x09 => {
                    let (dst, src) = match (op - 1) % 3 {
                        0 => (a & 15, a >> 4),
                        1 => (a, next()),
                        _ => (next(), usize::from(words[pc + 2])),
                    };
                    let kind = match op {
                        0x04..=0x06 => ValueKind::Wide64,
                        0x07..=0x09 => ValueKind::Reference,
                        _ => ValueKind::Bits32,
                    };
                    read(src, kind)?;
                    write(dst, kind)?;
                }
                0x0a..=0x0d => write(
                    a,
                    match op {
                        0x0b => ValueKind::Wide64,
                        0x0c | 0x0d => ValueKind::Reference,
                        _ => ValueKind::Unknown32,
                    },
                )?,
                0x0f..=0x11 => read(
                    a,
                    match op {
                        0x10 => ValueKind::Wide64,
                        0x11 => ValueKind::Reference,
                        _ => ValueKind::Bits32,
                    },
                )?,
                0x12 => {
                    write(a & 15, ValueKind::Bits32)?;
                    insn.literal = Some((((a >> 4) as i8) << 4 >> 4) as i64);
                }
                0x13 => {
                    write(a, ValueKind::Bits32)?;
                    insn.literal = Some(words[pc + 1] as i16 as i64);
                }
                0x14 => {
                    write(a, ValueKind::Bits32)?;
                    insn.literal = Some(i64::from(i32_at(words, pc + 1)?));
                }
                0x15 => {
                    write(a, ValueKind::Bits32)?;
                    insn.literal = Some(i64::from(words[pc + 1] as i16) << 16);
                }
                0x16 => {
                    write(a, ValueKind::Wide64)?;
                    insn.literal = Some(words[pc + 1] as i16 as i64);
                }
                0x17 => {
                    write(a, ValueKind::Wide64)?;
                    insn.literal = Some(i64::from(i32_at(words, pc + 1)?));
                }
                0x18 => {
                    write(a, ValueKind::Wide64)?;
                    insn.literal = Some(i64_at(words, pc + 1)?);
                }
                0x19 => {
                    write(a, ValueKind::Wide64)?;
                    insn.literal = Some(i64::from(words[pc + 1] as i16) << 48);
                }
                0x1a | 0x1b => {
                    write(a, ValueKind::Reference)?;
                    let index = if op == 0x1a {
                        u32::from(words[pc + 1])
                    } else {
                        i32_at(words, pc + 1)? as u32
                    };
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::String,
                        index,
                    });
                    insn.may_throw = true;
                }
                0x1c => {
                    write(a, ValueKind::Reference)?;
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::Type,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0x1d | 0x1e => {
                    read(a, ValueKind::Reference)?;
                    insn.may_throw = true;
                }
                0x1f => {
                    read(a, ValueKind::Reference)?;
                    write(a, ValueKind::Reference)?;
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::Type,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0x20 => {
                    read(a >> 4, ValueKind::Reference)?;
                    write(a & 15, ValueKind::Bits32)?;
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::Type,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0x21 => {
                    read(a >> 4, ValueKind::Reference)?;
                    write(a & 15, ValueKind::Bits32)?;
                    insn.may_throw = true;
                }
                0x22 => {
                    write(a, ValueKind::Reference)?;
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::Type,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0x23 => {
                    read(a >> 4, ValueKind::Bits32)?;
                    write(a & 15, ValueKind::Reference)?;
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::Type,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0x24 | 0x6e..=0x72 | 0xfc => {
                    let count = a >> 4;
                    ensure!(count <= 5, "invoke register count exceeds five at {pc}");
                    let packed = usize::from(words[pc + 2]);
                    let regs = [
                        packed & 15,
                        packed >> 4 & 15,
                        packed >> 8 & 15,
                        packed >> 12 & 15,
                        a & 15,
                    ];
                    for &reg in &regs[..count] {
                        read(reg, ValueKind::Unknown32)?;
                    }
                    insn.reference = Some(PoolReference {
                        kind: if op == 0x24 {
                            PoolKind::Type
                        } else if op == 0xfc {
                            PoolKind::CallSite
                        } else {
                            PoolKind::Method
                        },
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0x25 | 0x74..=0x78 | 0xfd => {
                    let start = usize::from(words[pc + 2]);
                    let end = start
                        .checked_add(a)
                        .context("range invoke register overflow")?;
                    ensure!(
                        end <= registers,
                        "range invoke exceeds method register file at {pc}"
                    );
                    for reg in start..end {
                        read(reg, ValueKind::Unknown32)?;
                    }
                    insn.reference = Some(PoolReference {
                        kind: if op == 0x25 {
                            PoolKind::Type
                        } else if op == 0xfd {
                            PoolKind::CallSite
                        } else {
                            PoolKind::Method
                        },
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0x26 => {
                    read(a, ValueKind::Reference)?;
                    insn.payload_target =
                        Some(relative_target(pc, i32_at(words, pc + 1)?, words.len())?);
                    insn.may_throw = true;
                }
                0x27 => {
                    read(a, ValueKind::Reference)?;
                    insn.may_throw = true;
                }
                0x28 => {
                    insn.branch_target =
                        Some(relative_target(pc, (word >> 8) as i8 as i32, words.len())?)
                }
                0x29 => {
                    insn.branch_target = Some(relative_target(
                        pc,
                        words[pc + 1] as i16 as i32,
                        words.len(),
                    )?)
                }
                0x2a => {
                    insn.branch_target =
                        Some(relative_target(pc, i32_at(words, pc + 1)?, words.len())?)
                }
                0x2b | 0x2c => {
                    read(a, ValueKind::Bits32)?;
                    insn.payload_target =
                        Some(relative_target(pc, i32_at(words, pc + 1)?, words.len())?);
                }
                0x2d..=0x31 => {
                    let pair_kind = if matches!(op, 0x2f..=0x31) {
                        ValueKind::Wide64
                    } else {
                        ValueKind::Bits32
                    };
                    read(next() & 255, pair_kind)?;
                    read(next() >> 8, pair_kind)?;
                    write(a, ValueKind::Bits32)?;
                }
                0x32..=0x37 => {
                    read(a & 15, ValueKind::Unknown32)?;
                    read(a >> 4, ValueKind::Unknown32)?;
                    insn.branch_target = Some(relative_target(
                        pc,
                        words[pc + 1] as i16 as i32,
                        words.len(),
                    )?);
                }
                0x38..=0x3d => {
                    read(a, ValueKind::Unknown32)?;
                    insn.branch_target = Some(relative_target(
                        pc,
                        words[pc + 1] as i16 as i32,
                        words.len(),
                    )?);
                }
                0x44..=0x51 => {
                    let kind = kind_for_array(op);
                    let pair = next();
                    read(pair & 255, ValueKind::Reference)?;
                    read(pair >> 8, ValueKind::Bits32)?;
                    if op <= 0x4a {
                        write(a, kind)?;
                    } else {
                        read(a, kind)?;
                    }
                    insn.may_throw = true;
                }
                0x52..=0x5f => {
                    let kind = kind_for_instance_field(op);
                    read(a >> 4, ValueKind::Reference)?;
                    if op <= 0x58 {
                        write(a & 15, kind)?;
                    } else {
                        read(a & 15, kind)?;
                    }
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::Field,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0x60..=0x6d => {
                    let kind = kind_for_static_field(op);
                    if op <= 0x66 {
                        write(a, kind)?;
                    } else {
                        read(a, kind)?;
                    }
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::Field,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0x73 | 0x79 | 0x7a => {
                    bail!("unsupported or reserved DEX opcode 0x{op:02x} at {pc}")
                }
                0x7b..=0x8f => {
                    let (input, output) = unary_kinds(op);
                    read(a >> 4, input)?;
                    write(a & 15, output)?;
                }
                0x90..=0xaf => {
                    let (left, right, output) = binary_kinds(op);
                    let pair = next();
                    read(pair & 255, left)?;
                    read(pair >> 8, right)?;
                    write(a, output)?;
                    insn.may_throw = matches!(op, 0x93 | 0x94 | 0x9e | 0x9f);
                }
                0xb0..=0xcf => {
                    let basic = op - 0x20;
                    let (left, right, output) = binary_kinds(basic);
                    read(a & 15, left)?;
                    read(a >> 4, right)?;
                    write(a & 15, output)?;
                    insn.may_throw = matches!(basic, 0x93 | 0x94 | 0x9e | 0x9f);
                }
                0xd0..=0xd7 => {
                    read(a >> 4, ValueKind::Bits32)?;
                    write(a & 15, ValueKind::Bits32)?;
                    insn.literal = Some(words[pc + 1] as i16 as i64);
                    insn.may_throw = matches!(op, 0xd3 | 0xd4);
                }
                0xd8..=0xe2 => {
                    read(next() & 255, ValueKind::Bits32)?;
                    write(a, ValueKind::Bits32)?;
                    insn.literal = Some((next() >> 8) as u8 as i8 as i64);
                    insn.may_throw = matches!(op, 0xdb | 0xdc);
                }
                0xe3..=0xf9 => bail!("unsupported or reserved DEX opcode 0x{op:02x} at {pc}"),
                0xfa | 0xfb => {
                    let count = if op == 0xfa { a >> 4 } else { a };
                    if op == 0xfa {
                        ensure!(count <= 5, "invoke register count exceeds five at {pc}");
                        let packed = usize::from(words[pc + 2]);
                        let regs = [
                            packed & 15,
                            packed >> 4 & 15,
                            packed >> 8 & 15,
                            packed >> 12 & 15,
                            a & 15,
                        ];
                        for &reg in &regs[..count] {
                            read(reg, ValueKind::Unknown32)?;
                        }
                    } else {
                        let start = usize::from(words[pc + 2]);
                        let end = start
                            .checked_add(count)
                            .context("range invoke register overflow")?;
                        ensure!(
                            end <= registers,
                            "range invoke exceeds method register file at {pc}"
                        );
                        for reg in start..end {
                            read(reg, ValueKind::Unknown32)?;
                        }
                    }
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::Method,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.prototype = Some(words[pc + 3]);
                    insn.may_throw = true;
                }
                0xfe => {
                    write(a, ValueKind::Reference)?;
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::MethodHandle,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                0xff => {
                    write(a, ValueKind::Reference)?;
                    insn.reference = Some(PoolReference {
                        kind: PoolKind::Proto,
                        index: u32::from(words[pc + 1]),
                    });
                    insn.may_throw = true;
                }
                _ => bail!("unsupported or reserved DEX opcode 0x{op:02x} at {pc}"),
            }
            instructions.push(insn);
        }

        for insn in &instructions {
            if let Some(target) = insn.branch_target {
                ensure!(
                    boundaries.get(target) == Some(&true),
                    "branch target is not an instruction boundary at {}",
                    insn.pc
                );
            }
            if let Some(target) = insn.payload_target {
                let expected = match insn.opcode {
                    0x2b => 0x0100,
                    0x2c => 0x0200,
                    0x26 => 0x0300,
                    _ => unreachable!(),
                };
                ensure!(
                    payloads.get(&target) == Some(&expected),
                    "payload target has wrong type at {}",
                    insn.pc
                );
                if matches!(insn.opcode, 0x2b | 0x2c) {
                    let count = usize::from(words[target + 1]);
                    let offsets = if insn.opcode == 0x2b {
                        target + 4
                    } else {
                        target + 2 + count * 2
                    };
                    if insn.opcode == 0x2b && count != 0 {
                        let first_key = i32_at(words, target + 2)?;
                        ensure!(
                            i64::from(first_key) + count as i64 - 1 <= i64::from(i32::MAX),
                            "packed-switch keys overflow at {}",
                            insn.pc
                        );
                    } else if insn.opcode == 0x2c {
                        let mut previous = None;
                        for index in 0..count {
                            let key = i32_at(words, target + 2 + index * 2)?;
                            ensure!(
                                previous.is_none_or(|value| value < key),
                                "sparse-switch keys are not strictly increasing at {}",
                                insn.pc
                            );
                            previous = Some(key);
                        }
                    }
                    for index in 0..count {
                        let case_target = relative_target(
                            insn.pc,
                            i32_at(words, offsets + index * 2)?,
                            words.len(),
                        )?;
                        ensure!(
                            boundaries.get(case_target) == Some(&true),
                            "switch case target is not an instruction boundary at {}",
                            insn.pc
                        );
                    }
                }
            }
        }
        Ok(Self { instructions })
    }
}
