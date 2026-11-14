//! APK Signature Block parsing (finding v2/v3 block in ZIP).

use byteorder::{LittleEndian, ReadBytesExt};
use memchr::memmem;
use std::collections::HashMap;
use std::io::Cursor;

use crate::error::{BrokenAPKError, Result};

const PK_END_OF_CENTRAL_DIR: &[u8; 4] = b"\x50\x4b\x05\x06";
const PK_CENTRAL_DIR: &[u8; 4] = b"\x50\x4b\x01\x02";
const APK_SIG_MAGIC: &[u8; 16] = b"APK Sig Block 42";
/// Max ZIP comment length + EOCD fixed size — EOCD must lie in this suffix (same as zip.rs).
const EOCD_SEARCH_WINDOW: usize = 65_535 + 22;

pub const APK_SIG_KEY_V2_SIGNATURE: u32 = 0x7109871A;
pub const APK_SIG_KEY_V3_SIGNATURE: u32 = 0xF05368C0;

/// Locate EOCD in a trailing window (like `zip.rs`); return `(cd_size, cd_offset)`.
fn find_eocd(data: &[u8]) -> Result<(u32, u32)> {
    let len = data.len();
    if len < 22 {
        return Err(BrokenAPKError("File too short for EOCD".to_string()).into());
    }
    let window_start = len.saturating_sub(EOCD_SEARCH_WINDOW);
    let window = &data[window_start..];

    // 1) Strict: EOCD whose 22 + comment_len reaches exactly EOF.
    for rel in memmem::rfind_iter(window, PK_END_OF_CENTRAL_DIR) {
        let pos = window_start + rel;
        if pos + 22 > len {
            continue;
        }
        let comment_len = u16::from_le_bytes([data[pos + 20], data[pos + 21]]) as usize;
        if pos + 22 + comment_len == len {
            return eocd_cd_fields(data, pos);
        }
    }

    // 2) Lenient: last EOCD signature in the window.
    if let Some(rel) = memmem::rfind(window, PK_END_OF_CENTRAL_DIR) {
        let pos = window_start + rel;
        if pos + 22 <= len {
            return eocd_cd_fields(data, pos);
        }
    }

    Err(BrokenAPKError("EOCD signature not found".to_string()).into())
}

fn eocd_cd_fields(data: &[u8], eocd_pos: usize) -> Result<(u32, u32)> {
    let mut c = Cursor::new(&data[eocd_pos + 4..]);
    let _ = c.read_u16::<LittleEndian>()?;
    let _ = c.read_u16::<LittleEndian>()?;
    let _ = c.read_u16::<LittleEndian>()?;
    let _ = c.read_u16::<LittleEndian>()?;
    let size_central = c.read_u32::<LittleEndian>()?;
    let offset_central = c.read_u32::<LittleEndian>()?;
    Ok((size_central, offset_central))
}

/// Parse the APK Signing Block immediately before the central directory at `cd_offset`.
///
/// Prefer this when you already have [`crate::zip::ZipIndex::central_directory_range`].
pub fn parse_apk_sig_block_at(
    apk: &[u8],
    cd_offset: usize,
) -> Result<(bool, bool, HashMap<u32, Vec<u8>>)> {
    let offset_central = cd_offset;
    if offset_central + 4 > apk.len() {
        return Ok((false, false, HashMap::new()));
    }
    if &apk[offset_central..offset_central + 4] != PK_CENTRAL_DIR {
        return Err(BrokenAPKError("No Central Dir at specified offset".to_string()).into());
    }
    let end_offset = offset_central;
    if end_offset < 24 {
        return Ok((false, false, HashMap::new()));
    }
    let block_start = end_offset - 24;
    let size_in_footer = {
        let mut c = Cursor::new(&apk[block_start..]);
        c.read_u64::<LittleEndian>()?
    };
    let magic = &apk[block_start + 8..block_start + 24];
    if magic != APK_SIG_MAGIC {
        return Ok((false, false, HashMap::new()));
    }
    // size_in_footer can mean either:
    // (a) length of pairs only: block is [size][pairs][size][magic], pairs length = size_in_footer
    // (b) total block length: block is [size][pairs][size][magic], total = size_in_footer (pairs = size_in_footer - 32)
    let end_of_pairs = end_offset - 24;
    let pair_start = {
        let block_begin_total = end_offset.saturating_sub(size_in_footer as usize);
        let block_begin_pairs_only = end_offset.saturating_sub(24 + size_in_footer as usize);
        if block_begin_total + 8 <= apk.len() {
            let size_at_start = {
                let mut c = Cursor::new(&apk[block_begin_total..]);
                c.read_u64::<LittleEndian>()?
            };
            if size_at_start == size_in_footer - 32 {
                // Total block length: block is [pairs][size][magic], first pair starts at block_begin_total
                block_begin_total
            } else if block_begin_pairs_only + 8 <= apk.len() {
                let size_at_start = {
                    let mut c = Cursor::new(&apk[block_begin_pairs_only..]);
                    c.read_u64::<LittleEndian>()?
                };
                if size_at_start == size_in_footer {
                    block_begin_pairs_only + 8
                } else if size_at_start == 0 || size_at_start > size_in_footer as u64 {
                    // No leading size: block is [pairs][size][magic], first pair at block_begin_total
                    block_begin_total
                } else {
                    block_begin_pairs_only
                }
            } else {
                block_begin_total
            }
        } else if block_begin_pairs_only + 8 <= apk.len() {
            let size_at_start = {
                let mut c = Cursor::new(&apk[block_begin_pairs_only..]);
                c.read_u64::<LittleEndian>()?
            };
            if size_at_start == size_in_footer {
                block_begin_pairs_only + 8
            } else if (size_at_start == 0 || size_at_start > size_in_footer as u64)
                && block_begin_total + 8 <= apk.len()
            {
                block_begin_total
            } else {
                block_begin_pairs_only
            }
        } else {
            if block_begin_pairs_only > apk.len() {
                return Err(BrokenAPKError("APK Sig Block extends before file".to_string()).into());
            }
            block_begin_pairs_only
        }
    };
    if pair_start >= end_of_pairs {
        return Ok((false, false, HashMap::new()));
    }
    let mut blocks: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut pos = pair_start;
    while pos + 12 <= end_of_pairs {
        let size = {
            let mut c = Cursor::new(&apk[pos..]);
            c.read_u64::<LittleEndian>()? as usize
        };
        let key = {
            let mut c = Cursor::new(&apk[pos + 8..]);
            c.read_u32::<LittleEndian>()?
        };
        pos += 12;
        if size >= 4 && pos + size - 4 <= end_of_pairs {
            let value = apk[pos..pos + size - 4].to_vec();
            pos += size - 4;
            if !blocks.contains_key(&key) {
                blocks.insert(key, value);
            }
        } else {
            break;
        }
    }
    let is_signed_v2 = blocks.contains_key(&APK_SIG_KEY_V2_SIGNATURE);
    let is_signed_v3 = blocks.contains_key(&APK_SIG_KEY_V3_SIGNATURE);
    Ok((is_signed_v2, is_signed_v3, blocks))
}

/// Locate the central directory via windowed EOCD search, then parse the signing block.
pub fn parse_apk_sig_block(apk: &[u8]) -> Result<(bool, bool, HashMap<u32, Vec<u8>>)> {
    let (_size_central, offset_central) = find_eocd(apk)?;
    parse_apk_sig_block_at(apk, offset_central as usize)
}
