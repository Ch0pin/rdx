//! code_item: registers_size, ins_size, outs_size, tries_size, debug_info_off, insns_size, insns[], padding?, tries?, handlers?

use crate::error::{DexError, Result};
use crate::leb128::{read_sleb128, read_u16, read_u32, read_uleb128};

#[derive(Clone, Debug)]
pub struct CodeItem {
    pub registers_size: u16,
    pub ins_size: u16,
    pub outs_size: u16,
    pub tries_size: u16,
    pub debug_info_off: u32,
    /// Size in 16-bit code units (bytes = insns_size * 2).
    pub insns_size: u32,
    /// Offset into file where insns array starts (after code_item header).
    pub insns_off: usize,
    pub code_off: u32,
}

#[derive(Clone, Debug)]
pub struct TryItem {
    pub start_unit: u32,
    pub insn_count: u16,
    /// `(Some(type_idx), addr)` or `(None, addr)` for catch-all.
    pub handlers: Vec<(Option<u32>, u32)>,
}

impl CodeItem {
    /// Parse code_item at given file offset.
    pub fn parse(data: &[u8], code_off: u32) -> Result<Self> {
        let off = code_off as usize;
        if data.len() < off + 16 {
            return Err(DexError::Truncated("code_item header".into()));
        }
        let registers_size =
            read_u16(data, off).ok_or(DexError::Truncated("registers_size".into()))?;
        let ins_size = read_u16(data, off + 2).ok_or(DexError::Truncated("ins_size".into()))?;
        let outs_size = read_u16(data, off + 4).ok_or(DexError::Truncated("outs_size".into()))?;
        let tries_size = read_u16(data, off + 6).ok_or(DexError::Truncated("tries_size".into()))?;
        let debug_info_off =
            read_u32(data, off + 8).ok_or(DexError::Truncated("debug_info_off".into()))?;
        let insns_size = read_u32(data, off + 12).ok_or(DexError::Truncated("insns_size".into()))?;
        let insns_off = off + 16;
        let insns_bytes = (insns_size as usize).saturating_mul(2);
        if data.len() < insns_off + insns_bytes {
            return Err(DexError::Truncated("code_item insns".into()));
        }
        Ok(Self {
            registers_size,
            ins_size,
            outs_size,
            tries_size,
            debug_info_off,
            insns_size,
            insns_off,
            code_off,
        })
    }

    /// Byte slice of instructions for this code_item (insns array only).
    pub fn insns_slice<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let len = (self.insns_size as usize).saturating_mul(2);
        let end = (self.insns_off + len).min(data.len());
        &data[self.insns_off..end]
    }

    /// Size of insns in 16-bit units.
    pub fn insns_size_units(&self) -> usize {
        self.insns_size as usize
    }

    /// Parse try_item + handlers for this code_item.
    pub fn tries(&self, data: &[u8]) -> Result<Vec<TryItem>> {
        if self.tries_size == 0 {
            return Ok(Vec::new());
        }
        let insns_bytes = (self.insns_size as usize) * 2;
        let mut pos = self.insns_off + insns_bytes;
        if self.insns_size % 2 == 1 {
            pos += 2;
        }
        let tries_pos = pos;
        let handlers_base = pos + self.tries_size as usize * 8;

        let mut out = Vec::with_capacity(self.tries_size as usize);
        for i in 0..self.tries_size as usize {
            let base = tries_pos + i * 8;
            let start_unit =
                read_u32(data, base).ok_or(DexError::Truncated("try start".into()))?;
            let insn_count =
                read_u16(data, base + 4).ok_or(DexError::Truncated("try count".into()))?;
            let handler_off =
                read_u16(data, base + 6).ok_or(DexError::Truncated("try handler_off".into()))?
                    as usize;
            let mut hpos = handlers_base + handler_off;
            let (size, n) =
                read_sleb128(data, hpos).ok_or(DexError::Truncated("catch size".into()))?;
            hpos += n;
            let mut handlers = Vec::new();
            let typed = size.unsigned_abs();
            for _ in 0..typed {
                let (ty, n1) =
                    read_uleb128(data, hpos).ok_or(DexError::Truncated("catch type".into()))?;
                hpos += n1;
                let (addr, n2) =
                    read_uleb128(data, hpos).ok_or(DexError::Truncated("catch addr".into()))?;
                hpos += n2;
                handlers.push((Some(ty), addr));
            }
            if size <= 0 {
                let (addr, _) =
                    read_uleb128(data, hpos).ok_or(DexError::Truncated("catchall addr".into()))?;
                handlers.push((None, addr));
            }
            out.push(TryItem {
                start_unit,
                insn_count,
                handlers,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_item_parse_minimal() {
        let mut data = vec![0u8; 18];
        data[0..2].copy_from_slice(&(2u16).to_le_bytes());
        data[12..16].copy_from_slice(&(1u32).to_le_bytes());
        data[16..18].copy_from_slice(&[0x0e, 0x00]);
        let code = CodeItem::parse(&data, 0).unwrap();
        assert_eq!(code.registers_size, 2);
        assert_eq!(code.insns_size, 1);
        assert_eq!(code.insns_slice(&data), &[0x0e, 0x00]);
    }

    #[test]
    fn code_item_parse_truncated_header() {
        let data = vec![0u8; 8];
        assert!(CodeItem::parse(&data, 0).is_err());
    }

    #[test]
    fn code_item_parse_truncated_insns() {
        let mut data = vec![0u8; 16];
        data[12..16].copy_from_slice(&(100u32).to_le_bytes());
        assert!(CodeItem::parse(&data, 0).is_err());
    }
}
