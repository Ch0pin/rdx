//! Bounded native DEX metadata reader, following JADX 1.5.6's DEX input pipeline.
//!
//! Porting references (tag v1.5.6, commit 28ff15e4ae69950aebea110a13e5ab895d234dfc):
//! `jadx-plugins/jadx-dex-input/src/main/java/jadx/plugins/input/dex/`
//! `sections/{DexHeader,SectionReader,DexClassData}.java` and
//! `utils/{Leb128,MUtf8}.java` in https://github.com/skylot/jadx.
//! Header/table resolution, delta-coded members and MUTF-8 decoding are adapted
//! from those Apache-2.0-licensed algorithms. This Rust implementation adds
//! bounds, allocation budgets and strict decoding, including encoded static values,
//! ordered catch-handler metadata, and shared annotation directories. Method Throws
//! declarations are also retained in declaration-ready form. Debug programs remain
//! opaque and unvalidated beyond offset checks.
//! Instructions and operand symbols are retained for the native disassembler.
//! No checksum/signature verification is performed (they are not trust signals).
//!
//! Attribution and modification details: `third_party/jadx/README.md`.
//! The upstream Apache license and notices are preserved in
//! `third_party/jadx/LICENSE` and `third_party/jadx/NOTICE`.

use anyhow::{Context, Result, bail, ensure};
use std::sync::Arc;

#[path = "native_dex_metadata.rs"]
mod metadata;

const MAX_ITEMS: usize = 1_000_000;
const MAX_BYTES: usize = 768 * 1024 * 1024;
const NO_INDEX: u32 = u32::MAX;

/// A native parser resource/capability limit, distinct from malformed DEX bytes.
/// A caller may select a different engine for this error specifically.
#[derive(Debug)]
pub struct UnsupportedDex(pub String);

impl std::fmt::Display for UnsupportedDex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UnsupportedDex {}

#[derive(Debug)]
pub struct DexFile {
    pub classes: Vec<DexClass>,
}

#[derive(Debug, Default)]
pub struct DexSymbols {
    pub hierarchy: std::sync::OnceLock<Arc<crate::native_hierarchy::TypeHierarchy>>,
    pub strings: Vec<String>,
    pub types: Vec<Arc<str>>,
    pub protos: Vec<(Arc<str>, Vec<Arc<str>>)>,
    pub fields: Vec<(u16, u16, u32)>,
    pub methods: Vec<(u16, u16, u32)>,
    /// Decoded annotation directories, shared by all classes in this DEX.
    pub annotations: std::collections::BTreeMap<u32, Arc<DexAnnotationDirectory>>,
}

pub type DexAnnotationSet = Arc<[Arc<DexAnnotation>]>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DexAnnotation {
    /// DEX visibility: 0 build, 1 runtime, 2 system.
    pub visibility: u8,
    pub type_idx: u32,
    pub elements: Vec<(u32, DexValue)>,
}

#[derive(Debug, Default)]
pub struct DexAnnotationDirectory {
    pub class: Option<DexAnnotationSet>,
    pub fields: Vec<Option<DexAnnotationSet>>,
    pub methods: Vec<Option<DexAnnotationSet>>,
    pub parameters: Vec<Vec<Option<DexAnnotationSet>>>,
}

#[derive(Debug)]
pub struct DexClass {
    pub symbols: Arc<DexSymbols>,
    pub descriptor: Arc<str>,
    pub superclass: Option<Arc<str>>,
    pub interfaces: Vec<Arc<str>>,
    pub access_flags: u32,
    pub annotations_offset: u32,
    pub static_values_offset: u32,
    /// Explicit encoded prefix, in static-field declaration order. Omitted values default.
    pub static_values: Vec<DexValue>,
    pub fields: Vec<DexField>,
    pub methods: Vec<DexMethod>,
}

/// Encoded values retain exact primitive bits and symbol indexes. Composite or
/// reflective values can remain DEX even when Java emission does not support them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DexValue {
    Byte(i8),
    Short(i16),
    Char(u16),
    Int(i32),
    Long(i64),
    Float(u32),
    Double(u64),
    Boolean(bool),
    Null,
    String(u32),
    Type(u32),
    MethodType(u32),
    MethodHandle(u32),
    Field(u32),
    Method(u32),
    Enum(u32),
    Array(Vec<DexValue>),
    Annotation {
        type_idx: u32,
        elements: Vec<(u32, DexValue)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DexTryRegion {
    pub start: u32,
    /// Exclusive code-unit offset.
    pub end: u32,
    /// Ordered typed handlers, followed by optional catch-all (None).
    pub catches: Arc<[(Option<Arc<str>>, u32)]>,
}

#[derive(Debug)]
pub struct DexField {
    pub declaring_type: Arc<str>,
    pub name: Arc<str>,
    pub field_type: Arc<str>,
    pub access_flags: u32,
    pub is_static: bool,
}

#[derive(Debug)]
pub struct DexMethod {
    pub declaring_type: Arc<str>,
    pub name: Arc<str>,
    pub return_type: Arc<str>,
    pub parameters: Vec<Arc<str>>,
    /// Declared exception types from the system Throws annotation, in declared order.
    pub thrown_types: Vec<Arc<str>>,
    pub access_flags: u32,
    pub code: Option<DexCode>,
}

#[derive(Debug)]
pub struct DexCode {
    pub registers: u16,
    pub ins: u16,
    pub outs: u16,
    pub tries: u16,
    pub try_regions: Vec<DexTryRegion>,
    pub instructions: Vec<u16>,
    pub offset: u32,
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn range(&self, offset: usize, len: usize) -> Result<&[u8]> {
        let end = offset.checked_add(len).context("DEX offset overflow")?;
        self.bytes
            .get(offset..end)
            .context("DEX range exceeds file")
    }
    fn u16(&self, offset: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(self.range(offset, 2)?.try_into()?))
    }
    fn u32(&self, offset: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.range(offset, 4)?.try_into()?))
    }
    fn byte(&self, offset: &mut usize) -> Result<u8> {
        let value = self.range(*offset, 1)?[0];
        *offset += 1;
        Ok(value)
    }
    fn uleb(&self, offset: &mut usize) -> Result<u32> {
        let mut value = 0;
        for i in 0..5 {
            let byte = self.byte(offset)?;
            ensure!(i != 4 || byte <= 15, "DEX ULEB128 overflow");
            value |= u32::from(byte & 127) << (i * 7);
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
        bail!("unterminated DEX ULEB128")
    }
    fn table(&self, header: usize, width: usize) -> Result<(usize, usize)> {
        let count = self.u32(header)? as usize;
        let offset = self.u32(header + 4)? as usize;
        ensure!(count <= MAX_ITEMS, "DEX table exceeds item budget");
        ensure!((count == 0) == (offset == 0), "invalid empty DEX table");
        ensure!(offset.is_multiple_of(4), "unaligned DEX table");
        ensure!(count == 0 || offset >= 112, "DEX table overlaps header");
        self.range(offset, count * width)?;
        Ok((count, offset))
    }
    fn data_offset(&self, offset: u32, data_start: usize) -> Result<usize> {
        let offset = offset as usize;
        ensure!(
            offset >= data_start && offset < self.bytes.len(),
            "invalid DEX data offset"
        );
        Ok(offset)
    }
    fn string(&self, offset: usize, budget: &mut Budget) -> Result<String> {
        let mut cursor = offset;
        let count = self.uleb(&mut cursor)? as usize;
        ensure!(
            count <= MAX_ITEMS && count <= self.bytes.len() - cursor,
            "DEX string exceeds budget or file"
        );
        budget.take(count * 10 + 32)?;
        let mut units = Vec::with_capacity(count);
        loop {
            let a = self.byte(&mut cursor)?;
            if a == 0 {
                break;
            }
            ensure!(units.len() < count, "DEX MUTF-8 length mismatch");
            let unit = match a {
                1..=127 => u16::from(a),
                0xc0..=0xdf => {
                    let b = self.byte(&mut cursor)?;
                    ensure!(b & 0xc0 == 0x80, "invalid DEX MUTF-8 continuation");
                    let value = (u16::from(a & 31) << 6) | u16::from(b & 63);
                    ensure!(value == 0 || value >= 128, "overlong DEX MUTF-8");
                    value
                }
                0xe0..=0xef => {
                    let b = self.byte(&mut cursor)?;
                    let c = self.byte(&mut cursor)?;
                    ensure!(
                        b & 0xc0 == 0x80 && c & 0xc0 == 0x80,
                        "invalid DEX MUTF-8 continuation"
                    );
                    let value =
                        (u16::from(a & 15) << 12) | (u16::from(b & 63) << 6) | u16::from(c & 63);
                    ensure!(value >= 2048, "overlong DEX MUTF-8");
                    value
                }
                _ => bail!("invalid DEX MUTF-8 leading byte"),
            };
            units.push(unit);
        }
        ensure!(units.len() == count, "DEX MUTF-8 length mismatch");
        // MUTF-8 may contain lone UTF-16 surrogates. Preserve them as explicit
        // display escapes, and escape literal backslashes to avoid collisions.
        // This is reversible text for inspection, not a Java String value.
        let mut text = String::new();
        for unit in char::decode_utf16(units) {
            match unit {
                Ok('\\') => text.push_str("\\\\"),
                Ok(ch) => text.push(ch),
                Err(error) => text.push_str(&format!("\\u{{{:04x}}}", error.unpaired_surrogate())),
            }
        }
        Ok(text)
    }
}

struct Budget(usize, std::collections::HashMap<String, Arc<str>>);
fn budget(bytes: usize) -> Budget {
    Budget(bytes, Default::default())
}
impl Budget {
    fn take(&mut self, bytes: usize) -> Result<()> {
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or_else(|| UnsupportedDex("native DEX allocation budget exceeded".into()))?;
        Ok(())
    }
    fn string(&mut self, value: &str) -> Result<Arc<str>> {
        if let Some(value) = self.1.get(value) {
            return Ok(Arc::clone(value));
        }
        self.take(value.len() * 2 + 96)?;
        let shared: Arc<str> = Arc::from(value);
        self.1.insert(value.to_owned(), Arc::clone(&shared));
        Ok(shared)
    }
}

fn at<T>(values: &[T], index: u32) -> Result<&T> {
    values
        .get(index as usize)
        .context("DEX table index out of range")
}

fn type_list(
    reader: &Reader<'_>,
    offset: u32,
    data_start: usize,
    types: &[Arc<str>],
    budget: &mut Budget,
) -> Result<Vec<Arc<str>>> {
    if offset == 0 {
        return Ok(Vec::new());
    }
    let offset = reader.data_offset(offset, data_start)?;
    ensure!(offset % 4 == 0, "unaligned DEX type list");
    let count = reader.u32(offset)? as usize;
    ensure!(count <= MAX_ITEMS, "DEX type list exceeds budget");
    reader.range(offset + 4, count * 2)?;
    (0..count)
        .map(|i| budget.string(at(types, u32::from(reader.u16(offset + 4 + i * 2)?))?))
        .collect()
}

fn code(
    reader: &Reader<'_>,
    offset: u32,
    data_start: usize,
    types: &[Arc<str>],
    budget: &mut Budget,
) -> Result<Option<DexCode>> {
    if offset == 0 {
        return Ok(None);
    }
    let start = reader.data_offset(offset, data_start)?;
    ensure!(start % 4 == 0, "unaligned DEX code item");
    reader.range(start, 16)?;
    let registers = reader.u16(start)?;
    let ins = reader.u16(start + 2)?;
    ensure!(
        ins <= registers,
        "DEX input registers exceed register count"
    );
    let tries = reader.u16(start + 6)?;
    let debug_offset = reader.u32(start + 8)?;
    if debug_offset != 0 {
        reader.data_offset(debug_offset, data_start)?;
    }
    let size = reader.u32(start + 12)? as usize;
    ensure!(size <= MAX_BYTES / 2, "DEX code exceeds budget");
    reader.range(start + 16, size * 2)?;
    let try_start = start + 16 + size * 2 + (size % 2) * 2;
    if tries != 0 && !size.is_multiple_of(2) {
        ensure!(
            reader.u16(start + 16 + size * 2)? == 0,
            "Nonzero DEX try padding"
        );
    }
    let try_regions = metadata::try_regions(reader, try_start, tries, size, types, budget)?;
    budget.take(size * 2 + 32)?;
    let instructions = (0..size)
        .map(|i| reader.u16(start + 16 + i * 2))
        .collect::<Result<_>>()?;
    Ok(Some(DexCode {
        registers,
        ins,
        outs: reader.u16(start + 4)?,
        tries,
        try_regions,
        instructions,
        offset,
    }))
}

/// Parse standard little-endian DEX 035/037/038/039/040 metadata without a JVM.
/// Unsupported formats and malformed or budget-exceeding inputs return errors.
pub fn parse(bytes: &[u8]) -> Result<DexFile> {
    let mut remaining = MAX_BYTES;
    parse_with_budget(bytes, &mut remaining)
}

/// Parse with a shared conservative allocation budget, suitable for APK multidex.
/// Each parser invocation has an independent 768 MiB temporary allocation budget.
/// Retained allocations are charged by actual container capacities, with overhead
/// allowances. Failed parsing exhausts the supplied retained budget. This is a
/// parser allocation budget, not a process RSS guarantee or an input-byte budget.
pub fn parse_with_budget(bytes: &[u8], remaining: &mut usize) -> Result<DexFile> {
    let available = *remaining;
    ensure!(
        available > 0,
        UnsupportedDex("native DEX allocation budget exceeded".into())
    );
    // Temporary allocations stay bounded per DEX. The caller-provided balance
    // is a separate aggregate retained-data budget and may legitimately exceed
    // this per-file limit for multidex APKs.
    let mut budget = budget(MAX_BYTES);
    let result = parse_inner(bytes, &mut budget);
    *remaining = 0;
    let dex = result?;
    let retained = retained_charge(&dex)?;
    *remaining = available.checked_sub(retained).ok_or_else(|| {
        UnsupportedDex(format!(
            "native DEX retained allocation budget exceeded (requires {retained} bytes, {available} bytes remain)"
        ))
    })?;
    Ok(dex)
}

// Capacities include Vec spare elements and String spare bytes. Struct storage
// is counted by the owning Vec, not again for each element. A per-allocation
// allowance covers small allocator rounding; allocator-specific RSS differs.
fn retained_charge(dex: &DexFile) -> Result<usize> {
    fn allocation(budget: &mut Budget, capacity: usize, width: usize) -> Result<()> {
        if capacity != 0 {
            budget.take(
                capacity
                    .checked_mul(width)
                    .and_then(|n| n.checked_add(32))
                    .context("native DEX allocation size overflow")?,
            )?;
        }
        Ok(())
    }
    fn string(budget: &mut Budget, value: &Arc<str>) -> Result<()> {
        let key = format!("{:p}", Arc::as_ptr(value));
        if budget.1.contains_key(&key) {
            return Ok(());
        }
        budget.1.insert(key, Arc::clone(value));
        allocation(budget, value.len() + 16, 1)
    }
    fn strings(budget: &mut Budget, values: &Vec<Arc<str>>) -> Result<()> {
        allocation(budget, values.capacity(), std::mem::size_of::<Arc<str>>())?;
        for value in values {
            string(budget, value)?;
        }
        Ok(())
    }
    fn values(budget: &mut Budget, values: &Vec<DexValue>) -> Result<()> {
        allocation(budget, values.capacity(), std::mem::size_of::<DexValue>())?;
        for value in values {
            value_children(budget, value)?;
        }
        Ok(())
    }
    fn value_children(budget: &mut Budget, value: &DexValue) -> Result<()> {
        match value {
            DexValue::Array(children) => values(budget, children)?,
            DexValue::Annotation { elements, .. } => {
                allocation(
                    budget,
                    elements.capacity(),
                    std::mem::size_of::<(u32, DexValue)>(),
                )?;
                for (_, value) in elements {
                    value_children(budget, value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut budget = budget(MAX_BYTES);
    let mut seen_handlers = std::collections::HashSet::new();
    allocation(
        &mut budget,
        dex.classes.capacity(),
        std::mem::size_of::<DexClass>(),
    )?;
    if let Some(class) = dex.classes.first() {
        let symbols = &class.symbols;
        allocation(
            &mut budget,
            symbols.strings.capacity(),
            std::mem::size_of::<String>(),
        )?;
        for value in &symbols.strings {
            allocation(&mut budget, value.capacity(), 1)?;
        }
        strings(&mut budget, &symbols.types)?;
        allocation(
            &mut budget,
            symbols.protos.capacity(),
            std::mem::size_of::<(Arc<str>, Vec<Arc<str>>)>(),
        )?;
        for (ret, params) in &symbols.protos {
            string(&mut budget, ret)?;
            strings(&mut budget, params)?;
        }
        allocation(&mut budget, symbols.fields.capacity(), 8)?;
        allocation(&mut budget, symbols.methods.capacity(), 8)?;
    }
    for class in &dex.classes {
        values(&mut budget, &class.static_values)?;
        string(&mut budget, &class.descriptor)?;
        if let Some(value) = &class.superclass {
            string(&mut budget, value)?;
        }
        strings(&mut budget, &class.interfaces)?;
        allocation(
            &mut budget,
            class.fields.capacity(),
            std::mem::size_of::<DexField>(),
        )?;
        for field in &class.fields {
            for value in [&field.declaring_type, &field.name, &field.field_type] {
                string(&mut budget, value)?;
            }
        }
        allocation(
            &mut budget,
            class.methods.capacity(),
            std::mem::size_of::<DexMethod>(),
        )?;
        for method in &class.methods {
            for value in [&method.declaring_type, &method.name, &method.return_type] {
                string(&mut budget, value)?;
            }
            strings(&mut budget, &method.parameters)?;
            strings(&mut budget, &method.thrown_types)?;
            if let Some(code) = &method.code {
                allocation(&mut budget, code.instructions.capacity(), 2)?;
                allocation(
                    &mut budget,
                    code.try_regions.capacity(),
                    std::mem::size_of::<DexTryRegion>(),
                )?;
                for region in &code.try_regions {
                    if seen_handlers.insert(Arc::as_ptr(&region.catches) as *const () as usize) {
                        allocation(
                            &mut budget,
                            region.catches.len(),
                            std::mem::size_of::<(Option<Arc<str>>, u32)>(),
                        )?;
                        budget.take(16)?;
                        for (ty, _) in region.catches.iter() {
                            if let Some(ty) = ty {
                                string(&mut budget, ty)?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(MAX_BYTES - budget.0)
}

fn parse_inner(bytes: &[u8], budget: &mut Budget) -> Result<DexFile> {
    ensure!(
        bytes.len() >= 112 && bytes.len() <= MAX_BYTES,
        "invalid DEX file size"
    );
    ensure!(
        &bytes[..4] == b"dex\n" && bytes[7] == 0,
        "invalid DEX magic"
    );
    ensure!(
        matches!(&bytes[4..7], b"035" | b"037" | b"038" | b"039" | b"040"),
        "unsupported DEX version"
    );
    let reader = Reader { bytes };
    ensure!(
        reader.u32(32)? as usize == bytes.len(),
        "DEX file size mismatch"
    );
    ensure!(reader.u32(36)? == 112, "unsupported DEX header size");
    ensure!(reader.u32(40)? == 0x12345678, "unsupported DEX endianness");
    let data_size = reader.u32(104)? as usize;
    let data_start = reader.u32(108)? as usize;
    ensure!(
        data_start >= 112 && data_start.is_multiple_of(4),
        "invalid DEX data section"
    );
    ensure!(
        data_start.checked_add(data_size) == Some(bytes.len()),
        "DEX data size mismatch"
    );
    let mut ranges = Vec::new();
    for (header, width) in [(56, 4), (64, 4), (72, 12), (80, 8), (88, 8), (96, 32)] {
        let (count, offset) = reader.table(header, width)?;
        if count != 0 {
            let end = offset + count * width;
            ensure!(end <= data_start, "DEX ID table overlaps data section");
            ranges.push((offset, end));
        }
    }
    ranges.sort_unstable();
    ensure!(
        ranges.windows(2).all(|pair| pair[0].1 <= pair[1].0),
        "overlapping DEX ID tables"
    );
    let (count, offset) = reader.table(56, 4)?;
    budget.take(count * 24)?;
    let strings = (0..count)
        .map(|i| {
            let offset = reader.data_offset(reader.u32(offset + i * 4)?, data_start)?;
            reader.string(offset, budget)
        })
        .collect::<Result<Vec<_>>>()?;
    let (count, offset) = reader.table(64, 4)?;
    let types = (0..count)
        .map(|i| budget.string(at(&strings, reader.u32(offset + i * 4)?)?))
        .collect::<Result<Vec<_>>>()?;
    let (count, offset) = reader.table(72, 12)?;
    budget.take(count * 48)?;
    let protos = (0..count)
        .map(|i| {
            let pos = offset + i * 12;
            at(&strings, reader.u32(pos)?)?;
            Ok((
                budget.string(at(&types, reader.u32(pos + 4)?)?)?,
                type_list(&reader, reader.u32(pos + 8)?, data_start, &types, budget)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let (field_count, field_offset) = reader.table(80, 8)?;
    let (method_count, method_offset) = reader.table(88, 8)?;
    // Validate referenced symbols even when they are not declared in this DEX.
    for i in 0..field_count {
        let p = field_offset + i * 8;
        at(&types, u32::from(reader.u16(p)?))?;
        at(&types, u32::from(reader.u16(p + 2)?))?;
        at(&strings, reader.u32(p + 4)?)?;
    }
    for i in 0..method_count {
        let p = method_offset + i * 8;
        at(&types, u32::from(reader.u16(p)?))?;
        at(&protos, u32::from(reader.u16(p + 2)?))?;
        at(&strings, reader.u32(p + 4)?)?;
    }
    let (count, offset) = reader.table(96, 32)?;
    budget.take(count * (std::mem::size_of::<DexClass>() + 64))?;
    let mut classes = Vec::with_capacity(count);
    let mut class_ids = std::collections::HashSet::new();
    let mut annotations = metadata::AnnotationCache::default();
    for i in 0..count {
        let p = offset + i * 32;
        let class_id = reader.u32(p)?;
        ensure!(class_ids.insert(class_id), "duplicate DEX class definition");
        let descriptor = budget.string(at(&types, class_id)?.as_ref())?;
        let superclass = match reader.u32(p + 8)? {
            NO_INDEX => None,
            index => Some(budget.string(at(&types, index)?)?),
        };
        let interfaces = type_list(&reader, reader.u32(p + 12)?, data_start, &types, budget)?;
        let source = reader.u32(p + 16)?;
        if source != NO_INDEX {
            at(&strings, source)?;
        }
        let annotations_offset = reader.u32(p + 20)?;
        let static_values_offset = reader.u32(p + 28)?;
        for opaque in [annotations_offset, static_values_offset] {
            if opaque != 0 {
                reader.data_offset(opaque, data_start)?;
            }
        }
        let mut class = DexClass {
            symbols: Arc::default(),
            descriptor,
            superclass,
            interfaces,
            access_flags: reader.u32(p + 4)?,
            annotations_offset,
            static_values_offset,
            static_values: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
        };
        let class_data = reader.u32(p + 24)?;
        let mut seen_fields = std::collections::HashSet::new();
        let mut field_positions = std::collections::HashMap::new();
        let mut method_positions = std::collections::HashMap::new();
        if class_data != 0 {
            let mut cursor = reader.data_offset(class_data, data_start)?;
            let counts = [
                reader.uleb(&mut cursor)?,
                reader.uleb(&mut cursor)?,
                reader.uleb(&mut cursor)?,
                reader.uleb(&mut cursor)?,
            ];
            ensure!(
                u64::from(counts[0]) + u64::from(counts[1]) <= field_count as u64,
                "invalid DEX field count"
            );
            ensure!(
                u64::from(counts[2]) + u64::from(counts[3]) <= method_count as u64,
                "invalid DEX method count"
            );
            let mut seen_methods = std::collections::HashSet::new();
            for (group, count) in counts.into_iter().enumerate() {
                let mut index = 0u32;
                for _ in 0..count {
                    index = index
                        .checked_add(reader.uleb(&mut cursor)?)
                        .context("DEX member index overflow")?;
                    let flags = reader.uleb(&mut cursor)?;
                    if group < 2 {
                        ensure!(
                            (index as usize) < field_count && seen_fields.insert(index),
                            "invalid/duplicate DEX field index"
                        );
                        let pos = field_offset + index as usize * 8;
                        ensure!(
                            u32::from(reader.u16(pos)?) == class_id,
                            "DEX field owner mismatch"
                        );
                        budget.take(2 * std::mem::size_of::<DexField>() + 64)?;
                        field_positions.insert(index, class.fields.len());
                        class.fields.push(DexField {
                            declaring_type: budget.string(&class.descriptor)?,
                            name: budget.string(at(&strings, reader.u32(pos + 4)?)?)?,
                            field_type: budget
                                .string(at(&types, u32::from(reader.u16(pos + 2)?))?)?,
                            access_flags: flags,
                            is_static: group == 0,
                        });
                    } else {
                        ensure!(
                            (index as usize) < method_count && seen_methods.insert(index),
                            "invalid/duplicate DEX method index"
                        );
                        let pos = method_offset + index as usize * 8;
                        ensure!(
                            u32::from(reader.u16(pos)?) == class_id,
                            "DEX method owner mismatch"
                        );
                        let proto = at(&protos, u32::from(reader.u16(pos + 2)?))?;
                        let code_offset = reader.uleb(&mut cursor)?;
                        method_positions.insert(index, class.methods.len());
                        budget.take(2 * std::mem::size_of::<DexMethod>() + 64)?;
                        class.methods.push(DexMethod {
                            declaring_type: budget.string(&class.descriptor)?,
                            name: budget.string(at(&strings, reader.u32(pos + 4)?)?)?,
                            return_type: budget.string(&proto.0)?,
                            parameters: proto
                                .1
                                .iter()
                                .map(|p| budget.string(p))
                                .collect::<Result<_>>()?,
                            thrown_types: Vec::new(),
                            access_flags: flags,
                            code: code(&reader, code_offset, data_start, &types, budget)?,
                        });
                    }
                }
            }
        }
        class.static_values = metadata::static_values(
            &reader,
            static_values_offset,
            data_start,
            &metadata::ValueTables {
                types: &types,
                strings: strings.len(),
                protos: protos.len(),
                fields: field_count,
                methods: method_count,
            },
            &class.fields,
            budget,
        )?;
        annotations.populate_methods(
            &reader,
            annotations_offset,
            data_start,
            &metadata::ValueTables {
                types: &types,
                strings: strings.len(),
                protos: protos.len(),
                fields: field_count,
                methods: method_count,
            },
            &strings,
            &seen_fields,
            &field_positions,
            &method_positions,
            &mut class.methods,
            budget,
        )?;
        classes.push(class);
    }
    budget.take((field_count + method_count) * 8)?;
    let fields = (0..field_count)
        .map(|i| {
            let p = field_offset + i * 8;
            Ok((reader.u16(p)?, reader.u16(p + 2)?, reader.u32(p + 4)?))
        })
        .collect::<Result<Vec<_>>>()?;
    let methods = (0..method_count)
        .map(|i| {
            let p = method_offset + i * 8;
            Ok((reader.u16(p)?, reader.u16(p + 2)?, reader.u32(p + 4)?))
        })
        .collect::<Result<Vec<_>>>()?;
    let symbols = Arc::new(DexSymbols {
        hierarchy: Default::default(),
        strings,
        types,
        protos,
        fields,
        methods,
        annotations: annotations.into_directories(),
    });
    for class in &mut classes {
        class.symbols = Arc::clone(&symbols);
    }
    Ok(DexFile { classes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_real_fixture_classes_and_code() {
        for bytes in [
            include_bytes!("../tests/fixtures/hello.dex").as_slice(),
            include_bytes!("../tests/fixtures/navigation.dex").as_slice(),
        ] {
            let dex = parse(bytes).unwrap();
            assert!(!dex.classes.is_empty());
            assert!(dex.classes.iter().flat_map(|c| &c.methods).any(|m| {
                m.code
                    .as_ref()
                    .is_some_and(|code| !code.instructions.is_empty())
            }));
            assert!(
                dex.classes
                    .iter()
                    .all(|c| c.descriptor.starts_with('L') && c.descriptor.ends_with(';'))
            );
        }
    }

    #[test]
    fn malformed_header_and_tables_are_rejected() {
        let original = include_bytes!("../tests/fixtures/hello.dex");
        for length in [0, 7, 111, original.len() - 1] {
            assert!(parse(&original[..length]).is_err());
        }
        for (offset, value) in [
            (32, 0),
            (36, 120),
            (40, 0x78563412),
            (56, u32::MAX),
            (60, u32::MAX),
            (108, 0),
        ] {
            let mut bytes = original.to_vec();
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(parse(&bytes).is_err());
        }
        let mut bytes = original.to_vec();
        bytes[4..7].copy_from_slice(b"041");
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn modified_utf8_preserves_nul_and_supplementary_characters() {
        let bytes = [4, b'A', 0xc0, 0x80, 0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80, 0];
        assert_eq!(
            Reader { bytes: &bytes }
                .string(0, &mut budget(100))
                .unwrap(),
            "A\0😀"
        );
        assert_eq!(
            Reader {
                bytes: &[1, 0xed, 0xa0, 0x80, 0]
            }
            .string(0, &mut budget(100))
            .unwrap(),
            "\\u{d800}"
        );
        assert_eq!(
            Reader {
                bytes: &[1, b'\\', 0]
            }
            .string(0, &mut budget(100))
            .unwrap(),
            "\\\\"
        );
        for bytes in [&[1, 0xc1, 0x81, 0][..], &[2, b'A', 0], &[1, 0xf0, 0]] {
            assert!(Reader { bytes }.string(0, &mut budget(100)).is_err());
        }
    }

    #[test]
    fn uleb_overflow_and_allocation_budget_are_rejected() {
        assert!(
            Reader {
                bytes: &[255, 255, 255, 255, 31]
            }
            .uleb(&mut 0)
            .is_err()
        );
        assert!(Reader { bytes: &[128] }.uleb(&mut 0).is_err());
        assert!(
            Reader {
                bytes: &[3, b'a', b'b', b'c', 0]
            }
            .string(0, &mut budget(1))
            .is_err()
        );
    }

    #[test]
    fn fixture_symbols_and_code_metadata_are_exact() {
        let dex = parse(include_bytes!("../tests/fixtures/hello.dex")).unwrap();
        assert_eq!(dex.classes.len(), 1);
        let class = &dex.classes[0];
        assert_eq!(class.descriptor.as_ref(), "Lsample/Hello;");
        assert_eq!(class.superclass.as_deref(), Some("Ljava/lang/Object;"));
        let method = class
            .methods
            .iter()
            .find(|m| m.name.as_ref() == "answer")
            .unwrap();
        assert_eq!(method.return_type.as_ref(), "I");
        assert!(method.parameters.is_empty());
        assert_eq!(method.access_flags & 9, 9);
        let code = method.code.as_ref().unwrap();
        assert_eq!(code.ins, 0);
        assert_eq!(code.tries, 0);
        assert_eq!(code.instructions, [0x0013, 42, 0x000f]);
    }

    #[test]
    fn bad_code_sizes_and_string_indices_are_rejected() {
        let original = include_bytes!("../tests/fixtures/hello.dex");
        let dex = parse(original).unwrap();
        let code_offset = dex.classes[0]
            .methods
            .iter()
            .find_map(|m| m.code.as_ref())
            .unwrap()
            .offset as usize;
        let mut bytes = original.to_vec();
        bytes[code_offset + 12..code_offset + 16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&bytes).is_err());
        let mut bytes = original.to_vec();
        let type_table = Reader { bytes: original }.u32(68).unwrap() as usize;
        bytes[type_table..type_table + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn multidex_calls_share_allocation_budget() {
        let bytes = include_bytes!("../tests/fixtures/hello.dex");
        let mut remaining = MAX_BYTES;
        let first = parse_with_budget(bytes, &mut remaining).unwrap();
        let consumed = MAX_BYTES - remaining;
        assert!(consumed > 0);
        assert_eq!(consumed, retained_charge(&first).unwrap());
        parse_with_budget(bytes, &mut remaining).unwrap();
        assert_eq!(remaining, MAX_BYTES - 2 * consumed);
        assert!(
            !first.classes[0].symbols.strings.is_empty(),
            "operand symbols are retained for disassembly"
        );
        let mut remaining = 0;
        assert!(parse_with_budget(bytes, &mut remaining).is_err());
    }

    #[test]
    fn retained_budget_above_per_file_limit_is_not_clamped() {
        let bytes = include_bytes!("../tests/fixtures/hello.dex");
        let charge = retained_charge(&parse(bytes).unwrap()).unwrap();
        let initial = MAX_BYTES + charge * 2;
        let mut remaining = initial;
        parse_with_budget(bytes, &mut remaining).unwrap();
        parse_with_budget(bytes, &mut remaining).unwrap();
        assert_eq!(remaining, initial - charge * 2);

        let mut too_small = charge - 1;
        assert!(parse_with_budget(bytes, &mut too_small).is_err());
        assert_eq!(too_small, 0);

        let mut extreme = usize::MAX;
        parse_with_budget(bytes, &mut extreme).unwrap();
        assert_eq!(extreme, usize::MAX - charge);
    }

    #[test]
    fn metadata_descriptors_are_interned() {
        let dex = parse(include_bytes!("../tests/fixtures/hello.dex")).unwrap();
        let class = &dex.classes[0];
        assert!(Arc::ptr_eq(
            &class.descriptor,
            &class.methods[0].declaring_type
        ));
        let ty = class
            .symbols
            .types
            .iter()
            .find(|ty| ty.as_ref() == class.descriptor.as_ref())
            .unwrap();
        assert!(Arc::ptr_eq(&class.descriptor, ty));
    }

    #[test]
    fn budget_limit_is_typed_but_malformed_bytes_are_not() {
        let bytes = include_bytes!("../tests/fixtures/hello.dex");
        let error = parse_with_budget(bytes, &mut 0).unwrap_err();
        assert!(error.downcast_ref::<UnsupportedDex>().is_some());
        assert!(
            parse(b"not a dex")
                .unwrap_err()
                .downcast_ref::<UnsupportedDex>()
                .is_none()
        );
        let mut bytes = bytes.to_vec();
        bytes[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            parse(&bytes)
                .unwrap_err()
                .downcast_ref::<UnsupportedDex>()
                .is_none()
        );
    }
}
