//! Lenient ZIP/APK archive reading (mirrors apkparser.zip).
//!
//! Unlike the strict `zip` crate / Python `zipfile`, we **do not validate** Extra Field
//! TLV records (except ZIP64 extra ID `0x0001` when sizes are `0xFFFFFFFF`). Malware
//! (e.g. Octo2) often fills the Extra Field with junk that makes declared Header ID /
//! size inconsistent with the Extra Field length, while Android still installs the APK.
//! See: https://hatching.io/blog/triage-insights-ep4/

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::ops::Range;

use byteorder::{LittleEndian, ReadBytesExt};
use flate2::read::DeflateDecoder;
use memchr::memmem;

use crate::error::{BrokenAPKError, Error, Result};
use crate::zip64::{apply_zip64_extra, resolve_zip64_cd};

const PK_EOCD: &[u8; 4] = b"\x50\x4b\x05\x06";
const PK_CENTRAL: &[u8; 4] = b"\x50\x4b\x01\x02";
const PK_LOCAL: &[u8; 4] = b"\x50\x4b\x03\x04";

/// Max ZIP comment length + EOCD fixed size — EOCD must lie in this suffix.
const EOCD_SEARCH_WINDOW: usize = 65_535 + 22;

/// Central-directory entry metadata (sizes may be widened by ZIP64).
#[derive(Debug, Clone)]
pub struct CentralEntry {
    pub compression_method: u16,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub local_header_offset: u64,
    pub filename: String,
    /// CRC-32 from the central directory.
    pub crc32: u32,
    /// General-purpose bit flags.
    pub flags: u16,
    /// Version made by.
    pub made_by: u16,
    /// External file attributes.
    pub external_attrs: u32,
    /// Byte range of the raw filename in the APK buffer (central directory).
    pub name_raw: Range<usize>,
    /// Index of this entry in the central directory (0-based order of appearance).
    pub cd_index: usize,
}

/// Caps for [`ZipIndex::read_into`].
#[derive(Debug, Clone, Copy)]
pub struct ReadLimits {
    /// Maximum number of uncompressed bytes to write into `out`.
    pub max_out: usize,
}

/// Which decompression path produced the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodUsed {
    /// Declared/resolved method was stored (0).
    Stored,
    /// Declared/resolved method was deflate (8) and inflate succeeded.
    Deflate,
    /// Fell back to treating payload as stored (magic prefer, inflate failure, etc.).
    StoredFallback,
    /// Fell back to deflate despite a non-8 declared method.
    DeflateFallback,
}

/// Differences between the local file header and the central-directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocalMismatch {
    pub method: bool,
    pub sizes: bool,
    pub name: bool,
    pub crc: bool,
}

impl LocalMismatch {
    pub fn any(self) -> bool {
        self.method || self.sizes || self.name || self.crc
    }
}

/// Result metadata from [`ZipIndex::read_into`].
#[derive(Debug, Clone)]
pub struct ReadOutcome {
    pub method_used: MethodUsed,
    pub crc_ok: bool,
    pub truncated: bool,
    pub local_mismatch: Option<LocalMismatch>,
}

/// Central-directory metadata only. No entry bytes are inflated until [`ZipIndex::read`].
pub struct ZipIndex<'a> {
    data: &'a [u8],
    entries: Vec<CentralEntry>,
    by_name: HashMap<String, usize>,
    cd_off: usize,
    cd_end: usize,
}

impl<'a> ZipIndex<'a> {
    /// Parse the central directory; do not extract entry contents.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let (cd_size, cd_off) = find_eocd(data)?;
        let cd_off = cd_off as usize;
        let cd_end = cd_off.saturating_add(cd_size as usize).min(data.len());
        let entries = parse_central_directory(data, cd_off)?;
        let mut by_name = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            by_name.insert(e.filename.clone(), i);
        }
        Ok(Self {
            data,
            entries,
            by_name,
            cd_off,
            cd_end,
        })
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn entries(&self) -> &[CentralEntry] {
        &self.entries
    }

    /// `(cd_off, cd_end)` byte range of the central directory in `data`.
    pub fn central_directory_range(&self) -> Option<(usize, usize)> {
        if self.cd_off < self.cd_end {
            Some((self.cd_off, self.cd_end))
        } else {
            None
        }
    }

    pub fn namelist(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.filename.as_str())
    }

    pub fn entry(&self, name: &str) -> Option<&CentralEntry> {
        self.by_name.get(name).map(|&i| &self.entries[i])
    }

    /// Duplicate entry names with all central-directory indices (Master Key / CVE-2013-4787).
    ///
    /// `entries` remains complete; [`Self::entry`] / `by_name` stay last-wins for lookup.
    pub fn duplicates(&self) -> impl Iterator<Item = (&str, Vec<usize>)> + '_ {
        let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, e) in self.entries.iter().enumerate() {
            groups.entry(e.filename.as_str()).or_default().push(i);
        }
        groups.into_iter().filter(|(_, idxs)| idxs.len() > 1)
    }

    /// Extract one entry using the existing lenient/tampered-APK logic.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let e = self
            .entry(name)
            .ok_or_else(|| Error::FileNotPresent(name.to_string()))?;
        self.read_entry(e)
    }

    pub fn read_entry(&self, e: &CentralEntry) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let _ = self.read_into(e, &mut out, ReadLimits { max_out: usize::MAX })?;
        Ok(out)
    }

    /// Inflate (or store) an entry into `out`, capped by `limits.max_out`.
    ///
    /// Clears `out` before writing. Verifies CRC-32 against the central-directory value.
    pub fn read_into(
        &self,
        e: &CentralEntry,
        out: &mut Vec<u8>,
        limits: ReadLimits,
    ) -> Result<ReadOutcome> {
        read_into_impl(self.data, e, out, limits)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// Drop the borrow of `data`, keeping CD metadata.
    pub fn into_catalog(self) -> (Vec<CentralEntry>, HashMap<String, usize>, usize, usize) {
        (self.entries, self.by_name, self.cd_off, self.cd_end)
    }
}

/// Owned ZIP: central directory only. Entry bytes are inflated in [`ZipArchive::read`].
pub struct ZipArchive {
    data: Vec<u8>,
    entries: Vec<CentralEntry>,
    by_name: HashMap<String, usize>,
    cd_off: usize,
    cd_end: usize,
}

impl ZipArchive {
    /// Parse the central directory; do not extract entry contents.
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let (entries, by_name, cd_off, cd_end) = ZipIndex::parse(&data)?.into_catalog();
        Ok(Self {
            data,
            entries,
            by_name,
            cd_off,
            cd_end,
        })
    }

    pub fn parse_slice(apk: &[u8]) -> Result<Self> {
        Self::parse(apk.to_vec())
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn entries(&self) -> &[CentralEntry] {
        &self.entries
    }

    pub fn namelist(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.filename.as_str())
    }

    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.filename.clone()).collect()
    }

    pub fn entry(&self, name: &str) -> Option<&CentralEntry> {
        self.by_name.get(name).map(|&i| &self.entries[i])
    }

    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let e = self
            .entry(name)
            .ok_or_else(|| Error::FileNotPresent(name.to_string()))?;
        let mut out = Vec::new();
        let _ = read_into_impl(&self.data, e, &mut out, ReadLimits { max_out: usize::MAX })?;
        Ok(out)
    }

    pub fn read_to_vec(&self, name: &str) -> Result<Vec<u8>> {
        self.read(name)
    }

    pub fn uncompressed_size(&self, name: &str) -> Option<u64> {
        self.entry(name).map(|e| e.uncompressed_size)
    }

    pub fn as_index(&self) -> ZipIndex<'_> {
        ZipIndex {
            data: &self.data,
            entries: self.entries.clone(),
            by_name: self.by_name.clone(),
            cd_off: self.cd_off,
            cd_end: self.cd_end,
        }
    }
}

/// ZIP archive wrapper: list names and read file contents (eager materialisation).
pub struct ZipEntry {
    names: Vec<String>,
    contents: HashMap<String, Vec<u8>>,
}

impl ZipEntry {
    /// Parse APK bytes and build file list + contents (like Python ZipEntry.parse).
    /// Implemented as [`ZipIndex::parse`] followed by extracting every entry.
    pub fn parse(apk: &[u8]) -> Result<Self> {
        let index = ZipIndex::parse(apk)?;
        let mut names = Vec::with_capacity(index.entries.len());
        let mut contents = HashMap::with_capacity(index.entries.len());
        for entry in &index.entries {
            let data = index.read_entry(entry)?;
            names.push(entry.filename.clone());
            contents.insert(entry.filename.clone(), data);
        }
        Ok(Self { names, contents })
    }

    pub fn namelist(&self) -> &[String] {
        &self.names
    }

    pub fn read(&self, name: &str) -> Result<&[u8]> {
        self.contents
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| Error::FileNotPresent(name.to_string()))
    }

    pub fn read_to_vec(&self, name: &str) -> Result<Vec<u8>> {
        self.contents
            .get(name)
            .cloned()
            .ok_or_else(|| Error::FileNotPresent(name.to_string()))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.contents.contains_key(name)
    }
}

/// Locate EOCD; return `(cd_size, cd_offset)` (possibly resolved via ZIP64).
fn find_eocd(data: &[u8]) -> Result<(u64, u64)> {
    let len = data.len();
    if len < 22 {
        return Err(BrokenAPKError("File too short for EOCD".into()).into());
    }
    let window_start = len.saturating_sub(EOCD_SEARCH_WINDOW);
    let window = &data[window_start..];

    // 1) Strict: EOCD whose 22 + comment_len reaches exactly EOF.
    for rel in memmem::rfind_iter(window, PK_EOCD) {
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
    if let Some(rel) = memmem::rfind(window, PK_EOCD) {
        let pos = window_start + rel;
        if pos + 22 <= len {
            return eocd_cd_fields(data, pos);
        }
    }

    Err(BrokenAPKError("EOCD signature not found".into()).into())
}

fn eocd_cd_fields(data: &[u8], eocd_pos: usize) -> Result<(u64, u64)> {
    let entries_this = u16::from_le_bytes([data[eocd_pos + 8], data[eocd_pos + 9]]);
    let entries_total = u16::from_le_bytes([data[eocd_pos + 10], data[eocd_pos + 11]]);
    let size_central = u32::from_le_bytes(data[eocd_pos + 12..eocd_pos + 16].try_into().unwrap());
    let offset_central =
        u32::from_le_bytes(data[eocd_pos + 16..eocd_pos + 20].try_into().unwrap());

    if size_central == 0xFFFF_FFFF
        || offset_central == 0xFFFF_FFFF
        || entries_this == 0xFFFF
        || entries_total == 0xFFFF
    {
        return resolve_zip64_cd(data, eocd_pos);
    }
    Ok((size_central as u64, offset_central as u64))
}

fn parse_central_directory(apk: &[u8], offset_central: usize) -> Result<Vec<CentralEntry>> {
    let mut offset = offset_central;
    if offset + 4 > apk.len() || &apk[offset..offset + 4] != PK_CENTRAL {
        return Err(BrokenAPKError("No Central Dir at specified offset".into()).into());
    }
    let mut entries = Vec::new();
    while offset + 46 <= apk.len() {
        if &apk[offset..offset + 4] != PK_CENTRAL {
            break;
        }
        let mut c = Cursor::new(&apk[offset + 4..]);
        let made_by = c.read_u16::<LittleEndian>()?;
        let _version_needed = c.read_u16::<LittleEndian>()?;
        let flags = c.read_u16::<LittleEndian>()?;
        let compression_method = c.read_u16::<LittleEndian>()?;
        let _mtime = c.read_u16::<LittleEndian>()?;
        let _mdate = c.read_u16::<LittleEndian>()?;
        let crc32 = c.read_u32::<LittleEndian>()?;
        let compressed_size32 = c.read_u32::<LittleEndian>()?;
        let uncompressed_size32 = c.read_u32::<LittleEndian>()?;
        let file_name_length = c.read_u16::<LittleEndian>()? as usize;
        let extra_field_length = c.read_u16::<LittleEndian>()? as usize;
        let file_comment_length = c.read_u16::<LittleEndian>()? as usize;
        let _disk = c.read_u16::<LittleEndian>()?;
        let _internal_attr = c.read_u16::<LittleEndian>()?;
        let external_attrs = c.read_u32::<LittleEndian>()?;
        let local_header_offset32 = c.read_u32::<LittleEndian>()?;

        let name_start = offset + 46;
        let name_end = name_start + file_name_length;
        if name_end > apk.len() {
            break;
        }
        let name_raw = name_start..name_end;
        let filename = String::from_utf8_lossy(&apk[name_start..name_end]).into_owned();
        let extra_end = name_end + extra_field_length;
        let next = extra_end + file_comment_length;
        if next > apk.len() {
            break;
        }
        let extra = &apk[name_end..extra_end];
        let (compressed_size, uncompressed_size, local_header_offset) = apply_zip64_extra(
            extra,
            compressed_size32 as u64,
            uncompressed_size32 as u64,
            local_header_offset32 as u64,
        );
        let cd_index = entries.len();
        entries.push(CentralEntry {
            compression_method,
            compressed_size,
            uncompressed_size,
            local_header_offset,
            filename,
            crc32,
            flags,
            made_by,
            external_attrs,
            name_raw,
            cd_index,
        });
        offset = next;
    }
    Ok(entries)
}

fn looks_like_android_payload(buf: &[u8]) -> bool {
    buf.starts_with(&[0x03, 0x00, 0x08, 0x00])
        || buf.starts_with(&[0x00, 0x00, 0x08, 0x00])
        || buf.starts_with(b"dex\n")
        || buf.starts_with(b"dey\n")
}

fn copy_capped(src: &[u8], out: &mut Vec<u8>, max_out: usize) -> bool {
    let take = src.len().min(max_out);
    out.extend_from_slice(&src[..take]);
    take < src.len()
}

fn inflate_capped(compressed: &[u8], out: &mut Vec<u8>, max_out: usize) -> Result<bool> {
    let mut decoder = DeflateDecoder::new(compressed);
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        if out.len() >= max_out {
            truncated = true;
            out.truncate(max_out);
            break;
        }
        let room = (max_out - out.len()).min(buf.len());
        let n = match decoder.read(&mut buf[..room]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(Error::Io(e)),
        };
        out.extend_from_slice(&buf[..n]);
    }
    Ok(truncated)
}

fn try_inflate_capped(compressed: &[u8], out: &mut Vec<u8>, max_out: usize) -> Option<bool> {
    out.clear();
    match inflate_capped(compressed, out, max_out) {
        Ok(truncated) if !out.is_empty() || compressed.is_empty() => Some(truncated),
        _ => {
            out.clear();
            None
        }
    }
}

fn inflate_or_stored_into(
    compressed: &[u8],
    stored_fallback: &[u8],
    uncompressed_hint: usize,
    out: &mut Vec<u8>,
    max_out: usize,
    prefer_deflate_fallback: bool,
) -> Result<(MethodUsed, bool)> {
    out.clear();
    if compressed.is_empty() {
        let truncated = copy_capped(stored_fallback, out, max_out);
        return Ok((MethodUsed::StoredFallback, truncated));
    }

    if looks_like_android_payload(stored_fallback) {
        if stored_fallback.len() >= uncompressed_hint && uncompressed_hint > 0 {
            let slice = &stored_fallback[..uncompressed_hint];
            let truncated = copy_capped(slice, out, max_out);
            return Ok((MethodUsed::StoredFallback, truncated));
        }
        if looks_like_android_payload(compressed) {
            let src = if stored_fallback.len() > compressed.len() {
                stored_fallback
            } else {
                compressed
            };
            let truncated = copy_capped(src, out, max_out);
            return Ok((MethodUsed::StoredFallback, truncated));
        }
        let truncated = copy_capped(stored_fallback, out, max_out);
        return Ok((MethodUsed::StoredFallback, truncated));
    }

    if let Some(truncated) = try_inflate_capped(compressed, out, max_out) {
        let method = if prefer_deflate_fallback {
            MethodUsed::DeflateFallback
        } else {
            MethodUsed::Deflate
        };
        return Ok((method, truncated));
    }

    out.clear();
    let truncated = copy_capped(stored_fallback, out, max_out);
    Ok((MethodUsed::StoredFallback, truncated))
}

fn read_into_impl(
    apk: &[u8],
    entry: &CentralEntry,
    out: &mut Vec<u8>,
    limits: ReadLimits,
) -> Result<ReadOutcome> {
    out.clear();
    let off = entry.local_header_offset as usize;
    if off + 30 > apk.len() || &apk[off..off + 4] != PK_LOCAL {
        return Err(Error::Parse(format!(
            "local header missing for {}",
            entry.filename
        )));
    }
    let mut c = Cursor::new(&apk[off + 4..]);
    let _version_needed = c.read_u16::<LittleEndian>()?;
    let _local_flags = c.read_u16::<LittleEndian>()?;
    let local_method = c.read_u16::<LittleEndian>()?;
    let _mtime = c.read_u16::<LittleEndian>()?;
    let _mdate = c.read_u16::<LittleEndian>()?;
    let local_crc = c.read_u32::<LittleEndian>()?;
    let mut compressed_size = c.read_u32::<LittleEndian>()? as u64;
    let mut uncompressed_size = c.read_u32::<LittleEndian>()? as u64;
    let file_name_length = c.read_u16::<LittleEndian>()? as usize;
    let extra_field_length = c.read_u16::<LittleEndian>()? as usize;

    let local_comp_raw = compressed_size;
    let local_uncomp_raw = uncompressed_size;

    // Prefer central directory sizes when local sizes are zero (data descriptor / evasion).
    if compressed_size == 0 || uncompressed_size == 0 {
        compressed_size = entry.compressed_size;
        uncompressed_size = entry.uncompressed_size;
    } else if compressed_size == 0xFFFF_FFFF || uncompressed_size == 0xFFFF_FFFF {
        compressed_size = entry.compressed_size;
        uncompressed_size = entry.uncompressed_size;
    }

    let name_start = off + 30;
    let name_end = name_start + file_name_length;
    if name_end > apk.len() {
        return Err(Error::Parse(format!(
            "truncated local entry for {}",
            entry.filename
        )));
    }
    let local_name = &apk[name_start..name_end];
    let central_name = if entry.name_raw.end <= apk.len() {
        &apk[entry.name_raw.clone()]
    } else {
        entry.filename.as_bytes()
    };

    let mismatch = LocalMismatch {
        method: local_method != entry.compression_method,
        sizes: local_comp_raw != entry.compressed_size || local_uncomp_raw != entry.uncompressed_size,
        name: local_name != central_name,
        crc: local_crc != entry.crc32,
    };

    let data_start = name_end + extra_field_length;
    if data_start > apk.len() {
        return Err(Error::Parse(format!(
            "truncated local entry for {}",
            entry.filename
        )));
    }

    let method = match local_method {
        0 | 8 => local_method,
        _ => match entry.compression_method {
            0 | 8 => entry.compression_method,
            _ => local_method,
        },
    };

    let end_c = (data_start + compressed_size as usize).min(apk.len());
    let end_u = (data_start + uncompressed_size as usize).min(apk.len());
    let compressed = &apk[data_start..end_c];
    let stored_span = &apk[data_start..end_u.max(end_c)];
    let max_out = limits.max_out;
    let uncompressed_hint = uncompressed_size as usize;

    let (method_used, truncated) = match method {
        0 => {
            let truncated = copy_capped(stored_span, out, max_out);
            (MethodUsed::Stored, truncated)
        }
        8 => inflate_or_stored_into(
            compressed,
            stored_span,
            uncompressed_hint,
            out,
            max_out,
            false,
        )?,
        _ if compressed_size == uncompressed_size => {
            let truncated = copy_capped(stored_span, out, max_out);
            (MethodUsed::StoredFallback, truncated)
        }
        _ => inflate_or_stored_into(
            compressed,
            stored_span,
            uncompressed_hint,
            out,
            max_out,
            true,
        )?,
    };

    let crc_ok = !truncated && crc32fast::hash(out) == entry.crc32;
    Ok(ReadOutcome {
        method_used,
        crc_ok,
        truncated,
        local_mismatch: if mismatch.any() {
            Some(mismatch)
        } else {
            None
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_stored_zip() -> Vec<u8> {
        let crc = crc32fast::hash(b"hi");
        let mut z = Vec::new();
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&[20, 0]);
        z.extend_from_slice(&[0, 0]);
        z.extend_from_slice(&[0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&[2, 0, 0, 0]);
        z.extend_from_slice(&[2, 0, 0, 0]);
        z.extend_from_slice(&[5, 0]);
        z.extend_from_slice(&[0, 0]);
        z.extend_from_slice(b"a.txt");
        z.extend_from_slice(b"hi");
        let cd_off = z.len();
        z.extend_from_slice(b"PK\x01\x02");
        z.extend_from_slice(&[20, 0, 20, 0]);
        z.extend_from_slice(&[0, 0]);
        z.extend_from_slice(&[0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&[2, 0, 0, 0, 2, 0, 0, 0]);
        z.extend_from_slice(&[5, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(b"a.txt");
        let cd_size = z.len() - cd_off;
        z.extend_from_slice(b"PK\x05\x06");
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(&[1, 0, 1, 0]);
        z.extend_from_slice(&(cd_size as u32).to_le_bytes());
        z.extend_from_slice(&(cd_off as u32).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        z
    }

    /// Two central entries with the same name (Master Key style).
    fn duplicate_name_zip() -> Vec<u8> {
        let mut z = Vec::new();
        // local 0: a.txt = "aa"
        let local0 = z.len();
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let crc_aa = crc32fast::hash(b"aa");
        z.extend_from_slice(&crc_aa.to_le_bytes());
        z.extend_from_slice(&[2, 0, 0, 0, 2, 0, 0, 0]);
        z.extend_from_slice(&[5, 0, 0, 0]);
        z.extend_from_slice(b"a.txt");
        z.extend_from_slice(b"aa");
        // local 1: a.txt = "bb"
        let local1 = z.len();
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let crc_bb = crc32fast::hash(b"bb");
        z.extend_from_slice(&crc_bb.to_le_bytes());
        z.extend_from_slice(&[2, 0, 0, 0, 2, 0, 0, 0]);
        z.extend_from_slice(&[5, 0, 0, 0]);
        z.extend_from_slice(b"a.txt");
        z.extend_from_slice(b"bb");
        let cd_off = z.len();
        for (local, crc) in [(local0, crc_aa), (local1, crc_bb)] {
            z.extend_from_slice(b"PK\x01\x02");
            z.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            z.extend_from_slice(&crc.to_le_bytes());
            z.extend_from_slice(&[2, 0, 0, 0, 2, 0, 0, 0]);
            z.extend_from_slice(&[5, 0, 0, 0, 0, 0]);
            z.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            z.extend_from_slice(&(local as u32).to_le_bytes());
            z.extend_from_slice(b"a.txt");
        }
        let cd_size = z.len() - cd_off;
        z.extend_from_slice(b"PK\x05\x06");
        z.extend_from_slice(&[0, 0, 0, 0, 2, 0, 2, 0]);
        z.extend_from_slice(&(cd_size as u32).to_le_bytes());
        z.extend_from_slice(&(cd_off as u32).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        z
    }

    #[test]
    fn parse_minimal_stored_zip() {
        let z = minimal_stored_zip();
        let zip = ZipEntry::parse(&z).unwrap();
        assert_eq!(zip.namelist(), &["a.txt".to_string()]);
        assert_eq!(zip.read("a.txt").unwrap(), b"hi");
    }

    #[test]
    fn zip_archive_does_not_require_eager_extract() {
        let z = minimal_stored_zip();
        let archive = ZipArchive::parse(z.clone()).unwrap();
        assert!(archive.contains("a.txt"));
        assert_eq!(archive.read("a.txt").unwrap(), b"hi");
        assert_eq!(archive.names(), vec!["a.txt".to_string()]);
    }

    #[test]
    fn zip_index_lazy_matches_eager() {
        let z = minimal_stored_zip();
        let eager = ZipEntry::parse(&z).unwrap();
        let index = ZipIndex::parse(&z).unwrap();
        let names: Vec<_> = index.namelist().map(|s| s.to_string()).collect();
        assert_eq!(names, eager.namelist());
        assert_eq!(index.read("a.txt").unwrap(), eager.read("a.txt").unwrap());
    }

    #[test]
    fn central_entry_exposes_cd_metadata() {
        let z = minimal_stored_zip();
        let index = ZipIndex::parse(&z).unwrap();
        let e = index.entry("a.txt").unwrap();
        assert_eq!(e.crc32, crc32fast::hash(b"hi"));
        assert_eq!(e.flags, 0);
        assert_eq!(e.made_by, 20);
        assert_eq!(e.external_attrs, 0);
        assert_eq!(e.cd_index, 0);
        assert_eq!(&z[e.name_raw.clone()], b"a.txt");
    }

    #[test]
    fn duplicates_reports_all_indices() {
        let z = duplicate_name_zip();
        let index = ZipIndex::parse(&z).unwrap();
        assert_eq!(index.entries().len(), 2);
        // last-wins lookup
        assert_eq!(index.read("a.txt").unwrap(), b"bb");
        let dups: Vec<_> = index.duplicates().collect();
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].0, "a.txt");
        assert_eq!(dups[0].1, vec![0, 1]);
    }

    #[test]
    fn read_into_respects_max_out_and_crc() {
        let z = minimal_stored_zip();
        let index = ZipIndex::parse(&z).unwrap();
        let e = index.entry("a.txt").unwrap();
        let mut out = Vec::new();
        let outcome = index
            .read_into(e, &mut out, ReadLimits { max_out: 1 })
            .unwrap();
        assert_eq!(out, b"h");
        assert!(outcome.truncated);
        assert!(!outcome.crc_ok);
        assert_eq!(outcome.method_used, MethodUsed::Stored);

        out.clear();
        let outcome = index
            .read_into(e, &mut out, ReadLimits { max_out: 64 })
            .unwrap();
        assert_eq!(out, b"hi");
        assert!(!outcome.truncated);
        assert!(outcome.crc_ok);
        assert!(outcome.local_mismatch.is_none());
    }

    #[test]
    fn read_into_reports_local_mismatch() {
        let mut z = minimal_stored_zip();
        // Flip local compression method (offset 8 in local header) to deflate (8).
        z[8] = 8;
        z[9] = 0;
        let index = ZipIndex::parse(&z).unwrap();
        let e = index.entry("a.txt").unwrap();
        let mut out = Vec::new();
        let outcome = index
            .read_into(e, &mut out, ReadLimits { max_out: 64 })
            .unwrap();
        let mm = outcome.local_mismatch.expect("expected mismatch");
        assert!(mm.method);
        assert!(!mm.sizes);
        assert!(!mm.name);
        assert!(!mm.crc);
    }

    #[test]
    fn find_eocd_with_comment() {
        let mut z = minimal_stored_zip();
        // Replace trailing comment length 0 with a comment.
        let comment = b"hello-comment";
        z.pop();
        z.pop();
        z.extend_from_slice(&(comment.len() as u16).to_le_bytes());
        z.extend_from_slice(comment);
        let (cd_size, cd_off) = find_eocd(&z).unwrap();
        assert!(cd_size > 0);
        assert!(cd_off > 0);
        ZipIndex::parse(&z).unwrap();
    }

    #[test]
    fn find_eocd_truncated_errors() {
        assert!(find_eocd(&[0u8; 10]).is_err());
    }

    #[test]
    fn skip_malformed_extra_field_tlv() {
        let mut z = Vec::new();
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&[0x49, 0x86, 0x0b, 0x34]);
        z.extend_from_slice(&[2, 0, 0, 0, 2, 0, 0, 0]);
        z.extend_from_slice(&[5, 0]);
        z.extend_from_slice(&[8, 0]);
        z.extend_from_slice(b"a.txt");
        z.extend_from_slice(b"..m.\x00\x00\x00\x00");
        z.extend_from_slice(b"hi");
        let cd_off = z.len();
        z.extend_from_slice(b"PK\x01\x02");
        z.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&[0x49, 0x86, 0x0b, 0x34]);
        z.extend_from_slice(&[2, 0, 0, 0, 2, 0, 0, 0]);
        z.extend_from_slice(&[5, 0, 8, 0, 0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(b"a.txt");
        z.extend_from_slice(b"..m.\x00\x00\x00\x00");
        let cd_size = z.len() - cd_off;
        z.extend_from_slice(b"PK\x05\x06");
        z.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend_from_slice(&(cd_size as u32).to_le_bytes());
        z.extend_from_slice(&(cd_off as u32).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        let zip = ZipEntry::parse(&z).unwrap();
        assert_eq!(zip.read("a.txt").unwrap(), b"hi");
    }

    #[test]
    fn fake_eocd_in_payload_prefers_strict() {
        // Put PK\x05\x06 inside file data, then a real EOCD at the end.
        let mut z = Vec::new();
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0]);
        let payload = b"xxxxPK\x05\x06yyyy";
        z.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        z.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        z.extend_from_slice(&[5, 0, 0, 0]);
        z.extend_from_slice(b"a.txt");
        z.extend_from_slice(payload);
        let cd_off = z.len();
        z.extend_from_slice(b"PK\x01\x02");
        z.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        z.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        z.extend_from_slice(&[5, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&[0, 0, 0, 0]);
        z.extend_from_slice(b"a.txt");
        let cd_size = z.len() - cd_off;
        z.extend_from_slice(b"PK\x05\x06");
        z.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend_from_slice(&(cd_size as u32).to_le_bytes());
        z.extend_from_slice(&(cd_off as u32).to_le_bytes());
        z.extend_from_slice(&[0, 0]);
        let (_sz, off) = find_eocd(&z).unwrap();
        assert_eq!(off as usize, cd_off);
    }
}
