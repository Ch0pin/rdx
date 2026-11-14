//! ASC-style findrefs: locators + raw code_item scan + insn→method map.

pub mod locator;
pub mod scan;

use std::collections::BTreeMap;

use crate::dex::raw::RawDex;
use crate::error::Result;

use locator::{FieldLocator, InsnLocator, MethodLocator, StringLocator, TypeLocator};
use scan::{code_item_region, scan_kind};

pub use locator::{MemberQuery, Owner};
pub use scan::RefKind;

/// One verified reference site.
#[derive(Debug, Clone)]
pub struct RefSite {
    pub file_off: u32,
    pub pool_idx: u32,
    pub method_idx: u32,
    /// Offset within the method's `insns` array (bytes), for UI navigation.
    pub insn_off: u32,
}

pub struct FastRef<'a> {
    dex: RawDex<'a>,
    insn: Option<InsnLocator>,
}

impl<'a> FastRef<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        Ok(Self {
            dex: RawDex::parse(data)?,
            insn: None,
        })
    }

    pub fn from_raw(dex: RawDex<'a>) -> Self {
        Self { dex, insn: None }
    }

    pub fn raw(&self) -> &RawDex<'a> {
        &self.dex
    }

    fn ensure_insn(&mut self) -> Result<()> {
        if self.insn.is_none() {
            self.insn = Some(InsnLocator::build(&self.dex)?);
        }
        Ok(())
    }

    pub fn find_strings(&mut self, needle: &str) -> Result<Vec<RefSite>> {
        let mut sl = StringLocator::build(&self.dex)?;
        let idxs = sl.locate(needle);
        self.scan_and_resolve(RefKind::String, &idxs, self.dex.string_ids.size)
    }

    pub fn find_types(&mut self, needle: &str) -> Result<Vec<RefSite>> {
        let mut sl = StringLocator::build(&self.dex)?;
        let tl = TypeLocator::build(&self.dex)?;
        let idxs = tl.locate(&mut sl, needle);
        self.scan_and_resolve(RefKind::Type, &idxs, self.dex.type_ids.size)
    }

    pub fn find_methods(&mut self, q: &MemberQuery) -> Result<Vec<RefSite>> {
        let mut sl = StringLocator::build(&self.dex)?;
        let tl = TypeLocator::build(&self.dex)?;
        let ml = MethodLocator::build(&self.dex)?;
        let idxs = ml.locate(&mut sl, &tl, q);
        self.scan_and_resolve(RefKind::Method, &idxs, self.dex.method_ids.size)
    }

    pub fn find_fields(&mut self, q: &MemberQuery) -> Result<Vec<RefSite>> {
        let mut sl = StringLocator::build(&self.dex)?;
        let tl = TypeLocator::build(&self.dex)?;
        let fl = FieldLocator::build(&self.dex)?;
        let idxs = fl.locate(&mut sl, &tl, q);
        self.scan_and_resolve(RefKind::Field, &idxs, self.dex.field_ids.size)
    }

    pub fn find_method_idx(&mut self, method_idx: u32) -> Result<Vec<RefSite>> {
        self.scan_and_resolve(RefKind::Method, &[method_idx], self.dex.method_ids.size)
    }

    /// One code-region scan for all `wanted` pool indices of `kind`.
    pub fn find_pool_batch(&mut self, kind: RefKind, wanted: &[u32]) -> Result<Vec<RefSite>> {
        let pool_size = match kind {
            RefKind::String => self.dex.string_ids.size,
            RefKind::Type => self.dex.type_ids.size,
            RefKind::Field => self.dex.field_ids.size,
            RefKind::Method => self.dex.method_ids.size,
        };
        self.scan_and_resolve(kind, wanted, pool_size)
    }

    fn scan_and_resolve(
        &mut self,
        kind: RefKind,
        wanted: &[u32],
        pool_size: u32,
    ) -> Result<Vec<RefSite>> {
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_insn()?;
        let insn = self.insn.as_mut().unwrap();
        insn.reset_cursors();
        let (start, end) = code_item_region(&self.dex).unwrap_or((
            insn.code_item_start,
            insn.code_item_end.min(self.dex.data.len() as u32),
        ));
        let start = start as usize;
        let end = (end as usize).min(self.dex.data.len()).max(start);
        let region = &self.dex.data[start..end];
        let mut hits = Vec::new();
        scan_kind(kind, region, start as u32, wanted, pool_size, &mut hits);
        hits.sort_by_key(|h| h.file_off);
        let owners = insn.locate(self.dex.data, &hits);
        let mut sites = Vec::new();
        for (hit, owner) in hits.into_iter().zip(owners.into_iter()) {
            let Some(owner) = owner else { continue };
            for &m in owner.methods() {
                let insn_off = insn
                    .method_insns_base(m)
                    .map(|base| hit.file_off.saturating_sub(base))
                    .unwrap_or(0);
                sites.push(RefSite {
                    file_off: hit.file_off,
                    pool_idx: hit.pool_idx,
                    method_idx: m,
                    insn_off,
                });
            }
        }
        Ok(sites)
    }
}

/// Group reference sites by defining method (`method_idx` → distinct `pool_idx` values).
pub fn group_by_method(sites: &[RefSite]) -> BTreeMap<u32, Vec<u32>> {
    let mut out: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for site in sites {
        let entry = out.entry(site.method_idx).or_default();
        if !entry.contains(&site.pool_idx) {
            entry.push(site.pool_idx);
        }
    }
    out
}

/// True if this DEX **defines** `descriptor` (not merely references it).
pub fn dex_defines_class(data: &[u8], descriptor: &str) -> bool {
    let Ok(raw) = RawDex::parse(data) else {
        return false;
    };
    let Ok(types) = TypeLocator::build(&raw) else {
        return false;
    };
    let Some(tidx) = types.locate_exact(descriptor) else {
        return false;
    };
    for i in 0..raw.class_defs.size {
        let off = raw.class_defs.off as usize + i as usize * 0x20;
        if let Ok(class_idx) = raw.u32_at(off) {
            if class_idx == tidx {
                return true;
            }
        }
    }
    false
}
