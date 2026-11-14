//! Allocation-free raw Dalvik instruction decoder (no operand strings).

use core::convert::TryInto;

use crate::error::DexError;
use crate::instruction::RefKind;
use crate::opcodes::{format_length, get_opcode_entry, get_payload_kind, Format, PayloadKind};

/// Decoded instruction operands without heap allocation.
#[derive(Clone, Copy, Debug, Default)]
pub struct RawInsn {
    pub offset: u32,
    pub length: u32,
    pub opcode: u8,
    pub format: Format,
    pub regs: [u16; 5],
    pub reg_count: u8,
    /// string/type/field/method/proto/call-site index (0 if none)
    pub index: u32,
    pub literal: i64,
    /// relative branch in 16-bit code units (0 if none)
    pub branch: i32,
    pub ref_kind: RefKind,
}

#[inline]
fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let end = offset + 2;
    if end > data.len() {
        return None;
    }
    let bytes: [u8; 2] = data[offset..end].try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

#[inline]
fn read_i16(data: &[u8], offset: usize) -> Option<i16> {
    read_u16(data, offset).map(|u| u as i16)
}

#[inline]
fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let end = offset + 4;
    if end > data.len() {
        return None;
    }
    let bytes: [u8; 4] = data[offset..end].try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

#[inline]
fn read_i32(data: &[u8], offset: usize) -> Option<i32> {
    read_u32(data, offset).map(|u| u as i32)
}

#[inline]
fn read_u64(data: &[u8], offset: usize) -> Option<u64> {
    let end = offset + 8;
    if end > data.len() {
        return None;
    }
    let bytes: [u8; 8] = data[offset..end].try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

#[inline]
fn high_byte(data: &[u8], offset: usize) -> u8 {
    data.get(offset + 1).copied().unwrap_or(0)
}

#[inline]
fn ab_from_first(data: &[u8], offset: usize) -> (u8, u8) {
    let w = read_u16(data, offset).unwrap_or(0);
    let a = ((w >> 8) & 0x0F) as u8;
    let b = ((w >> 12) & 0x0F) as u8;
    (a, b)
}

#[inline]
fn decode_35c(data: &[u8], offset: usize) -> (u8, u8, u8, u8, u8, u8) {
    let w1 = read_u16(data, offset).unwrap_or(0);
    let w2 = read_u16(data, offset + 4).unwrap_or(0);
    let a = ((w1 >> 12) & 0x0F) as u8;
    let g = ((w1 >> 8) & 0x0F) as u8;
    let c = (w2 & 0x0F) as u8;
    let d = ((w2 >> 4) & 0x0F) as u8;
    let e = ((w2 >> 8) & 0x0F) as u8;
    let f = ((w2 >> 12) & 0x0F) as u8;
    (a, g, c, d, e, f)
}

#[inline]
fn push_reg(out: &mut RawInsn, r: u16) {
    if (out.reg_count as usize) < out.regs.len() {
        out.regs[out.reg_count as usize] = r;
        out.reg_count = out.reg_count.saturating_add(1);
    }
}

fn push_35_regs(out: &mut RawInsn, a: u8, c: u8, d: u8, e: u8, f: u8, g: u8) {
    match a {
        0 => {}
        1 => push_reg(out, c as u16),
        2 => {
            push_reg(out, c as u16);
            push_reg(out, d as u16);
        }
        3 => {
            push_reg(out, c as u16);
            push_reg(out, d as u16);
            push_reg(out, e as u16);
        }
        4 => {
            push_reg(out, c as u16);
            push_reg(out, d as u16);
            push_reg(out, e as u16);
            push_reg(out, f as u16);
        }
        5 => {
            push_reg(out, c as u16);
            push_reg(out, d as u16);
            push_reg(out, e as u16);
            push_reg(out, f as u16);
            push_reg(out, g as u16);
        }
        _ => {}
    }
}

fn decode_payload_raw(data: &[u8], offset: usize, kind: PayloadKind) -> Result<RawInsn, DexError> {
    let (len, opcode) = match kind {
        PayloadKind::PackedSwitch => {
            if data.len() < offset + 8 {
                return Err(DexError::invalid("truncated packed-switch-payload"));
            }
            let size = read_u16(data, offset + 2).unwrap_or(0) as usize;
            (4 + 4 + size * 4, 0x0100u16)
        }
        PayloadKind::SparseSwitch => {
            if data.len() < offset + 4 {
                return Err(DexError::invalid("truncated sparse-switch-payload"));
            }
            let size = read_u16(data, offset + 2).unwrap_or(0) as usize;
            (4 + size * 4 + size * 4, 0x0200u16)
        }
        PayloadKind::FillArrayData => {
            if data.len() < offset + 8 {
                return Err(DexError::invalid("truncated fill-array-data-payload"));
            }
            let elem_width = read_u16(data, offset + 2).unwrap_or(0) as usize;
            let size = read_u32(data, offset + 4).unwrap_or(0) as usize;
            let data_len = size * elem_width;
            let padded = (data_len + 1) & !1;
            (8 + padded, 0x0300u16)
        }
    };
    if offset + len > data.len() {
        return Err(DexError::invalid("payload extends past buffer"));
    }
    Ok(RawInsn {
        offset: offset as u32,
        length: len as u32,
        opcode: (opcode & 0xFF) as u8,
        format: Format::F00x,
        ..RawInsn::default()
    })
}

fn decode_format_raw(
    data: &[u8],
    offset: usize,
    opcode: u8,
    entry: &crate::opcodes::OpcodeEntry,
    len: usize,
) -> Result<RawInsn, DexError> {
    let mut out = RawInsn {
        offset: offset as u32,
        length: len as u32,
        opcode,
        format: entry.format,
        ref_kind: entry.ref_kind,
        ..RawInsn::default()
    };

    match entry.format {
        Format::F10x => {}
        Format::F10t => {
            out.branch = high_byte(data, offset) as i8 as i32;
        }
        Format::F11n => {
            let byte = data[offset + 1] as i8;
            push_reg(&mut out, (data[offset + 1] & 0x0F) as u16);
            out.literal = (byte >> 4) as i64;
        }
        Format::F11x => push_reg(&mut out, high_byte(data, offset) as u16),
        Format::F12x => {
            let (a, b) = ab_from_first(data, offset);
            push_reg(&mut out, a as u16);
            push_reg(&mut out, b as u16);
        }
        Format::F20t => {
            out.branch = read_i16(data, offset + 2).unwrap_or(0) as i32;
        }
        Format::F20bc => {
            push_reg(&mut out, data[offset + 1] as u16);
            out.index = read_u16(data, offset + 2).unwrap_or(0) as u32;
        }
        Format::F21c => {
            push_reg(&mut out, data[offset + 1] as u16);
            out.index = read_u16(data, offset + 2).unwrap_or(0) as u32;
        }
        Format::F21h => {
            push_reg(&mut out, data[offset + 1] as u16);
            let bbbb = read_i16(data, offset + 2).unwrap_or(0);
            out.literal = if opcode == 0x15 {
                (bbbb as i64) << 16
            } else if opcode == 0x19 {
                (bbbb as i64) << 48
            } else {
                bbbb as i64
            };
        }
        Format::F21s => {
            push_reg(&mut out, data[offset + 1] as u16);
            out.literal = read_i16(data, offset + 2).unwrap_or(0) as i64;
        }
        Format::F21t => {
            push_reg(&mut out, data[offset + 1] as u16);
            out.branch = read_i16(data, offset + 2).unwrap_or(0) as i32;
        }
        Format::F22b => {
            push_reg(&mut out, data[offset + 1] as u16);
            push_reg(&mut out, data[offset + 2] as u16);
            out.literal = data[offset + 3] as i8 as i64;
        }
        Format::F22c => {
            let (a, b) = ab_from_first(data, offset);
            push_reg(&mut out, a as u16);
            push_reg(&mut out, b as u16);
            out.index = read_u16(data, offset + 2).unwrap_or(0) as u32;
        }
        Format::F22s => {
            let (a, b) = ab_from_first(data, offset);
            push_reg(&mut out, a as u16);
            push_reg(&mut out, b as u16);
            out.literal = read_i16(data, offset + 2).unwrap_or(0) as i64;
        }
        Format::F22t => {
            let (a, b) = ab_from_first(data, offset);
            push_reg(&mut out, a as u16);
            push_reg(&mut out, b as u16);
            out.branch = read_i16(data, offset + 2).unwrap_or(0) as i32;
        }
        Format::F22cs => {
            let (a, b) = ab_from_first(data, offset);
            push_reg(&mut out, a as u16);
            push_reg(&mut out, b as u16);
            out.index = read_u16(data, offset + 2).unwrap_or(0) as u32;
        }
        Format::F23x => {
            push_reg(&mut out, data[offset + 1] as u16);
            push_reg(&mut out, data[offset + 2] as u16);
            push_reg(&mut out, data[offset + 3] as u16);
        }
        Format::F30t => {
            out.branch = read_i32(data, offset + 2).unwrap_or(0);
        }
        Format::F31c => {
            push_reg(&mut out, data[offset + 1] as u16);
            out.index = read_u32(data, offset + 2).unwrap_or(0);
        }
        Format::F31i => {
            push_reg(&mut out, data[offset + 1] as u16);
            out.literal = read_i32(data, offset + 2).unwrap_or(0) as i64;
        }
        Format::F31t => {
            push_reg(&mut out, data[offset + 1] as u16);
            out.branch = read_i32(data, offset + 2).unwrap_or(0);
        }
        Format::F32x => {
            push_reg(&mut out, read_u16(data, offset + 2).unwrap_or(0));
            push_reg(&mut out, read_u16(data, offset + 4).unwrap_or(0));
        }
        Format::F35c | Format::F35mi | Format::F35ms => {
            let (a, g, c, d, e, f) = decode_35c(data, offset);
            push_35_regs(&mut out, a, c, d, e, f, g);
            out.index = read_u16(data, offset + 2).unwrap_or(0) as u32;
        }
        Format::F3rc | Format::F3rmi | Format::F3rms => {
            let aa = data[offset + 1];
            out.index = read_u16(data, offset + 2).unwrap_or(0) as u32;
            let cccc = read_u16(data, offset + 4).unwrap_or(0);
            let nnnn = (cccc as u32).saturating_add(aa as u32).saturating_sub(1) as u16;
            push_reg(&mut out, cccc);
            if cccc != nnnn {
                push_reg(&mut out, nnnn);
            }
        }
        Format::F40sc => {
            out.index = read_u32(data, offset + 2).unwrap_or(0);
            push_reg(&mut out, read_u16(data, offset + 6).unwrap_or(0));
        }
        Format::F41c => {
            out.index = read_u32(data, offset + 2).unwrap_or(0);
            push_reg(&mut out, read_u16(data, offset + 6).unwrap_or(0));
        }
        Format::F45cc => {
            let (a, g, c, d, e, f) = decode_35c(data, offset);
            push_35_regs(&mut out, a, c, d, e, f, g);
            out.index = read_u16(data, offset + 2).unwrap_or(0) as u32;
            out.literal = read_u16(data, offset + 6).unwrap_or(0) as i64;
            out.ref_kind = entry.ref_kind;
        }
        Format::F4rcc => {
            let aa = data[offset + 1];
            out.index = read_u16(data, offset + 2).unwrap_or(0) as u32;
            let cccc = read_u16(data, offset + 4).unwrap_or(0);
            let nnnn = (cccc as u32).saturating_add(aa as u32).saturating_sub(1) as u16;
            push_reg(&mut out, cccc);
            if cccc != nnnn {
                push_reg(&mut out, nnnn);
            }
            out.literal = read_u16(data, offset + 6).unwrap_or(0) as i64;
        }
        Format::F51l => {
            push_reg(&mut out, data[offset + 1] as u16);
            out.literal = read_u64(data, offset + 2).unwrap_or(0) as i64;
        }
        Format::F52c => {
            out.index = read_u32(data, offset + 2).unwrap_or(0);
            push_reg(&mut out, read_u16(data, offset + 6).unwrap_or(0));
            push_reg(&mut out, read_u16(data, offset + 8).unwrap_or(0));
        }
        Format::F5rc => {
            out.index = read_u32(data, offset + 2).unwrap_or(0);
            let aaaa = read_u16(data, offset + 6).unwrap_or(0);
            let cccc = read_u16(data, offset + 8).unwrap_or(0);
            let nnnn = (cccc as u32).saturating_add(aaaa as u32).saturating_sub(1) as u16;
            push_reg(&mut out, cccc);
            if cccc != nnnn {
                push_reg(&mut out, nnnn);
            }
        }
        Format::F22x => {
            push_reg(&mut out, data[offset + 1] as u16);
            push_reg(&mut out, read_u16(data, offset + 2).unwrap_or(0));
        }
        Format::F00x => {}
    }

    Ok(out)
}

/// Decode a single instruction at `offset` without allocating operand strings.
pub fn decode_raw_one(data: &[u8], offset: usize) -> Result<RawInsn, DexError> {
    let unit = read_u16(data, offset)
        .ok_or_else(|| DexError::invalid("truncated at first 16-bit unit"))?;
    let op_byte = unit as u8;

    if unit > 0xFF && (op_byte == 0x00 || op_byte == 0xFF) {
        if let Some(payload) = get_payload_kind(unit) {
            return decode_payload_raw(data, offset, payload);
        }
    }

    let entry = get_opcode_entry(op_byte);
    let len = format_length(entry.format) as usize;

    if entry.format == Format::F00x {
        return Err(DexError::invalid_owned(format!(
            "unused or invalid opcode 0x{:02x}",
            op_byte
        )));
    }

    if offset + len > data.len() {
        return Err(DexError::invalid("instruction extends past buffer"));
    }

    decode_format_raw(data, offset, op_byte, entry, len)
}

/// Iterator over raw instructions until end-of-buffer or first decode error.
pub struct RawInsnIter<'a> {
    data: &'a [u8],
    offset: usize,
    done: bool,
}

impl<'a> RawInsnIter<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            offset: 0,
            done: false,
        }
    }
}

impl<'a> Iterator for RawInsnIter<'a> {
    type Item = Result<RawInsn, DexError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.offset >= self.data.len() {
            return None;
        }
        match decode_raw_one(self.data, self.offset) {
            Ok(ins) => {
                self.offset += ins.length as usize;
                Some(Ok(ins))
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

/// Linear-sweep iterator from the start of `data`.
pub fn decode_raw_iter(data: &[u8]) -> RawInsnIter<'_> {
    RawInsnIter::new(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{encode_goto, encode_nop, encode_return_void};

    #[test]
    fn raw_decode_nop_return_goto() {
        let mut buf = [0u8; 6];
        buf[0..2].copy_from_slice(&encode_nop());
        buf[2..4].copy_from_slice(&encode_return_void());
        buf[4..6].copy_from_slice(&encode_goto(2));

        let nop = decode_raw_one(&buf, 0).unwrap();
        assert_eq!(nop.opcode, 0x00);
        assert_eq!(nop.format, Format::F10x);

        let ret = decode_raw_one(&buf, 2).unwrap();
        assert_eq!(ret.opcode, 0x0e);

        let goto = decode_raw_one(&buf, 4).unwrap();
        assert_eq!(goto.opcode, 0x28);
        assert_eq!(goto.branch, 2);
    }
}
