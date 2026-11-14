//! Zero-copy DEX header / table views over `&[u8]`.

use crate::error::{DexError, Result};
use crate::leb128::read_u32;

use super::class_data::ClassData;
use super::class_def::ClassDef;
use super::code_item::CodeItem;
use super::DexFile;

/// Method with executable code (`code_off != 0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawMethod {
    pub class_idx: u32,
    pub method_idx: u32,
    pub access: u32,
    pub code_off: u32,
}

/// Table view: `(offset, size)` of id items.
#[derive(Debug, Clone, Copy)]
pub struct TableView {
    pub off: u32,
    pub size: u32,
}

/// Header offsets for fastref without constructing a full [`DexFile`].
#[derive(Debug, Clone)]
pub struct RawDex<'a> {
    pub data: &'a [u8],
    pub string_ids: TableView,
    pub type_ids: TableView,
    pub proto_ids: TableView,
    pub field_ids: TableView,
    pub method_ids: TableView,
    pub class_defs: TableView,
    pub map_off: u32,
}

impl<'a> RawDex<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < 0x70 || &data[0..4] != b"dex\n" {
            return Err(DexError::InvalidMagic);
        }
        let map_off = read_u32(data, 0x34).ok_or(DexError::Truncated("map_off".into()))?;
        let string_ids = table(data, 0x38, 0x3c)?;
        let type_ids = table(data, 0x40, 0x44)?;
        let proto_ids = table(data, 0x48, 0x4c)?;
        let field_ids = table(data, 0x50, 0x54)?;
        let method_ids = table(data, 0x58, 0x5c)?;
        let class_defs = table(data, 0x60, 0x64)?;
        Ok(Self {
            data,
            string_ids,
            type_ids,
            proto_ids,
            field_ids,
            method_ids,
            class_defs,
            map_off,
        })
    }

    pub fn from_dex(dex: &'a DexFile) -> Self {
        Self {
            data: &*dex.data,
            string_ids: TableView {
                off: dex.header.string_ids_off,
                size: dex.header.string_ids_size,
            },
            type_ids: TableView {
                off: dex.header.type_ids_off,
                size: dex.header.type_ids_size,
            },
            proto_ids: TableView {
                off: dex.header.proto_ids_off,
                size: dex.header.proto_ids_size,
            },
            field_ids: TableView {
                off: dex.header.field_ids_off,
                size: dex.header.field_ids_size,
            },
            method_ids: TableView {
                off: dex.header.method_ids_off,
                size: dex.header.method_ids_size,
            },
            class_defs: TableView {
                off: dex.header.class_defs_off,
                size: dex.header.class_defs_size,
            },
            map_off: dex.header.map_off,
        }
    }

    #[inline]
    pub fn u32_at(&self, off: usize) -> Result<u32> {
        read_u32(self.data, off).ok_or_else(|| DexError::Truncated(format!("u32@{off}")))
    }

    #[inline]
    pub fn u16_at(&self, off: usize) -> Result<u16> {
        if off + 2 > self.data.len() {
            return Err(DexError::Truncated(format!("u16@{off}")));
        }
        Ok(u16::from_le_bytes([self.data[off], self.data[off + 1]]))
    }

    /// Absolute offset of `string_ids[i]`'s `string_data_item`.
    pub fn string_data_off(&self, idx: u32) -> Result<u32> {
        if idx >= self.string_ids.size {
            return Err(DexError::Truncated("string index".into()));
        }
        self.u32_at(self.string_ids.off as usize + idx as usize * 4)
    }

    /// Descriptor string_idx for `type_ids[i]`.
    pub fn type_descriptor_idx(&self, idx: u32) -> Result<u32> {
        if idx >= self.type_ids.size {
            return Err(DexError::Truncated("type index".into()));
        }
        self.u32_at(self.type_ids.off as usize + idx as usize * 4)
    }

    /// Read MUTF-8 payload bytes (excluding uleb128 size and trailing NUL) for a string_id.
    pub fn string_payload_bytes(&self, idx: u32) -> Result<&'a [u8]> {
        let offset = self.string_data_off(idx)? as usize;
        let (_utf16, n) = crate::leb128::read_uleb128(self.data, offset)
            .ok_or(DexError::Truncated("utf16_size".into()))?;
        let start = offset + n;
        let mut end = start;
        while end < self.data.len() && self.data[end] != 0 {
            end += 1;
        }
        Ok(&self.data[start..end])
    }

    /// Parse `class_def_item` at index `idx`.
    pub fn class_def_at(&self, idx: u32) -> Result<ClassDef> {
        if idx >= self.class_defs.size {
            return Err(DexError::Truncated("class_def index".into()));
        }
        let off = self.class_defs.off as usize + idx as usize * 32;
        if self.data.len() < off + 32 {
            return Err(DexError::Truncated("class_def_item".into()));
        }
        Ok(ClassDef {
            class_idx: read_u32(self.data, off).ok_or(DexError::Truncated("class_def class_idx".into()))?,
            access_flags: read_u32(self.data, off + 4).ok_or(DexError::Truncated("class_def access_flags".into()))?,
            superclass_idx: read_u32(self.data, off + 8).ok_or(DexError::Truncated("class_def superclass_idx".into()))?,
            interfaces_off: read_u32(self.data, off + 12).ok_or(DexError::Truncated("class_def interfaces_off".into()))?,
            source_file_idx: read_u32(self.data, off + 16).ok_or(DexError::Truncated("class_def source_file_idx".into()))?,
            annotations_off: read_u32(self.data, off + 20).ok_or(DexError::Truncated("class_def annotations_off".into()))?,
            class_data_off: read_u32(self.data, off + 24).ok_or(DexError::Truncated("class_def class_data_off".into()))?,
            static_values_off: read_u32(self.data, off + 28).ok_or(DexError::Truncated("class_def static_values_off".into()))?,
        })
    }

    /// Parse `code_item` at `code_off`.
    pub fn code_item_at(&self, code_off: u32) -> Result<CodeItem> {
        CodeItem::parse(self.data, code_off)
    }

    /// Iterate encoded methods (direct + virtual) that have a `code_item`.
    pub fn methods_with_code(&self) -> MethodsWithCodeIter<'_> {
        MethodsWithCodeIter {
            dex: self,
            class_i: 0,
            pending: None,
        }
    }

    /// Look up map_list entry of `type_` (e.g. `0x2001` CODE_ITEM). Returns `(size, offset)`.
    pub fn map_entry(&self, type_: u16) -> Option<(u32, u32)> {
        if self.map_off == 0 {
            return None;
        }
        let off = self.map_off as usize;
        if off + 4 > self.data.len() {
            return None;
        }
        let size = u32::from_le_bytes(self.data[off..off + 4].try_into().ok()?) as usize;
        let mut p = off + 4;
        for _ in 0..size {
            if p + 12 > self.data.len() {
                return None;
            }
            let t = u16::from_le_bytes([self.data[p], self.data[p + 1]]);
            let count = u32::from_le_bytes(self.data[p + 4..p + 8].try_into().ok()?);
            let offset = u32::from_le_bytes(self.data[p + 8..p + 12].try_into().ok()?);
            if t == type_ {
                return Some((count, offset));
            }
            p += 12;
        }
        None
    }
}

/// Iterator over [`RawMethod`] entries with non-zero `code_off`.
pub struct MethodsWithCodeIter<'a> {
    dex: &'a RawDex<'a>,
    class_i: u32,
    pending: Option<(u32, std::vec::IntoIter<super::class_data::EncodedMethod>)>,
}

impl<'a> Iterator for MethodsWithCodeIter<'a> {
    type Item = Result<RawMethod>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((class_idx, methods)) = &mut self.pending {
                let class_idx = *class_idx;
                while let Some(m) = methods.next() {
                    if m.code_off != 0 {
                        return Some(Ok(RawMethod {
                            class_idx,
                            method_idx: m.method_idx,
                            access: m.access_flags,
                            code_off: m.code_off,
                        }));
                    }
                }
                self.pending = None;
            }

            if self.class_i >= self.dex.class_defs.size {
                return None;
            }
            let table_idx = self.class_i;
            self.class_i += 1;
            let class_def = match self.dex.class_def_at(table_idx) {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };
            let class_idx = class_def.class_idx;
            let data = match ClassData::parse(self.dex.data, &class_def) {
                Ok(Some(d)) => d,
                Ok(None) => continue,
                Err(e) => return Some(Err(e)),
            };
            let mut combined = data.direct_methods;
            combined.extend(data.virtual_methods);
            if combined.is_empty() {
                continue;
            }
            self.pending = Some((class_idx, combined.into_iter()));
        }
    }
}

fn table(data: &[u8], size_off: usize, off_off: usize) -> Result<TableView> {
    Ok(TableView {
        size: read_u32(data, size_off).ok_or(DexError::Truncated("table size".into()))?,
        off: read_u32(data, off_off).ok_or(DexError::Truncated("table off".into()))?,
    })
}
