//! O(1) instruction file-offset → method_ids via 16-byte buckets.

use std::collections::HashMap;

use crate::dex::class_data::ClassData;
use crate::dex::class_def::ClassDef;
use crate::dex::raw::RawDex;
use crate::error::Result;
use crate::leb128::read_u32;

use super::super::scan::CodeScanHit;

#[derive(Debug, Clone)]
pub enum Owner {
    None,
    One(u32),
    Many(Box<[u32]>),
}

impl Owner {
    fn add(&mut self, m: u32) {
        match self {
            Owner::None => *self = Owner::One(m),
            Owner::One(a) if *a == m => {}
            Owner::One(a) => *self = Owner::Many(Box::from([*a, m])),
            Owner::Many(list) => {
                if !list.contains(&m) {
                    let mut v = list.to_vec();
                    v.push(m);
                    *self = Owner::Many(v.into_boxed_slice());
                }
            }
        }
    }

    pub fn methods(&self) -> &[u32] {
        match self {
            Owner::None => &[],
            Owner::One(m) => std::slice::from_ref(m),
            Owner::Many(m) => m,
        }
    }
}

pub struct InsnLocator {
    buckets: Vec<Owner>,
    base_bucket: u32,
    method_start: HashMap<u32, u32>,
    method_end: HashMap<u32, u32>,
    pub code_item_start: u32,
    pub code_item_end: u32,
    cursor: HashMap<u32, u32>,
}

impl InsnLocator {
    pub fn build(dex: &RawDex) -> Result<Self> {
        let mut owners: HashMap<u32, Owner> = HashMap::new();
        let mut method_start = HashMap::new();
        let mut method_end = HashMap::new();
        let mut min_b = u32::MAX;
        let mut max_b = 0u32;

        walk_encoded_methods(dex, |method_idx, code_off| {
            if code_off == 0 {
                return;
            }
            let aligned = (code_off + 3) & !3;
            let off = aligned as usize;
            if off + 16 > dex.data.len() {
                return;
            }
            let insns_size = u32::from_le_bytes(dex.data[off + 12..off + 16].try_into().unwrap());
            let insn_off = aligned + 16;
            let insn_bytes = insns_size.saturating_mul(2);
            if insn_bytes == 0 {
                return;
            }
            method_start.insert(method_idx, insn_off);
            method_end.insert(method_idx, insn_off + insn_bytes);
            let first = insn_off >> 4;
            let last = (insn_off + insn_bytes - 1) >> 4;
            min_b = min_b.min(first);
            max_b = max_b.max(last);
            for b in first..=last {
                owners.entry(b).or_insert(Owner::None).add(method_idx);
            }
        })?;

        if min_b == u32::MAX {
            return Ok(Self {
                buckets: Vec::new(),
                base_bucket: 0,
                method_start,
                method_end,
                code_item_start: 0,
                code_item_end: 0,
                cursor: HashMap::new(),
            });
        }
        let len = (max_b - min_b + 1) as usize;
        let mut buckets = vec![Owner::None; len];
        for (b, o) in owners {
            buckets[(b - min_b) as usize] = o;
        }
        Ok(Self {
            buckets,
            base_bucket: min_b,
            method_start,
            method_end,
            code_item_start: min_b << 4,
            code_item_end: (max_b + 1) << 4,
            cursor: HashMap::new(),
        })
    }

    pub fn reset_cursors(&mut self) {
        self.cursor.clear();
    }

    /// Byte offset of this method's `insns` array within the DEX file.
    pub fn method_insns_base(&self, method_idx: u32) -> Option<u32> {
        self.method_start.get(&method_idx).copied()
    }

    /// Hits must arrive in ascending `file_off` order.
    pub fn locate(&mut self, data: &[u8], hits: &[CodeScanHit]) -> Vec<Option<Owner>> {
        debug_assert!(hits.windows(2).all(|w| w[0].file_off <= w[1].file_off));
        hits.iter()
            .map(|h| self.resolve(data, h.file_off))
            .collect()
    }

    fn resolve(&mut self, data: &[u8], off: u32) -> Option<Owner> {
        let b = off >> 4;
        if b < self.base_bucket {
            return None;
        }
        let i = (b - self.base_bucket) as usize;
        if i >= self.buckets.len() {
            return None;
        }
        let methods: Vec<u32> = self.buckets[i].methods().to_vec();
        if methods.is_empty() {
            return None;
        }
        let mut ok = Vec::new();
        for m in methods {
            if self.verify_boundary(data, m, off) {
                ok.push(m);
            }
        }
        match ok.len() {
            0 => None,
            1 => Some(Owner::One(ok[0])),
            _ => Some(Owner::Many(ok.into_boxed_slice())),
        }
    }

    fn verify_boundary(&mut self, data: &[u8], method_idx: u32, off: u32) -> bool {
        let Some(&start) = self.method_start.get(&method_idx) else {
            return false;
        };
        let Some(&end) = self.method_end.get(&method_idx) else {
            return false;
        };
        if off < start || off >= end {
            return false;
        }
        let mut pc = *self.cursor.get(&method_idx).unwrap_or(&start);
        if pc > off {
            pc = start;
        }
        while pc < off {
            if pc >= end {
                return false;
            }
            let Some(len) = insn_len_at(data, pc as usize) else {
                return false;
            };
            if len == 0 {
                return false;
            }
            pc += len;
            if pc > end {
                return false;
            }
        }
        self.cursor.insert(method_idx, pc);
        pc == off
    }
}

fn walk_encoded_methods(dex: &RawDex, mut f: impl FnMut(u32, u32)) -> Result<()> {
    for i in 0..dex.class_defs.size {
        let cdef_off = dex.class_defs.off as usize + i as usize * 0x20;
        let class_def = ClassDef {
            class_idx: dex.u32_at(cdef_off).unwrap_or(0),
            access_flags: 0,
            superclass_idx: 0,
            interfaces_off: 0,
            source_file_idx: 0,
            annotations_off: 0,
            class_data_off: dex.u32_at(cdef_off + 24).unwrap_or(0),
            static_values_off: 0,
        };
        let Some(cd) = ClassData::parse(dex.data, &class_def)? else {
            continue;
        };
        for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
            f(m.method_idx, m.code_off);
        }
    }
    Ok(())
}

/// Length in **bytes** of the Dalvik instruction or payload at `off`.
pub(crate) fn insn_len_at(data: &[u8], off: usize) -> Option<u32> {
    if off + 2 > data.len() {
        return None;
    }
    let unit0 = u16::from_le_bytes([data[off], data[off + 1]]);
    match unit0 {
        0x0100 => {
            // packed-switch-payload: size*2+4 units
            if off + 4 > data.len() {
                return None;
            }
            let size = u16::from_le_bytes([data[off + 2], data[off + 3]]) as u32;
            Some((size * 2 + 4) * 2)
        }
        0x0200 => {
            if off + 4 > data.len() {
                return None;
            }
            let size = u16::from_le_bytes([data[off + 2], data[off + 3]]) as u32;
            Some((size * 4 + 2) * 2)
        }
        0x0300 => {
            if off + 8 > data.len() {
                return None;
            }
            let elem_width = u16::from_le_bytes([data[off + 2], data[off + 3]]) as u32;
            let size = u32::from_le_bytes(data[off + 4..off + 8].try_into().ok()?);
            let data_units = (size * elem_width + 1) / 2;
            Some((data_units + 4) * 2)
        }
        _ => {
            let op = data[off];
            let entry = dex_bytecode::get_opcode_entry(op);
            let len = dex_bytecode::format_length(entry.format);
            if len == 0 {
                Some(2)
            } else {
                Some(len)
            }
        }
    }
}

#[allow(dead_code)]
fn _read_u32(data: &[u8], off: usize) -> Option<u32> {
    read_u32(data, off)
}
