use byteorder::{LittleEndian, ReadBytesExt};
use std::io::Cursor;

use crate::error::Result;

/// Read a little-endian u32 and advance the cursor.
pub fn read_uint32_le<R: std::io::Read>(cursor: &mut R) -> Result<u32> {
    cursor
        .read_u32::<LittleEndian>()
        .map_err(|e| crate::error::Error::Io(e))
}

/// Parse digests/signatures block: length-prefixed sequence of (algorithm_id, length-prefixed digest).
pub fn parse_signatures_or_digests(digest_bytes: &[u8]) -> Result<Vec<(u32, Vec<u8>)>> {
    if digest_bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut cursor = Cursor::new(digest_bytes);
    let data_len = read_uint32_le(&mut cursor)? as u64;
    let mut digests = Vec::new();
    while cursor.position() < data_len {
        let algorithm_id = read_uint32_le(&mut cursor)?;
        let digest_len = read_uint32_le(&mut cursor)? as usize;
        let start = cursor.position() as usize;
        if start + digest_len > digest_bytes.len() {
            break;
        }
        let digest = digest_bytes[start..start + digest_len].to_vec();
        cursor.set_position((start + digest_len) as u64);
        digests.push((algorithm_id, digest));
    }
    Ok(digests)
}
