//! DEX write helpers: patch/replace code_item insns and fix header checksum/signature.

use crate::dex::{ClassData, ClassDef, CodeItem, DexHeader, NO_INDEX};
use crate::error::{DexError, Result};
use crate::leb128::{read_sleb128, read_u32, read_uleb128, write_u32, write_uleb128};

/// Patch the instruction array of a `code_item` in-place (same byte length only).
pub fn patch_code_insns(dex: &mut [u8], code_off: u32, new_insns: &[u8]) -> Result<()> {
    let code = CodeItem::parse(dex, code_off)?;
    let old_len = (code.insns_size as usize).saturating_mul(2);
    if new_insns.len() != old_len {
        return Err(DexError::Parse(format!(
            "instruction size mismatch at code_off 0x{code_off:x}: expected {old_len} bytes, got {} (use replace_code_insns for variable-size edits)",
            new_insns.len()
        )));
    }
    let start = code.insns_off;
    let end = start + old_len;
    if end > dex.len() {
        return Err(DexError::Truncated("patch_code_insns".into()));
    }
    dex[start..end].copy_from_slice(new_insns);
    Ok(())
}

/// Replace the instruction array of a `code_item`, resizing the DEX when needed.
pub fn replace_code_insns(dex: &mut Vec<u8>, code_off: u32, new_insns: &[u8]) -> Result<()> {
    if new_insns.len() % 2 != 0 {
        return Err(DexError::Parse(format!(
            "instruction bytes must be an even length, got {}",
            new_insns.len()
        )));
    }
    let code = CodeItem::parse(dex, code_off)?;
    let old_insns_bytes = (code.insns_size as usize).saturating_mul(2);
    if new_insns.len() == old_insns_bytes {
        return patch_code_insns(dex, code_off, new_insns);
    }

    let off = code_off as usize;
    let total_old = code_item_byte_size(dex, code_off)?;
    let old_padding = insns_padding(code.tries_size, code.insns_size);
    let new_insns_size = (new_insns.len() / 2) as u32;
    let new_padding = insns_padding(code.tries_size, new_insns_size);

    let insns_off = code.insns_off;
    let tries_start_old = insns_off + old_insns_bytes + old_padding;
    let tail_start = tries_start_old + code.tries_size as usize * 8;
    let handlers_size = if code.tries_size > 0 {
        encoded_catch_handler_list_size(dex, tail_start)?
    } else {
        0
    };
    let tail_end = tail_start + handlers_size;

    let mut new_item = Vec::with_capacity(total_old + new_insns.len().saturating_sub(old_insns_bytes));
    new_item.extend_from_slice(&dex[off..insns_off]);
    new_item.extend_from_slice(new_insns);
    if new_padding > 0 {
        new_item.extend_from_slice(&[0u8; 2]);
    }
    if code.tries_size > 0 {
        new_item.extend_from_slice(&dex[tries_start_old..tail_end]);
    }
    new_item[12..16].copy_from_slice(&new_insns_size.to_le_bytes());

    let delta = new_item.len() as i64 - total_old as i64;
    dex.splice(off..off + total_old, new_item);
    if delta != 0 {
        adjust_file_offsets(dex, off + total_old, delta)?;
    }
    Ok(())
}

fn insns_padding(tries_size: u16, insns_size: u32) -> usize {
    if tries_size > 0 && insns_size % 2 == 1 {
        2
    } else {
        0
    }
}

fn code_item_byte_size(data: &[u8], code_off: u32) -> Result<usize> {
    let code = CodeItem::parse(data, code_off)?;
    let off = code_off as usize;
    let insns_bytes = (code.insns_size as usize).saturating_mul(2);
    let mut end = code.insns_off + insns_bytes + insns_padding(code.tries_size, code.insns_size);
    end += code.tries_size as usize * 8;
    if code.tries_size > 0 {
        end += encoded_catch_handler_list_size(data, end)?;
    }
    Ok(end - off)
}

fn encoded_catch_handler_list_size(data: &[u8], off: usize) -> Result<usize> {
    let (size, n) = read_uleb128(data, off).ok_or(DexError::Truncated("handlers size".into()))?;
    let mut pos = off + n;
    for _ in 0..size {
        pos += encoded_catch_handler_size(data, pos)?;
    }
    Ok(pos - off)
}

fn encoded_catch_handler_size(data: &[u8], off: usize) -> Result<usize> {
    let (size, n) = read_sleb128(data, off).ok_or(DexError::Truncated("catch handler size".into()))?;
    let mut pos = off + n;
    if size <= 0 {
        let (_, n2) = read_uleb128(data, pos).ok_or(DexError::Truncated("catch-all addr".into()))?;
        pos += n2;
    } else {
        for _ in 0..size as u32 {
            let (_, n1) = read_uleb128(data, pos).ok_or(DexError::Truncated("handler type".into()))?;
            pos += n1;
            let (_, n2) = read_uleb128(data, pos).ok_or(DexError::Truncated("handler addr".into()))?;
            pos += n2;
        }
        let (_, n3) = read_uleb128(data, pos).ok_or(DexError::Truncated("catch-all addr".into()))?;
        pos += n3;
    }
    Ok(pos - off)
}

fn adjust_file_offsets(dex: &mut [u8], from: usize, delta: i64) -> Result<()> {
    if delta == 0 {
        return Ok(());
    }
    let header = DexHeader::parse(dex)?;
    let adjust = |off: u32| -> u32 {
        if off == 0 || off == NO_INDEX {
            return off;
        }
        if (off as usize) >= from {
            ((off as i64) + delta) as u32
        } else {
            off
        }
    };

    write_u32(dex, 32, adjust(header.file_size));
    if (header.data_off as usize) <= from {
        write_u32(dex, 104, adjust(header.data_size));
    }
    write_u32(dex, 52, adjust(header.map_off));
    write_u32(dex, 48, adjust(header.link_off));

    for i in 0..header.string_ids_size {
        let pos = header.string_ids_off as usize + i as usize * 4;
        let v = read_u32(dex, pos).ok_or(DexError::Truncated("string_id".into()))?;
        write_u32(dex, pos, adjust(v));
    }

    for i in 0..header.proto_ids_size {
        let pos = header.proto_ids_off as usize + i as usize * 12 + 8;
        let v = read_u32(dex, pos).ok_or(DexError::Truncated("proto parameters_off".into()))?;
        if v != 0 {
            write_u32(dex, pos, adjust(v));
        }
    }

    for i in 0..header.class_defs_size {
        let base = header.class_defs_off as usize + i as usize * 32;
        for rel in [12usize, 20, 24, 28] {
            let v = read_u32(dex, base + rel).ok_or(DexError::Truncated("class_def offset".into()))?;
            if v != 0 && v != NO_INDEX {
                write_u32(dex, base + rel, adjust(v));
            }
        }
        let class_def = ClassDef::parse(dex, &header, i)?;
        if class_def.class_data_off != 0 {
            adjust_class_data_code_offs(dex, class_def.class_data_off, &adjust)?;
        }
    }

    adjust_debug_info_offs(dex, &header, &adjust)?;
    if header.map_off != 0 {
        adjust_map_list(dex, header.map_off, adjust)?;
    }
    Ok(())
}

fn adjust_debug_info_offs(
    dex: &mut [u8],
    header: &DexHeader,
    adjust: &dyn Fn(u32) -> u32,
) -> Result<()> {
    for i in 0..header.class_defs_size {
        let class_def = ClassDef::parse(dex, header, i)?;
        if class_def.class_data_off == 0 {
            continue;
        }
        let class_data = ClassData::parse(dex, &class_def)?;
        let Some(class_data) = class_data else {
            continue;
        };
        for enc in class_data
            .direct_methods
            .iter()
            .chain(&class_data.virtual_methods)
        {
            if enc.code_off == 0 {
                continue;
            }
            let pos = enc.code_off as usize + 8;
            if pos + 4 > dex.len() {
                continue;
            }
            let debug_off =
                read_u32(dex, pos).ok_or(DexError::Truncated("debug_info_off".into()))?;
            if debug_off != 0 {
                write_u32(dex, pos, adjust(debug_off));
            }
        }
    }
    Ok(())
}

fn adjust_class_data_code_offs(
    dex: &mut [u8],
    class_data_off: u32,
    adjust: &dyn Fn(u32) -> u32,
) -> Result<()> {
    let mut off = class_data_off as usize;
    if off >= dex.len() {
        return Err(DexError::Truncated("class_data_off".into()));
    }

    let (static_fields_size, n) =
        read_uleb128(dex, off).ok_or(DexError::Truncated("static_fields_size".into()))?;
    off += n;
    let (instance_fields_size, n) =
        read_uleb128(dex, off).ok_or(DexError::Truncated("instance_fields_size".into()))?;
    off += n;
    let (direct_methods_size, n) =
        read_uleb128(dex, off).ok_or(DexError::Truncated("direct_methods_size".into()))?;
    off += n;
    let (virtual_methods_size, n) =
        read_uleb128(dex, off).ok_or(DexError::Truncated("virtual_methods_size".into()))?;
    off += n;

    off = skip_fields(dex, off, static_fields_size)?;
    off = skip_fields(dex, off, instance_fields_size)?;
    patch_method_code_offs(dex, off, direct_methods_size, &adjust)?;
    patch_method_code_offs(dex, off, virtual_methods_size, &adjust)?;
    Ok(())
}

fn skip_fields(dex: &[u8], mut off: usize, count: u32) -> Result<usize> {
    for _ in 0..count {
        let (_, n1) = read_uleb128(dex, off).ok_or(DexError::Truncated("field_idx".into()))?;
        off += n1;
        let (_, n2) = read_uleb128(dex, off).ok_or(DexError::Truncated("field access".into()))?;
        off += n2;
    }
    Ok(off)
}

fn patch_method_code_offs(
    dex: &mut [u8],
    mut off: usize,
    count: u32,
    adjust: &dyn Fn(u32) -> u32,
) -> Result<usize> {
    for _ in 0..count {
        let (_, n1) = read_uleb128(dex, off).ok_or(DexError::Truncated("method_idx".into()))?;
        off += n1;
        let (_, n2) = read_uleb128(dex, off).ok_or(DexError::Truncated("method access".into()))?;
        off += n2;
        let (code_off, n3) = read_uleb128(dex, off).ok_or(DexError::Truncated("code_off".into()))?;
        if code_off != 0 {
            let new_off = adjust(code_off);
            patch_uleb128_in_place(dex, off, new_off)?;
        }
        off += n3;
    }
    Ok(off)
}

fn patch_uleb128_in_place(dex: &mut [u8], off: usize, value: u32) -> Result<()> {
    let new_bytes = write_uleb128(value);
    let (_, old_len) = read_uleb128(&dex[off..], 0).ok_or(DexError::Truncated("uleb128".into()))?;
    if new_bytes.len() != old_len {
        return Err(DexError::Parse(format!(
            "code_off uleb128 size changed at 0x{off:x} (old {old_len} bytes, new {} bytes); cannot resize class_data yet",
            new_bytes.len()
        )));
    }
    dex[off..off + old_len].copy_from_slice(&new_bytes);
    Ok(())
}

fn adjust_map_list(dex: &mut [u8], map_off: u32, adjust: impl Fn(u32) -> u32) -> Result<()> {
    let off = map_off as usize;
    if off + 4 > dex.len() {
        return Err(DexError::Truncated("map_list".into()));
    }
    let size = read_u32(dex, off).ok_or(DexError::Truncated("map_list size".into()))? as usize;
    for i in 0..size {
        let base = off + 4 + i * 12;
        let item_off = read_u32(dex, base + 8).ok_or(DexError::Truncated("map_item offset".into()))?;
        write_u32(dex, base + 8, adjust(item_off));
    }
    Ok(())
}

/// Recompute DEX header checksum (Adler32) and signature (SHA-1).
pub fn fix_checksums(dex: &mut [u8]) -> Result<()> {
    if dex.len() < 0x70 {
        return Err(DexError::Truncated("dex too short for header".into()));
    }
    let checksum = adler32(&dex[12..]);
    dex[8..12].copy_from_slice(&checksum.to_le_bytes());

    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(&dex[32..]);
    let signature = hasher.finalize();
    dex[12..32].copy_from_slice(&signature);
    Ok(())
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_code_item_dex(insns: &[u8]) -> Vec<u8> {
        let insns_size = (insns.len() / 2) as u32;
        let code_item_len = 16 + insns.len();
        let data_off = 0x70u32;
        let file_size = data_off + code_item_len as u32;
        let mut data = vec![0u8; file_size as usize];
        data[0..4].copy_from_slice(&[0x64, 0x65, 0x78, 0x0a]);
        data[4..8].copy_from_slice(b"035\0");
        data[32..36].copy_from_slice(&file_size.to_le_bytes());
        data[36..40].copy_from_slice(&(0x70u32).to_le_bytes());
        data[40..44].copy_from_slice(&(0x1234_5678u32).to_le_bytes());
        data[104..108].copy_from_slice(&data_off.to_le_bytes());
        data[108..112].copy_from_slice(&(code_item_len as u32).to_le_bytes());
        let code_off = data_off as usize;
        data[code_off + 12..code_off + 16].copy_from_slice(&insns_size.to_le_bytes());
        data[code_off + 16..code_off + 16 + insns.len()].copy_from_slice(insns);
        data
    }

    #[test]
    fn fix_checksums_runs() {
        let mut data = vec![0u8; 0x80];
        data[0..4].copy_from_slice(&[0x64, 0x65, 0x78, 0x0a]);
        data[4..8].copy_from_slice(b"035\0");
        data[32..36].copy_from_slice(&(0x80u32).to_le_bytes());
        data[36..40].copy_from_slice(&(0x70u32).to_le_bytes());
        data[40..44].copy_from_slice(&(0x1234_5678u32).to_le_bytes());
        fix_checksums(&mut data).unwrap();
        assert_ne!(data[8..12], [0, 0, 0, 0]);
    }

    #[test]
    fn replace_code_insns_variable_size() {
        let mut dex = minimal_code_item_dex(&[0x0e, 0x00]);
        let code_off = 0x70;
        replace_code_insns(&mut dex, code_off, &[0x12, 0x00, 0x13, 0x00, 0x00, 0x00])
            .unwrap();
        let code = CodeItem::parse(&dex, code_off).unwrap();
        assert_eq!(code.insns_size, 3);
        assert_eq!(code.insns_slice(&dex), &[0x12, 0x00, 0x13, 0x00, 0x00, 0x00]);
    }
}
