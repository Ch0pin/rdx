//! ZIP alignment for APK signing (`zipalign -p` semantics).

use super::{central_dir_offset, find_eocd, SignError, SignResult};

const LFH_SIG: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];
const CDH_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
const PAGE_ALIGNMENT: u32 = 4096;
const DEFAULT_ALIGNMENT: u32 = 4;

/// Align uncompressed entries: `.so` → 4096-byte pages, others → 4 bytes.
/// Rebuilds local headers + central directory offsets (does not change compression).
pub fn zipalign(apk: &[u8], _alignment: u32) -> SignResult<Vec<u8>> {
    zipalign_page(apk)
}

pub fn zipalign_page(apk: &[u8]) -> SignResult<Vec<u8>> {
    let cd_off = central_dir_offset(apk)?;
    let eocd = find_eocd(apk).ok_or_else(|| SignError::InvalidApk("EOCD not found".into()))?;
    if eocd < cd_off {
        return Err(SignError::InvalidApk("EOCD before central directory".into()));
    }

    // Parse local file headers in order.
    let mut entries = Vec::new();
    let mut pos = 0usize;
    while pos + 30 <= cd_off {
        if apk[pos..pos + 4] != LFH_SIG {
            break;
        }
        let comp_method = u16::from_le_bytes([apk[pos + 8], apk[pos + 9]]);
        let comp_size = u32::from_le_bytes(apk[pos + 18..pos + 22].try_into().unwrap()) as usize;
        let name_len = u16::from_le_bytes([apk[pos + 26], apk[pos + 27]]) as usize;
        let extra_len = u16::from_le_bytes([apk[pos + 28], apk[pos + 29]]) as usize;
        let header_size = 30 + name_len + extra_len;
        let data_start = pos + header_size;
        let data_end = data_start + comp_size;
        if data_end > cd_off || pos + 30 + name_len > apk.len() {
            break;
        }
        let name = String::from_utf8_lossy(&apk[pos + 30..pos + 30 + name_len]).into_owned();
        let align = entry_alignment(&name, comp_method);
        entries.push(LocalEntry {
            lfh: apk[pos..data_start].to_vec(),
            data: apk[data_start..data_end].to_vec(),
            name,
            align,
            old_extra_len: extra_len,
        });
        pos = data_end;
    }

    // Emit aligned local headers.
    let mut out = Vec::with_capacity(apk.len() + entries.len() * 64);
    let mut new_offsets = Vec::with_capacity(entries.len());
    for entry in &entries {
        let lfh_start = out.len();
        new_offsets.push(lfh_start as u32);

        let name_len = entry.name.len();
        // data starts after: 30 + name + extra(+pad)
        let base_data_start = lfh_start + 30 + name_len + entry.old_extra_len;
        let pad = if entry.align > 1 {
            let m = (base_data_start as u32) % entry.align;
            if m == 0 {
                0
            } else {
                (entry.align - m) as usize
            }
        } else {
            0
        };
        let new_extra = entry.old_extra_len + pad;

        // Copy LFH, patch extra length.
        out.extend_from_slice(&entry.lfh[..28]);
        out.extend_from_slice(&(new_extra as u16).to_le_bytes());
        // name + old extra from original LFH
        out.extend_from_slice(&entry.lfh[30..]);
        if pad > 0 {
            out.extend(std::iter::repeat_n(0u8, pad));
        }
        out.extend_from_slice(&entry.data);
    }

    let new_cd_off = out.len();

    // Rewrite central directory with updated local offsets / extra lengths.
    let mut cd_pos = cd_off;
    let mut entry_idx = 0usize;
    while cd_pos + 46 <= eocd {
        if apk[cd_pos..cd_pos + 4] != CDH_SIG {
            break;
        }
        let name_len = u16::from_le_bytes([apk[cd_pos + 28], apk[cd_pos + 29]]) as usize;
        let extra_len = u16::from_le_bytes([apk[cd_pos + 30], apk[cd_pos + 31]]) as usize;
        let comment_len = u16::from_le_bytes([apk[cd_pos + 32], apk[cd_pos + 33]]) as usize;
        let rec_len = 46 + name_len + extra_len + comment_len;
        if cd_pos + rec_len > eocd {
            break;
        }
        if entry_idx >= entries.len() {
            return Err(SignError::InvalidApk("CD entry count mismatch".into()));
        }
        let entry = &entries[entry_idx];
        let pad = {
            let lfh_start = new_offsets[entry_idx] as usize;
            let base = lfh_start + 30 + entry.name.len() + entry.old_extra_len;
            if entry.align > 1 {
                let m = (base as u32) % entry.align;
                if m == 0 {
                    0
                } else {
                    (entry.align - m) as usize
                }
            } else {
                0
            }
        };
        let new_extra = entry.old_extra_len + pad;

        out.extend_from_slice(&apk[cd_pos..cd_pos + 30]);
        out.extend_from_slice(&(new_extra as u16).to_le_bytes());
        out.extend_from_slice(&apk[cd_pos + 32..cd_pos + 42]);
        out.extend_from_slice(&new_offsets[entry_idx].to_le_bytes());
        // name (from CD)
        out.extend_from_slice(&apk[cd_pos + 46..cd_pos + 46 + name_len]);
        // Match LFH extra: original LFH extra bytes + alignment padding.
        let lfh_extra = &entry.lfh[30 + entry.name.len()..];
        out.extend_from_slice(lfh_extra);
        if pad > 0 {
            out.extend(std::iter::repeat_n(0u8, pad));
        }
        // comment from CD
        out.extend_from_slice(
            &apk[cd_pos + 46 + name_len + extra_len..cd_pos + 46 + name_len + extra_len + comment_len],
        );

        cd_pos += rec_len;
        entry_idx += 1;
    }

    if entry_idx != entries.len() {
        return Err(SignError::InvalidApk("CD/LFH entry count mismatch".into()));
    }

    // EOCD with updated CD size + offset (padding grows the central directory).
    let eocd_start = out.len();
    let cd_size = eocd_start - new_cd_off;
    out.extend_from_slice(&apk[eocd..]);
    if out.len() >= eocd_start + 22 {
        out[eocd_start + 12..eocd_start + 16]
            .copy_from_slice(&(cd_size as u32).to_le_bytes());
        out[eocd_start + 16..eocd_start + 20]
            .copy_from_slice(&(new_cd_off as u32).to_le_bytes());
    }
    Ok(out)
}

struct LocalEntry {
    lfh: Vec<u8>,
    data: Vec<u8>,
    name: String,
    align: u32,
    old_extra_len: usize,
}

fn entry_alignment(name: &str, comp_method: u16) -> u32 {
    if comp_method != 0 {
        return 0; // compressed: no alignment requirement
    }
    if name.starts_with("lib/") && name.ends_with(".so") {
        PAGE_ALIGNMENT
    } else {
        DEFAULT_ALIGNMENT
    }
}
