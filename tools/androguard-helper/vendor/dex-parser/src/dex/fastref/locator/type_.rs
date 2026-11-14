//! Type index locators (fuzzy substring + exact binary search).

use std::collections::HashMap;

use crate::dex::raw::RawDex;
use crate::dex::strings::encode_mutf8;
use crate::error::Result;

use super::StringLocator;

pub struct TypeLocator<'a> {
    dex: &'a RawDex<'a>,
    /// string_idx → type indices that use that descriptor.
    str_to_types: HashMap<u32, Vec<u32>>,
}

impl<'a> TypeLocator<'a> {
    pub fn build(dex: &'a RawDex<'a>) -> Result<Self> {
        let mut str_to_types: HashMap<u32, Vec<u32>> = HashMap::new();
        for i in 0..dex.type_ids.size {
            let sidx = dex.type_descriptor_idx(i)?;
            str_to_types.entry(sidx).or_default().push(i);
        }
        Ok(Self { dex, str_to_types })
    }

    /// Fuzzy: any type whose descriptor string contains `query`.
    pub fn locate(&self, strings: &mut StringLocator<'a>, query: &str) -> Vec<u32> {
        let mut out = Vec::new();
        for sidx in strings.locate(query) {
            if let Some(types) = self.str_to_types.get(&sidx) {
                out.extend(types.iter().copied());
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Exact descriptor match via binary search on type_ids (sorted by descriptor).
    pub fn locate_exact(&self, descriptor: &str) -> Option<u32> {
        let want = encode_mutf8(descriptor);
        let n = self.dex.type_ids.size;
        if n == 0 {
            return None;
        }
        let mut lo = 0u32;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let Ok(bytes) = self.dex.string_payload_bytes(
                self.dex.type_descriptor_idx(mid).ok()?,
            ) else {
                return None;
            };
            match bytes.cmp(want.as_slice()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }
}
