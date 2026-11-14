//! Method / field member locators.

use std::collections::HashMap;

use crate::dex::raw::RawDex;
use crate::error::Result;

use super::{StringLocator, TypeLocator};

#[derive(Debug, Clone, Default)]
pub struct MemberQuery {
    pub class: Option<(String, bool)>, // (name, exact)
    pub name: Option<String>,          // substring
}

pub struct MethodLocator<'a> {
    dex: &'a RawDex<'a>,
    by_class: HashMap<u16, Vec<u32>>,
    by_name: HashMap<u32, Vec<u32>>,
}

impl<'a> MethodLocator<'a> {
    pub fn build(dex: &'a RawDex<'a>) -> Result<Self> {
        let mut by_class: HashMap<u16, Vec<u32>> = HashMap::new();
        let mut by_name: HashMap<u32, Vec<u32>> = HashMap::new();
        let base = dex.method_ids.off as usize;
        for i in 0..dex.method_ids.size {
            let off = base + i as usize * 8;
            let class_idx = dex.u16_at(off)?;
            let name_idx = dex.u32_at(off + 4)?;
            by_class.entry(class_idx).or_default().push(i);
            by_name.entry(name_idx).or_default().push(i);
        }
        Ok(Self {
            dex,
            by_class,
            by_name,
        })
    }

    pub fn locate(
        &self,
        strings: &mut StringLocator<'a>,
        types: &TypeLocator<'a>,
        q: &MemberQuery,
    ) -> Vec<u32> {
        locate_members(self.dex, &self.by_class, &self.by_name, strings, types, q)
    }
}

pub struct FieldLocator<'a> {
    dex: &'a RawDex<'a>,
    by_class: HashMap<u16, Vec<u32>>,
    by_name: HashMap<u32, Vec<u32>>,
}

impl<'a> FieldLocator<'a> {
    pub fn build(dex: &'a RawDex<'a>) -> Result<Self> {
        let mut by_class: HashMap<u16, Vec<u32>> = HashMap::new();
        let mut by_name: HashMap<u32, Vec<u32>> = HashMap::new();
        let base = dex.field_ids.off as usize;
        for i in 0..dex.field_ids.size {
            let off = base + i as usize * 8;
            let class_idx = dex.u16_at(off)?;
            let name_idx = dex.u32_at(off + 4)?;
            by_class.entry(class_idx).or_default().push(i);
            by_name.entry(name_idx).or_default().push(i);
        }
        Ok(Self {
            dex,
            by_class,
            by_name,
        })
    }

    pub fn locate(
        &self,
        strings: &mut StringLocator<'a>,
        types: &TypeLocator<'a>,
        q: &MemberQuery,
    ) -> Vec<u32> {
        locate_members(self.dex, &self.by_class, &self.by_name, strings, types, q)
    }
}

fn locate_members<'a>(
    _dex: &RawDex<'a>,
    by_class: &HashMap<u16, Vec<u32>>,
    by_name: &HashMap<u32, Vec<u32>>,
    strings: &mut StringLocator<'a>,
    types: &TypeLocator<'a>,
    q: &MemberQuery,
) -> Vec<u32> {
    let name_set: Option<Vec<u32>> = q.name.as_ref().map(|n| {
        let mut idxs = Vec::new();
        for sidx in strings.locate(n) {
            if let Some(ms) = by_name.get(&sidx) {
                idxs.extend(ms.iter().copied());
            }
        }
        idxs.sort_unstable();
        idxs.dedup();
        idxs
    });

    let class_set: Option<Vec<u32>> = q.class.as_ref().map(|(c, exact)| {
        let type_idxs = if *exact {
            types.locate_exact(c).into_iter().collect::<Vec<_>>()
        } else {
            types.locate(strings, c)
        };
        let mut idxs = Vec::new();
        for t in type_idxs {
            if let Some(ms) = by_class.get(&(t as u16)) {
                idxs.extend(ms.iter().copied());
            }
        }
        idxs.sort_unstable();
        idxs.dedup();
        idxs
    });

    match (class_set, name_set) {
        (None, None) => Vec::new(),
        (Some(c), None) => c,
        (None, Some(n)) => n,
        (Some(mut c), Some(n)) => {
            c.retain(|x| n.binary_search(x).is_ok());
            c
        }
    }
}
