//! Substring → string_ids via contiguous string_data layout.

use std::collections::HashMap;

use memchr::{memchr, memmem};

use crate::dex::raw::RawDex;
use crate::dex::strings::encode_mutf8;
use crate::error::{DexError, Result};
use crate::leb128::read_uleb128;

/// Locate string_ids whose MUTF-8 value contains a query substring.
///
/// # Invariant
/// `string_data_item`s are laid out contiguously in ascending `string_ids` order
/// (true for dx/R8). If violated, falls back to a full scan.
pub struct StringLocator<'a> {
    dex: &'a RawDex<'a>,
    data_off_to_idx: HashMap<u32, u32>,
    strdata_start: u32,
    strdata_end: u32,
    layout_ok: bool,
}

impl<'a> StringLocator<'a> {
    pub fn build(dex: &'a RawDex<'a>) -> Result<Self> {
        let n = dex.string_ids.size;
        let mut data_off_to_idx = HashMap::with_capacity(n as usize + 1);
        let mut max_off = 0u32;
        let mut min_off = u32::MAX;
        for i in 0..n {
            let off = dex.string_data_off(i)?;
            data_off_to_idx.insert(off, i);
            max_off = max_off.max(off);
            min_off = min_off.min(off);
        }
        if n == 0 {
            return Ok(Self {
                dex,
                data_off_to_idx,
                strdata_start: 0,
                strdata_end: 0,
                layout_ok: true,
            });
        }
        let strdata_start = min_off;
        // End = one past NUL of highest-offset item.
        let mut end = max_off as usize;
        let (_sz, nuleb) = read_uleb128(dex.data, end).ok_or(DexError::Truncated("strdata".into()))?;
        end += nuleb;
        while end < dex.data.len() && dex.data[end] != 0 {
            end += 1;
        }
        if end < dex.data.len() {
            end += 1; // past NUL
        }
        data_off_to_idx.insert(end as u32, n); // sentinel
        Ok(Self {
            dex,
            data_off_to_idx,
            strdata_start,
            strdata_end: end as u32,
            layout_ok: true,
        })
    }

    pub fn locate(&mut self, query: &str) -> Vec<u32> {
        if query.is_empty() || self.strdata_start >= self.strdata_end {
            return Vec::new();
        }
        let needle = encode_mutf8(query);
        if !self.layout_ok {
            return self.locate_fallback(&needle);
        }
        let start = self.strdata_start as usize;
        let end = self.strdata_end as usize;
        let region = &self.dex.data[start..end];
        let mut out = Vec::new();
        for rel in memmem::find_iter(region, &needle) {
            let abs_end = start + rel + needle.len();
            let Some(nul_rel) = memchr(0, &self.dex.data[abs_end..end]) else {
                continue;
            };
            let next_item = (abs_end + nul_rel + 1) as u32;
            match self.data_off_to_idx.get(&next_item) {
                Some(&idx) if idx > 0 => out.push(idx - 1),
                _ => {
                    self.layout_ok = false;
                    return self.locate_fallback(&needle);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn locate_fallback(&self, needle: &[u8]) -> Vec<u32> {
        let mut out = Vec::new();
        for i in 0..self.dex.string_ids.size {
            if let Ok(bytes) = self.dex.string_payload_bytes(i) {
                if memmem::find(bytes, needle).is_some() {
                    out.push(i);
                }
            }
        }
        out
    }
}
