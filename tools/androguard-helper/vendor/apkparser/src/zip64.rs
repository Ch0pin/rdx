//! ZIP64 EOCD locator and end-of-central-directory record parsing.

use crate::error::{BrokenAPKError, Result};

const PK_EOCD64_LOCATOR: &[u8; 4] = b"\x50\x4b\x06\x07";
const PK_EOCD64: &[u8; 4] = b"\x50\x4b\x06\x06";

/// ZIP64 EOCD locator + EOCD64: returns `(cd_size, cd_offset)`.
pub fn resolve_zip64_cd(data: &[u8], eocd_pos: usize) -> Result<(u64, u64)> {
    if eocd_pos < 20 {
        return Err(BrokenAPKError("ZIP64: EOCD too early for locator".into()).into());
    }
    let loc_pos = eocd_pos - 20;
    if &data[loc_pos..loc_pos + 4] != PK_EOCD64_LOCATOR {
        return Err(BrokenAPKError("ZIP64: EOCD64 locator not found".into()).into());
    }
    let eocd64_off = u64::from_le_bytes(data[loc_pos + 8..loc_pos + 16].try_into().unwrap());
    let off = eocd64_off as usize;
    if off + 56 > data.len() || &data[off..off + 4] != PK_EOCD64 {
        return Err(BrokenAPKError("ZIP64: EOCD64 record not found".into()).into());
    }
    let cd_size = u64::from_le_bytes(data[off + 40..off + 48].try_into().unwrap());
    let cd_off = u64::from_le_bytes(data[off + 48..off + 56].try_into().unwrap());
    Ok((cd_size, cd_off))
}

/// Parse ZIP64 extra field (header ID 0x0001) for overridden sizes/offsets.
/// On any inconsistency, returns the original 32-bit values unchanged.
pub fn apply_zip64_extra(
    extra: &[u8],
    mut compressed_size: u64,
    mut uncompressed_size: u64,
    mut local_header_offset: u64,
) -> (u64, u64, u64) {
    let need_comp = compressed_size == 0xFFFF_FFFF;
    let need_uncomp = uncompressed_size == 0xFFFF_FFFF;
    let need_off = local_header_offset == 0xFFFF_FFFF;
    if !need_comp && !need_uncomp && !need_off {
        return (compressed_size, uncompressed_size, local_header_offset);
    }

    let mut i = 0;
    while i + 4 <= extra.len() {
        let header_id = u16::from_le_bytes([extra[i], extra[i + 1]]);
        let data_size = u16::from_le_bytes([extra[i + 2], extra[i + 3]]) as usize;
        let data_start = i + 4;
        let data_end = data_start + data_size;
        if data_end > extra.len() {
            break;
        }
        if header_id == 0x0001 {
            let mut p = data_start;
            let take_u64 = |buf: &[u8], p: &mut usize| -> Option<u64> {
                if *p + 8 > buf.len() {
                    return None;
                }
                let v = u64::from_le_bytes(buf[*p..*p + 8].try_into().ok()?);
                *p += 8;
                Some(v)
            };
            // Order per APPNOTE: uncompressed, compressed, local header offset
            // (fields present only when the corresponding 32-bit value is 0xFFFFFFFF).
            if need_uncomp {
                if let Some(v) = take_u64(extra, &mut p) {
                    uncompressed_size = v;
                } else {
                    return (compressed_size, uncompressed_size, local_header_offset);
                }
            }
            if need_comp {
                if let Some(v) = take_u64(extra, &mut p) {
                    compressed_size = v;
                } else {
                    return (compressed_size, uncompressed_size, local_header_offset);
                }
            }
            if need_off {
                if let Some(v) = take_u64(extra, &mut p) {
                    local_header_offset = v;
                }
            }
            break;
        }
        i = data_end;
    }
    (compressed_size, uncompressed_size, local_header_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip64_extra_overrides_sizes() {
        // header 0x0001, size 24: uncomp + comp + offset
        let mut extra = Vec::new();
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.extend_from_slice(&24u16.to_le_bytes());
        extra.extend_from_slice(&0x1_0000_0000u64.to_le_bytes());
        extra.extend_from_slice(&0x2_0000_0000u64.to_le_bytes());
        extra.extend_from_slice(&0x3_0000_0000u64.to_le_bytes());
        let (c, u, o) = apply_zip64_extra(&extra, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF);
        assert_eq!(u, 0x1_0000_0000);
        assert_eq!(c, 0x2_0000_0000);
        assert_eq!(o, 0x3_0000_0000);
    }
}
