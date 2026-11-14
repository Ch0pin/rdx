//! Raw-byte `(opcode, idx)` scanner over the code_item region.

use memchr::memchr_iter;

#[derive(Debug, Clone, Copy)]
pub struct CodeScanHit {
    pub file_off: u32,
    pub pool_idx: u32,
}

#[derive(Clone)]
enum IndexSet {
    Small([u32; 8], u8),
    Bitset(Vec<u64>),
    Hash(std::collections::HashSet<u32>),
}

impl IndexSet {
    fn new(indices: &[u32], pool_size: u32) -> Self {
        if indices.len() <= 8 {
            let mut a = [0u32; 8];
            for (i, &v) in indices.iter().enumerate() {
                a[i] = v;
            }
            return Self::Small(a, indices.len() as u8);
        }
        if pool_size > 0 && indices.len() as u32 * 32 > pool_size {
            let words = ((pool_size as usize) + 63) / 64;
            let mut bits = vec![0u64; words];
            for &i in indices {
                if i < pool_size {
                    bits[i as usize / 64] |= 1u64 << (i % 64);
                }
            }
            return Self::Bitset(bits);
        }
        Self::Hash(indices.iter().copied().collect())
    }

    fn contains(&self, idx: u32) -> bool {
        match self {
            Self::Small(a, n) => a[..*n as usize].iter().any(|&x| x == idx),
            Self::Bitset(b) => {
                let w = idx as usize / 64;
                w < b.len() && (b[w] & (1u64 << (idx % 64))) != 0
            }
            Self::Hash(h) => h.contains(&idx),
        }
    }
}

pub const STRING_OPS_U16: &[u8] = &[0x1a];
pub const STRING_OPS_U32: &[u8] = &[0x1b];
pub const TYPE_OPS: &[u8] = &[0x1c, 0x1f, 0x20, 0x22, 0x23, 0x24, 0x25];
pub const FIELD_OPS_LO: u8 = 0x52;
pub const FIELD_OPS_HI: u8 = 0x6d;
pub const METHOD_OPS: &[u8] = &[
    0x6e, 0x6f, 0x70, 0x71, 0x72, 0x74, 0x75, 0x76, 0x77, 0x78, 0xfa, 0xfb,
];

fn opcode_mask(ops: &[u8]) -> [bool; 256] {
    let mut m = [false; 256];
    for &op in ops {
        m[op as usize] = true;
    }
    m
}

pub fn scan_u16(
    region: &[u8],
    region_base: u32,
    opcode_mask: &[bool; 256],
    wanted: &[u32],
    pool_size: u32,
    out: &mut Vec<CodeScanHit>,
) {
    let set = IndexSet::new(wanted, pool_size);
    let n = region.len();
    let mut p = 0;
    while p + 4 <= n {
        if opcode_mask[region[p] as usize] {
            let idx = u16::from_le_bytes([region[p + 2], region[p + 3]]) as u32;
            if set.contains(idx) {
                out.push(CodeScanHit {
                    file_off: region_base + p as u32,
                    pool_idx: idx,
                });
            }
        }
        p += 1;
    }
}

pub fn scan_u32(
    region: &[u8],
    region_base: u32,
    opcode: u8,
    wanted: &[u32],
    pool_size: u32,
    out: &mut Vec<CodeScanHit>,
) {
    let set = IndexSet::new(wanted, pool_size);
    let n = region.len();
    let mut p = 0;
    while p + 6 <= n {
        if region[p] == opcode {
            let idx = u32::from_le_bytes([
                region[p + 2],
                region[p + 3],
                region[p + 4],
                region[p + 5],
            ]);
            if set.contains(idx) {
                out.push(CodeScanHit {
                    file_off: region_base + p as u32,
                    pool_idx: idx,
                });
            }
        }
        p += 1;
    }
}

/// Single-opcode fast path via memchr (const-string 0x1a).
pub fn scan_single_u16(
    region: &[u8],
    region_base: u32,
    opcode: u8,
    wanted: &[u32],
    pool_size: u32,
    out: &mut Vec<CodeScanHit>,
) {
    let set = IndexSet::new(wanted, pool_size);
    for p in memchr_iter(opcode, region) {
        if p + 4 > region.len() {
            continue;
        }
        let idx = u16::from_le_bytes([region[p + 2], region[p + 3]]) as u32;
        if set.contains(idx) {
            out.push(CodeScanHit {
                file_off: region_base + p as u32,
                pool_idx: idx,
            });
        }
    }
}

pub fn scan_kind(
    kind: RefKind,
    region: &[u8],
    region_base: u32,
    wanted: &[u32],
    pool_size: u32,
    out: &mut Vec<CodeScanHit>,
) {
    match kind {
        RefKind::String => {
            scan_single_u16(region, region_base, 0x1a, wanted, pool_size, out);
            scan_u32(region, region_base, 0x1b, wanted, pool_size, out);
        }
        RefKind::Type => {
            scan_u16(region, region_base, &opcode_mask(TYPE_OPS), wanted, pool_size, out);
        }
        RefKind::Field => {
            let mut m = [false; 256];
            for op in FIELD_OPS_LO..=FIELD_OPS_HI {
                m[op as usize] = true;
            }
            scan_u16(region, region_base, &m, wanted, pool_size, out);
        }
        RefKind::Method => {
            scan_u16(region, region_base, &opcode_mask(METHOD_OPS), wanted, pool_size, out);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    String,
    Type,
    Field,
    Method,
}

/// Prefer map_list TYPE_CODE_ITEM (0x2001) for the contiguous code region.
pub fn code_item_region(dex: &crate::dex::raw::RawDex) -> Option<(u32, u32)> {
    dex.map_entry(0x2001).map(|(size, off)| {
        // size is item count; we only use off as start. End from next map or file.
        let _ = size;
        (off, dex.data.len() as u32)
    })
}
