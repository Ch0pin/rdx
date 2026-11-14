//! LEB128 (Little-Endian Base 128) encoding for DEX.
//! uleb128, sleb128, uleb128p1 per Android DEX format.

/// Read unsigned LEB128; returns (value, bytes consumed).
#[inline]
pub fn read_uleb128(data: &[u8], offset: usize) -> Option<(u32, usize)> {
    let mut result: u32 = 0;
    let mut shift = 0;
    let mut consumed = 0;
    for i in offset..data.len().min(offset + 5) {
        let b = data[i];
        consumed += 1;
        result |= ((b & 0x7f) as u32) << shift;
        if (b & 0x80) == 0 {
            return Some((result, consumed));
        }
        shift += 7;
        if shift >= 35 {
            return None;
        }
    }
    None
}

/// Read signed LEB128; returns (value, bytes consumed).
#[inline]
pub fn read_sleb128(data: &[u8], offset: usize) -> Option<(i32, usize)> {
    let mut result: i32 = 0;
    let mut shift = 0;
    let mut consumed = 0;
    for i in offset..data.len().min(offset + 5) {
        let b = data[i];
        consumed += 1;
        result |= ((b & 0x7f) as i32) << shift;
        shift += 7;
        if (b & 0x80) == 0 {
            if shift < 32 && (b & 0x40) != 0 {
                result |= !0 << shift;
            }
            return Some((result, consumed));
        }
        if shift >= 35 {
            return None;
        }
    }
    None
}

/// uleb128p1: value stored is (encoded - 1). So 0 -> -1, 1 -> 0.
#[inline]
pub fn read_uleb128p1(data: &[u8], offset: usize) -> Option<(i32, usize)> {
    read_uleb128(data, offset).map(|(v, n)| (v.wrapping_sub(1) as i32, n))
}

/// Encode unsigned LEB128.
pub fn write_uleb128(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
    out
}

/// Encode signed LEB128.
pub fn write_sleb128(mut value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value as u32 & 0x7f) as u8;
        value >>= 7;
        let done = (value == 0 && (byte & 0x40) == 0) || (value == -1 && (byte & 0x40) != 0);
        if !done {
            byte |= 0x80;
        }
        out.push(byte);
        if done {
            break;
        }
    }
    out
}

/// Encode uleb128p1: stores `(value + 1)` as uleb128. `-1` → `0`.
pub fn write_uleb128p1(value: i32) -> Vec<u8> {
    write_uleb128((value as u32).wrapping_add(1))
}

/// Write u32 little-endian at offset.
#[inline]
pub fn write_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Read u16 little-endian at offset.
#[inline]
pub fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let end = offset + 2;
    if end > data.len() {
        return None;
    }
    let bytes: [u8; 2] = data[offset..end].try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

/// Read u32 little-endian at offset.
#[inline]
pub fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let end = offset + 4;
    if end > data.len() {
        return None;
    }
    let bytes: [u8; 4] = data[offset..end].try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uleb128_examples() {
        assert_eq!(read_uleb128(&[0x00], 0), Some((0, 1)));
        assert_eq!(read_uleb128(&[0x01], 0), Some((1, 1)));
        assert_eq!(read_uleb128(&[0x7f], 0), Some((127, 1)));
        assert_eq!(read_uleb128(&[0x80, 0x7f], 0), Some((16256, 2)));
    }

    #[test]
    fn read_u16_u32_bounds() {
        assert_eq!(read_u16(&[0x01, 0x00], 0), Some(1));
        assert_eq!(read_u32(&[1, 0, 0, 0], 0), Some(1));
    }
}
