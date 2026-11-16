use crate::error::{Error, Result};

use super::CodeItem;

#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    PackedSwitch {
        first_key: i32,
        targets: Vec<i32>,
    },
    SparseSwitch {
        keys: Vec<i32>,
        targets: Vec<i32>,
    },
    FillArray {
        element_width: u16,
        size: u32,
        data: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operands {
    None,
    Register(u16),
    Registers2 {
        a: u16,
        b: u16,
    },
    Registers3 {
        a: u16,
        b: u16,
        c: u16,
    },
    RegisterLiteral {
        a: u16,
        value: i64,
    },
    Registers2Literal {
        a: u16,
        b: u16,
        value: i32,
    },
    RegisterIndex {
        a: u16,
        index: u32,
    },
    Registers2Index {
        a: u16,
        b: u16,
        index: u32,
    },
    Invoke {
        registers: [u16; 5],
        count: u8,
        index: u32,
    },
    InvokeRange {
        start: u16,
        count: u8,
        index: u32,
    },
    Branch {
        registers: [u16; 2],
        count: u8,
        relative: i32,
    },
    PayloadOffset {
        register: u16,
        relative: i32,
    },
    InvokePolymorphic {
        registers: [u16; 5],
        count: u8,
        method_idx: u32,
        proto_idx: u32,
    },
    InvokePolymorphicRange {
        start: u16,
        count: u8,
        method_idx: u32,
        proto_idx: u32,
    },
    Payload(Payload),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedInstruction {
    pub offset: u32,
    pub opcode: u16,
    pub width: u32,
    pub operands: Operands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InlineBranchTargets {
    targets: [Option<u32>; 2],
    len: u8,
}

impl InlineBranchTargets {
    pub fn as_slice(&self) -> &[Option<u32>] {
        &self.targets[..usize::from(self.len)]
    }
}

impl DecodedInstruction {
    pub fn mnemonic(&self) -> &'static str {
        mnemonic(self.opcode)
    }

    pub fn branch_targets(&self) -> Option<InlineBranchTargets> {
        match &self.operands {
            Operands::Branch { relative, .. } => {
                if (0x28..=0x2a).contains(&self.opcode) {
                    Some(InlineBranchTargets {
                        targets: [relative_target(self.offset, *relative), None],
                        len: 1,
                    })
                } else {
                    Some(InlineBranchTargets {
                        targets: [
                            self.offset.checked_add(self.width),
                            relative_target(self.offset, *relative),
                        ],
                        len: 2,
                    })
                }
            }
            _ if matches!(self.opcode, 0x0e..=0x11 | 0x27) => Some(InlineBranchTargets {
                targets: [None, None],
                len: 1,
            }),
            _ => None,
        }
    }
}

fn relative_target(offset: u32, relative_units: i32) -> Option<u32> {
    let target = i64::from(offset) + i64::from(relative_units) * 2;
    u32::try_from(target).ok()
}

pub fn decode(code: &CodeItem<'_>) -> Result<Vec<DecodedInstruction>> {
    let bytes = code.instruction_bytes();
    let mut instructions = Vec::with_capacity(bytes.len() / 4);
    let mut unit_offset = 0_usize;
    while unit_offset < code.insns_size as usize {
        let first = unit(bytes, unit_offset)?;
        let payload_ident = first;
        let byte_offset = u32::try_from(unit_offset * 2)
            .map_err(|_| Error::data(unit_offset * 2, "instruction offset overflow"))?;
        let (opcode, width_units, operands) = if first & 0xff == 0 && first != 0 {
            decode_payload(bytes, unit_offset, first)?
        } else {
            let opcode = (first & 0xff) as u8;
            let (width, operands) = decode_normal(bytes, unit_offset, opcode, first)?;
            (u16::from(opcode), width, operands)
        };
        if width_units == 0 {
            return Err(Error::data(unit_offset * 2, "zero-width instruction"));
        }
        let end = unit_offset
            .checked_add(width_units)
            .ok_or_else(|| Error::data(unit_offset * 2, "instruction width overflow"))?;
        if end > code.insns_size as usize {
            return Err(Error::data(
                unit_offset * 2,
                "instruction extends beyond code_item",
            ));
        }
        instructions.push(DecodedInstruction {
            offset: byte_offset,
            opcode,
            width: u32::try_from(width_units * 2)
                .map_err(|_| Error::data(unit_offset * 2, "instruction width overflow"))?,
            operands,
        });
        unit_offset = end;

        if payload_ident == 0 {
            debug_assert_eq!(opcode, 0);
        }
    }
    Ok(instructions)
}

fn decode_normal(bytes: &[u8], offset: usize, opcode: u8, first: u16) -> Result<(usize, Operands)> {
    let high = (first >> 8) as u8;
    let low_nibble = u16::from(high & 0x0f);
    let high_nibble = u16::from(high >> 4);
    let at = offset
        .checked_mul(2)
        .ok_or_else(|| Error::data(offset, "instruction offset overflow"))?;

    let decoded = match opcode {
        0x00 => {
            if high != 0 {
                return Err(Error::data(at, "unknown payload instruction"));
            }
            (1, Operands::None)
        }
        0x01 | 0x04 | 0x07 | 0x21 | 0x7b..=0x8f | 0xb0..=0xcf => (
            1,
            Operands::Registers2 {
                a: low_nibble,
                b: high_nibble,
            },
        ),
        0x02 | 0x05 | 0x08 => (
            2,
            Operands::Registers2 {
                a: u16::from(high),
                b: unit(bytes, offset + 1)?,
            },
        ),
        0x03 | 0x06 | 0x09 => {
            if high != 0 {
                return Err(Error::data(at, "non-zero padding in 32x instruction"));
            }
            (
                3,
                Operands::Registers2 {
                    a: unit(bytes, offset + 1)?,
                    b: unit(bytes, offset + 2)?,
                },
            )
        }
        0x0a..=0x0d | 0x0f..=0x11 | 0x1d | 0x1e | 0x27 => (1, Operands::Register(u16::from(high))),
        0x0e => {
            if high != 0 {
                return Err(Error::data(at, "non-zero padding in 10x instruction"));
            }
            (1, Operands::None)
        }
        0x12 => (
            1,
            Operands::RegisterLiteral {
                a: low_nibble,
                value: i64::from(sign_extend_4(high >> 4)),
            },
        ),
        0x13 | 0x16 => (
            2,
            Operands::RegisterLiteral {
                a: u16::from(high),
                value: i64::from(unit(bytes, offset + 1)? as i16),
            },
        ),
        0x14 | 0x17 => (
            3,
            Operands::RegisterLiteral {
                a: u16::from(high),
                value: i64::from(i32_units(bytes, offset + 1)?),
            },
        ),
        0x15 => (
            2,
            Operands::RegisterLiteral {
                a: u16::from(high),
                value: i64::from(unit(bytes, offset + 1)? as i16) << 16,
            },
        ),
        0x18 => (
            5,
            Operands::RegisterLiteral {
                a: u16::from(high),
                value: i64_units(bytes, offset + 1)?,
            },
        ),
        0x19 => (
            2,
            Operands::RegisterLiteral {
                a: u16::from(high),
                value: i64::from(unit(bytes, offset + 1)? as i16) << 48,
            },
        ),
        0x1a | 0x1c | 0x1f | 0x22 | 0x60..=0x6d | 0xfe | 0xff => (
            2,
            Operands::RegisterIndex {
                a: u16::from(high),
                index: u32::from(unit(bytes, offset + 1)?),
            },
        ),
        0x1b => (
            3,
            Operands::RegisterIndex {
                a: u16::from(high),
                index: u32_units(bytes, offset + 1)?,
            },
        ),
        0x20 | 0x23 | 0x52..=0x5f => (
            2,
            Operands::Registers2Index {
                a: low_nibble,
                b: high_nibble,
                index: u32::from(unit(bytes, offset + 1)?),
            },
        ),
        0x24 | 0x6e..=0x72 | 0xfc => {
            let count = high >> 4;
            if count > 5 {
                return Err(Error::data(at, "35c register count exceeds five"));
            }
            let register_word = unit(bytes, offset + 2)?;
            (
                3,
                Operands::Invoke {
                    registers: [
                        register_word & 0x0f,
                        (register_word >> 4) & 0x0f,
                        (register_word >> 8) & 0x0f,
                        (register_word >> 12) & 0x0f,
                        u16::from(high & 0x0f),
                    ],
                    count,
                    index: u32::from(unit(bytes, offset + 1)?),
                },
            )
        }
        0x25 | 0x74..=0x78 | 0xfd => (
            3,
            Operands::InvokeRange {
                start: unit(bytes, offset + 2)?,
                count: high,
                index: u32::from(unit(bytes, offset + 1)?),
            },
        ),
        0x26 | 0x2b | 0x2c => (
            3,
            Operands::PayloadOffset {
                register: u16::from(high),
                relative: i32_units(bytes, offset + 1)?,
            },
        ),
        0x28 => (
            1,
            Operands::Branch {
                registers: [0, 0],
                count: 0,
                relative: i32::from(high as i8),
            },
        ),
        0x29 => {
            if high != 0 {
                return Err(Error::data(at, "non-zero padding in 20t instruction"));
            }
            (
                2,
                Operands::Branch {
                    registers: [0, 0],
                    count: 0,
                    relative: i32::from(unit(bytes, offset + 1)? as i16),
                },
            )
        }
        0x2a => {
            if high != 0 {
                return Err(Error::data(at, "non-zero padding in 30t instruction"));
            }
            (
                3,
                Operands::Branch {
                    registers: [0, 0],
                    count: 0,
                    relative: i32_units(bytes, offset + 1)?,
                },
            )
        }
        0x2d..=0x31 | 0x44..=0x51 | 0x90..=0xaf => {
            let second = unit(bytes, offset + 1)?;
            (
                2,
                Operands::Registers3 {
                    a: u16::from(high),
                    b: second & 0xff,
                    c: second >> 8,
                },
            )
        }
        0x32..=0x37 => (
            2,
            Operands::Branch {
                registers: [low_nibble, high_nibble],
                count: 2,
                relative: i32::from(unit(bytes, offset + 1)? as i16),
            },
        ),
        0x38..=0x3d => (
            2,
            Operands::Branch {
                registers: [u16::from(high), 0],
                count: 1,
                relative: i32::from(unit(bytes, offset + 1)? as i16),
            },
        ),
        0xd0..=0xd7 => (
            2,
            Operands::Registers2Literal {
                a: low_nibble,
                b: high_nibble,
                value: i32::from(unit(bytes, offset + 1)? as i16),
            },
        ),
        0xd8..=0xe2 => {
            let second = unit(bytes, offset + 1)?;
            (
                2,
                Operands::Registers2Literal {
                    a: u16::from(high),
                    b: second & 0xff,
                    value: i32::from((second >> 8) as u8 as i8),
                },
            )
        }
        0xfa => {
            let count = high >> 4;
            if count > 5 {
                return Err(Error::data(at, "45cc register count exceeds five"));
            }
            let register_word = unit(bytes, offset + 2)?;
            (
                4,
                Operands::InvokePolymorphic {
                    registers: [
                        register_word & 0x0f,
                        (register_word >> 4) & 0x0f,
                        (register_word >> 8) & 0x0f,
                        (register_word >> 12) & 0x0f,
                        u16::from(high & 0x0f),
                    ],
                    count,
                    method_idx: u32::from(unit(bytes, offset + 1)?),
                    proto_idx: u32::from(unit(bytes, offset + 3)?),
                },
            )
        }
        0xfb => (
            4,
            Operands::InvokePolymorphicRange {
                start: unit(bytes, offset + 2)?,
                count: high,
                method_idx: u32::from(unit(bytes, offset + 1)?),
                proto_idx: u32::from(unit(bytes, offset + 3)?),
            },
        ),
        0x3e..=0x43 | 0x73 | 0x79 | 0x7a | 0xe3..=0xf9 => {
            return Err(Error::data(at, "unused Dalvik opcode"));
        }
    };
    Ok(decoded)
}

fn decode_payload(bytes: &[u8], offset: usize, ident: u16) -> Result<(u16, usize, Operands)> {
    let at = offset
        .checked_mul(2)
        .ok_or_else(|| Error::data(offset, "payload offset overflow"))?;
    match ident {
        0x0100 => {
            let size = unit(bytes, offset + 1)? as usize;
            let width = 4_usize
                .checked_add(
                    size.checked_mul(2)
                        .ok_or_else(|| Error::data(at, "packed-switch size overflow"))?,
                )
                .ok_or_else(|| Error::data(at, "packed-switch size overflow"))?;
            require_units(bytes, offset, width)?;
            let first_key = i32_units(bytes, offset + 2)?;
            let mut targets = Vec::with_capacity(size);
            for index in 0..size {
                targets.push(i32_units(bytes, offset + 4 + index * 2)?);
            }
            Ok((
                ident,
                width,
                Operands::Payload(Payload::PackedSwitch { first_key, targets }),
            ))
        }
        0x0200 => {
            let size = unit(bytes, offset + 1)? as usize;
            let width = 2_usize
                .checked_add(
                    size.checked_mul(4)
                        .ok_or_else(|| Error::data(at, "sparse-switch size overflow"))?,
                )
                .ok_or_else(|| Error::data(at, "sparse-switch size overflow"))?;
            require_units(bytes, offset, width)?;
            let mut keys = Vec::with_capacity(size);
            let mut targets = Vec::with_capacity(size);
            for index in 0..size {
                keys.push(i32_units(bytes, offset + 2 + index * 2)?);
                targets.push(i32_units(bytes, offset + 2 + size * 2 + index * 2)?);
            }
            Ok((
                ident,
                width,
                Operands::Payload(Payload::SparseSwitch { keys, targets }),
            ))
        }
        0x0300 => {
            let element_width = unit(bytes, offset + 1)?;
            let size = u32_units(bytes, offset + 2)?;
            let data_len = usize::try_from(size)
                .ok()
                .and_then(|count| count.checked_mul(element_width as usize))
                .ok_or_else(|| Error::data(at, "fill-array-data size overflow"))?;
            let data_units = data_len
                .checked_add(1)
                .map(|value| value / 2)
                .ok_or_else(|| Error::data(at, "fill-array-data size overflow"))?;
            let width = 4_usize
                .checked_add(data_units)
                .ok_or_else(|| Error::data(at, "fill-array-data size overflow"))?;
            require_units(bytes, offset, width)?;
            let data_start = (offset + 4) * 2;
            let data = bytes[data_start..data_start + data_len].to_vec();
            Ok((
                ident,
                width,
                Operands::Payload(Payload::FillArray {
                    element_width,
                    size,
                    data,
                }),
            ))
        }
        _ => Err(Error::data(at, "unknown Dalvik payload opcode")),
    }
}

fn unit(bytes: &[u8], offset: usize) -> Result<u16> {
    let byte_offset = offset
        .checked_mul(2)
        .ok_or_else(|| Error::data(offset, "instruction offset overflow"))?;
    let pair = bytes
        .get(byte_offset..byte_offset + 2)
        .ok_or(Error::UnexpectedEof {
            offset: byte_offset,
            needed: 2,
            len: bytes.len(),
        })?;
    Ok(u16::from_le_bytes([pair[0], pair[1]]))
}

fn require_units(bytes: &[u8], offset: usize, width: usize) -> Result<()> {
    let end = offset
        .checked_add(width)
        .and_then(|units| units.checked_mul(2))
        .ok_or_else(|| Error::data(offset * 2, "instruction size overflow"))?;
    if end > bytes.len() {
        return Err(Error::UnexpectedEof {
            offset: offset * 2,
            needed: width * 2,
            len: bytes.len(),
        });
    }
    Ok(())
}

fn u32_units(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from(unit(bytes, offset)?) | (u32::from(unit(bytes, offset + 1)?) << 16))
}

fn i32_units(bytes: &[u8], offset: usize) -> Result<i32> {
    Ok(u32_units(bytes, offset)? as i32)
}

fn i64_units(bytes: &[u8], offset: usize) -> Result<i64> {
    let low = u64::from(u32_units(bytes, offset)?);
    let high = u64::from(u32_units(bytes, offset + 2)?);
    Ok((low | (high << 32)) as i64)
}

fn sign_extend_4(value: u8) -> i8 {
    ((value << 4) as i8) >> 4
}

pub fn mnemonic(opcode: u16) -> &'static str {
    match opcode {
        0x0000 => "nop",
        0x0001 => "move",
        0x0002 => "move/from16",
        0x0003 => "move/16",
        0x0004 => "move-wide",
        0x0005 => "move-wide/from16",
        0x0006 => "move-wide/16",
        0x0007 => "move-object",
        0x0008 => "move-object/from16",
        0x0009 => "move-object/16",
        0x000a => "move-result",
        0x000b => "move-result-wide",
        0x000c => "move-result-object",
        0x000d => "move-exception",
        0x000e => "return-void",
        0x000f => "return",
        0x0010 => "return-wide",
        0x0011 => "return-object",
        0x0012 => "const/4",
        0x0013 => "const/16",
        0x0014 => "const",
        0x0015 => "const/high16",
        0x0016 => "const-wide/16",
        0x0017 => "const-wide/32",
        0x0018 => "const-wide",
        0x0019 => "const-wide/high16",
        0x001a => "const-string",
        0x001b => "const-string/jumbo",
        0x001c => "const-class",
        0x001d => "monitor-enter",
        0x001e => "monitor-exit",
        0x001f => "check-cast",
        0x0020 => "instance-of",
        0x0021 => "array-length",
        0x0022 => "new-instance",
        0x0023 => "new-array",
        0x0024 => "filled-new-array",
        0x0025 => "filled-new-array/range",
        0x0026 => "fill-array-data",
        0x0027 => "throw",
        0x0028 => "goto",
        0x0029 => "goto/16",
        0x002a => "goto/32",
        0x002b => "packed-switch",
        0x002c => "sparse-switch",
        0x002d => "cmpl-float",
        0x002e => "cmpg-float",
        0x002f => "cmpl-double",
        0x0030 => "cmpg-double",
        0x0031 => "cmp-long",
        0x0032 => "if-eq",
        0x0033 => "if-ne",
        0x0034 => "if-lt",
        0x0035 => "if-ge",
        0x0036 => "if-gt",
        0x0037 => "if-le",
        0x0038 => "if-eqz",
        0x0039 => "if-nez",
        0x003a => "if-ltz",
        0x003b => "if-gez",
        0x003c => "if-gtz",
        0x003d => "if-lez",
        0x0044 => "aget",
        0x0045 => "aget-wide",
        0x0046 => "aget-object",
        0x0047 => "aget-boolean",
        0x0048 => "aget-byte",
        0x0049 => "aget-char",
        0x004a => "aget-short",
        0x004b => "aput",
        0x004c => "aput-wide",
        0x004d => "aput-object",
        0x004e => "aput-boolean",
        0x004f => "aput-byte",
        0x0050 => "aput-char",
        0x0051 => "aput-short",
        0x0052 => "iget",
        0x0053 => "iget-wide",
        0x0054 => "iget-object",
        0x0055 => "iget-boolean",
        0x0056 => "iget-byte",
        0x0057 => "iget-char",
        0x0058 => "iget-short",
        0x0059 => "iput",
        0x005a => "iput-wide",
        0x005b => "iput-object",
        0x005c => "iput-boolean",
        0x005d => "iput-byte",
        0x005e => "iput-char",
        0x005f => "iput-short",
        0x0060 => "sget",
        0x0061 => "sget-wide",
        0x0062 => "sget-object",
        0x0063 => "sget-boolean",
        0x0064 => "sget-byte",
        0x0065 => "sget-char",
        0x0066 => "sget-short",
        0x0067 => "sput",
        0x0068 => "sput-wide",
        0x0069 => "sput-object",
        0x006a => "sput-boolean",
        0x006b => "sput-byte",
        0x006c => "sput-char",
        0x006d => "sput-short",
        0x006e => "invoke-virtual",
        0x006f => "invoke-super",
        0x0070 => "invoke-direct",
        0x0071 => "invoke-static",
        0x0072 => "invoke-interface",
        0x0074 => "invoke-virtual/range",
        0x0075 => "invoke-super/range",
        0x0076 => "invoke-direct/range",
        0x0077 => "invoke-static/range",
        0x0078 => "invoke-interface/range",
        0x007b => "neg-int",
        0x007c => "not-int",
        0x007d => "neg-long",
        0x007e => "not-long",
        0x007f => "neg-float",
        0x0080 => "neg-double",
        0x0081 => "int-to-long",
        0x0082 => "int-to-float",
        0x0083 => "int-to-double",
        0x0084 => "long-to-int",
        0x0085 => "long-to-float",
        0x0086 => "long-to-double",
        0x0087 => "float-to-int",
        0x0088 => "float-to-long",
        0x0089 => "float-to-double",
        0x008a => "double-to-int",
        0x008b => "double-to-long",
        0x008c => "double-to-float",
        0x008d => "int-to-byte",
        0x008e => "int-to-char",
        0x008f => "int-to-short",
        0x0090 => "add-int",
        0x0091 => "sub-int",
        0x0092 => "mul-int",
        0x0093 => "div-int",
        0x0094 => "rem-int",
        0x0095 => "and-int",
        0x0096 => "or-int",
        0x0097 => "xor-int",
        0x0098 => "shl-int",
        0x0099 => "shr-int",
        0x009a => "ushr-int",
        0x009b => "add-long",
        0x009c => "sub-long",
        0x009d => "mul-long",
        0x009e => "div-long",
        0x009f => "rem-long",
        0x00a0 => "and-long",
        0x00a1 => "or-long",
        0x00a2 => "xor-long",
        0x00a3 => "shl-long",
        0x00a4 => "shr-long",
        0x00a5 => "ushr-long",
        0x00a6 => "add-float",
        0x00a7 => "sub-float",
        0x00a8 => "mul-float",
        0x00a9 => "div-float",
        0x00aa => "rem-float",
        0x00ab => "add-double",
        0x00ac => "sub-double",
        0x00ad => "mul-double",
        0x00ae => "div-double",
        0x00af => "rem-double",
        0x00b0 => "add-int/2addr",
        0x00b1 => "sub-int/2addr",
        0x00b2 => "mul-int/2addr",
        0x00b3 => "div-int/2addr",
        0x00b4 => "rem-int/2addr",
        0x00b5 => "and-int/2addr",
        0x00b6 => "or-int/2addr",
        0x00b7 => "xor-int/2addr",
        0x00b8 => "shl-int/2addr",
        0x00b9 => "shr-int/2addr",
        0x00ba => "ushr-int/2addr",
        0x00bb => "add-long/2addr",
        0x00bc => "sub-long/2addr",
        0x00bd => "mul-long/2addr",
        0x00be => "div-long/2addr",
        0x00bf => "rem-long/2addr",
        0x00c0 => "and-long/2addr",
        0x00c1 => "or-long/2addr",
        0x00c2 => "xor-long/2addr",
        0x00c3 => "shl-long/2addr",
        0x00c4 => "shr-long/2addr",
        0x00c5 => "ushr-long/2addr",
        0x00c6 => "add-float/2addr",
        0x00c7 => "sub-float/2addr",
        0x00c8 => "mul-float/2addr",
        0x00c9 => "div-float/2addr",
        0x00ca => "rem-float/2addr",
        0x00cb => "add-double/2addr",
        0x00cc => "sub-double/2addr",
        0x00cd => "mul-double/2addr",
        0x00ce => "div-double/2addr",
        0x00cf => "rem-double/2addr",
        0x00d0 => "add-int/lit16",
        0x00d1 => "rsub-int",
        0x00d2 => "mul-int/lit16",
        0x00d3 => "div-int/lit16",
        0x00d4 => "rem-int/lit16",
        0x00d5 => "and-int/lit16",
        0x00d6 => "or-int/lit16",
        0x00d7 => "xor-int/lit16",
        0x00d8 => "add-int/lit8",
        0x00d9 => "rsub-int/lit8",
        0x00da => "mul-int/lit8",
        0x00db => "div-int/lit8",
        0x00dc => "rem-int/lit8",
        0x00dd => "and-int/lit8",
        0x00de => "or-int/lit8",
        0x00df => "xor-int/lit8",
        0x00e0 => "shl-int/lit8",
        0x00e1 => "shr-int/lit8",
        0x00e2 => "ushr-int/lit8",
        0x00fa => "invoke-polymorphic",
        0x00fb => "invoke-polymorphic/range",
        0x00fc => "invoke-custom",
        0x00fd => "invoke-custom/range",
        0x00fe => "const-method-handle",
        0x00ff => "const-method-type",
        0x0100 => "packed-switch-payload",
        0x0200 => "sparse-switch-payload",
        0x0300 => "fill-array-data-payload",
        _ => "unused",
    }
}
