//! Discover `classes*.dex` entries and split DEX 041 containers.

use memchr::memmem;

use crate::zip::{CentralEntry, ZipIndex};

const DEX_041_MAGIC: &[u8; 8] = b"dex\n041\0";

/// `classes*.dex` entries at archive root, sorted ascending by `compressed_size`.
pub fn dex_entries<'a>(index: &'a ZipIndex<'a>) -> Vec<&'a CentralEntry> {
    let mut found = scan_cd_for_dex(index);
    if found.is_empty() {
        found = index
            .entries()
            .iter()
            .filter(|e| is_root_classes_dex(&e.filename))
            .collect();
    }
    found.sort_by_key(|e| e.compressed_size);
    found
}

fn is_root_classes_dex(name: &str) -> bool {
    if name.contains('/') {
        return false;
    }
    name == "classes.dex"
        || (name.starts_with("classes") && name.ends_with(".dex") && name.len() > "classes.dex".len())
}

/// ASC-style memmem over the central-directory byte range for `classes`.
fn scan_cd_for_dex<'a>(index: &'a ZipIndex<'a>) -> Vec<&'a CentralEntry> {
    let Some((cd_off, cd_end)) = index.central_directory_range() else {
        return Vec::new();
    };
    let data = index.data();
    if cd_off >= data.len() || cd_end > data.len() || cd_off >= cd_end {
        return Vec::new();
    }
    let cd = &data[cd_off..cd_end];
    let mut names = Vec::new();
    for rel in memmem::find_iter(cd, b"classes") {
        if rel < 46 {
            continue;
        }
        let hdr_rel = rel - 46;
        if &cd[hdr_rel..hdr_rel + 4] != b"PK\x01\x02" {
            continue;
        }
        let name_len = u16::from_le_bytes([cd[hdr_rel + 28], cd[hdr_rel + 29]]) as usize;
        let name_start = hdr_rel + 46;
        let name_end = name_start + name_len;
        if name_end > cd.len() {
            continue;
        }
        let name = match std::str::from_utf8(&cd[name_start..name_end]) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if is_root_classes_dex(name) {
            names.push(name.to_string());
        }
    }
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter_map(|n| index.entry(&n))
        .collect()
}

/// Byte offsets of each logical DEX header inside a container; `[0]` for a normal DEX.
pub fn logical_dex_offsets(data: &[u8]) -> Vec<usize> {
    if data.len() < 8 || &data[0..4] != b"dex\n" {
        return Vec::new();
    }
    if data.len() < 8 || &data[0..8] != DEX_041_MAGIC {
        return vec![0];
    }
    let mut offs = Vec::new();
    let mut off = 0usize;
    while off + 0x24 <= data.len() && &data[off..off + 8] == DEX_041_MAGIC {
        let file_size = u32::from_le_bytes(data[off + 0x20..off + 0x24].try_into().unwrap()) as usize;
        if file_size < 0x70 || off + file_size > data.len() {
            break;
        }
        offs.push(off);
        off += file_size;
    }
    if offs.is_empty() {
        vec![0]
    } else {
        offs
    }
}

/// Yield `(name, bytes)` for each logical DEX, renaming multi-DEX containers like ASC.
pub fn iter_logical_dexes(
    name: &str,
    data: Vec<u8>,
) -> impl Iterator<Item = (String, Vec<u8>)> {
    let offs = logical_dex_offsets(&data);
    if offs.len() <= 1 {
        return vec![(name.to_string(), data)].into_iter();
    }
    let mut out = Vec::with_capacity(offs.len());
    for (i, &hdr_off) in offs.iter().enumerate() {
        let mut copy = data.clone();
        // ASC: copy header to offset 0; leave container-absolute offsets intact.
        let hdr = &data[hdr_off..hdr_off + 0x70];
        copy[..0x70].copy_from_slice(hdr);
        let label = if i == 0 {
            format!("{name}!classes1.dex")
        } else {
            format!("{name}!classes{}.dex", i + 1)
        };
        out.push((label, copy));
    }
    out.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_classes_filter() {
        assert!(is_root_classes_dex("classes.dex"));
        assert!(is_root_classes_dex("classes2.dex"));
        assert!(!is_root_classes_dex("assets/classes.dex"));
        assert!(!is_root_classes_dex("AndroidManifest.xml"));
    }

    #[test]
    fn logical_offsets_plain_dex() {
        let mut d = vec![0u8; 0x70];
        d[0..4].copy_from_slice(b"dex\n");
        d[4..8].copy_from_slice(b"035\0");
        assert_eq!(logical_dex_offsets(&d), vec![0]);
    }
}
