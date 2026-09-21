//! Checked encoded values and catch-handler metadata, following the AOSP DEX format:
//! https://source.android.com/docs/core/runtime/dex-format
use super::{
    Budget, DexAnnotation, DexAnnotationDirectory, DexAnnotationSet, DexField, DexTryRegion,
    DexValue, MAX_ITEMS, Reader, at,
};
use anyhow::{Context, Result, bail, ensure};
use std::{collections::BTreeMap, sync::Arc};

pub(super) struct ValueTables<'a> {
    pub types: &'a [Arc<str>],
    pub strings: usize,
    pub protos: usize,
    pub fields: usize,
    pub methods: usize,
}

fn unsigned(reader: &Reader<'_>, cursor: &mut usize, width: usize) -> Result<u64> {
    let mut value = 0u64;
    for byte in 0..width {
        value |= u64::from(reader.byte(cursor)?) << (8 * byte);
    }
    Ok(value)
}

fn signed(reader: &Reader<'_>, cursor: &mut usize, width: usize) -> Result<i64> {
    let value = unsigned(reader, cursor, width)?;
    let shift = 64 - width * 8;
    Ok(((value << shift) as i64) >> shift)
}

/// SLEB128 quantities are signed 32-bit values, including legal sign extension.
fn sleb(reader: &Reader<'_>, cursor: &mut usize) -> Result<i32> {
    let mut value = 0i64;
    for i in 0..5 {
        let byte = reader.byte(cursor)?;
        value |= i64::from(byte & 127) << (i * 7);
        if byte & 128 == 0 {
            if byte & 64 != 0 {
                value |= !0i64 << ((i + 1) * 7);
            }
            return i32::try_from(value).context("DEX SLEB128 overflow");
        }
    }
    bail!("unterminated DEX SLEB128")
}

fn method_handles(reader: &Reader<'_>, fields: usize, methods: usize) -> Result<usize> {
    let map = reader.data_offset(reader.u32(52)?, reader.u32(108)? as usize)?;
    ensure!(
        map != 0 && map.is_multiple_of(4),
        "Invalid method-handle map offset"
    );
    let count = reader.u32(map)? as usize;
    ensure!(count <= MAX_ITEMS, "DEX map exceeds item budget");
    reader.range(map + 4, count * 12)?;
    let mut handles = None;
    for i in 0..count {
        let position = map + 4 + i * 12;
        if reader.u16(position)? != 8 {
            continue;
        }
        ensure!(handles.is_none(), "Duplicate DEX method-handle section");
        let size = reader.u32(position + 4)? as usize;
        let start = reader.data_offset(reader.u32(position + 8)?, reader.u32(108)? as usize)?;
        ensure!(
            size <= MAX_ITEMS && start.is_multiple_of(4),
            "Invalid method-handle section"
        );
        reader.range(start, size * 8)?;
        for j in 0..size {
            let p = start + j * 8;
            let kind = reader.u16(p)?;
            ensure!(
                kind <= 8 && reader.u16(p + 2)? == 0 && reader.u16(p + 6)? == 0,
                "Invalid method-handle item"
            );
            let index = reader.u16(p + 4)? as usize;
            ensure!(
                index < if kind <= 3 { fields } else { methods },
                "Method-handle member index out of range"
            );
        }
        handles = Some(size);
    }
    Ok(handles.unwrap_or(0))
}

struct Values<'a, 'b> {
    reader: &'a Reader<'b>,
    tables: &'a ValueTables<'a>,
    budget: &'a mut Budget,
    remaining: usize,
    handles: Option<usize>,
}

impl Values<'_, '_> {
    fn count(&mut self, cursor: &mut usize, width: usize) -> Result<usize> {
        let count = self.reader.uleb(cursor)? as usize;
        ensure!(
            count <= self.remaining && count <= self.reader.bytes.len() - *cursor,
            "Encoded value count exceeds remaining input or budget"
        );
        self.budget.take(
            count
                .checked_mul(width)
                .and_then(|n| n.checked_add(32))
                .context("Encoded value allocation overflow")?,
        )?;
        Ok(count)
    }

    fn array(&mut self, cursor: &mut usize, depth: usize) -> Result<Vec<DexValue>> {
        let count = self.count(cursor, std::mem::size_of::<DexValue>())?;
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(self.value(cursor, depth)?);
        }
        Ok(values)
    }

    fn value(&mut self, cursor: &mut usize, depth: usize) -> Result<DexValue> {
        ensure!(depth <= 64, "Encoded value nesting exceeds 64 levels");
        self.remaining = self
            .remaining
            .checked_sub(1)
            .context("Encoded value item budget exceeded")?;
        let header = self.reader.byte(cursor)?;
        let kind = header & 31;
        let arg = header >> 5;
        let width = usize::from(arg) + 1;
        let limit = match kind {
            0x00 | 0x1c..=0x1e => 0,
            0x02 | 0x03 | 0x1f => 1,
            0x04 | 0x10 | 0x15..=0x1b => 3,
            0x06 | 0x11 => 7,
            _ => bail!("Invalid DEX encoded value type 0x{kind:02x}"),
        };
        ensure!(arg <= limit, "Invalid DEX encoded value argument");
        Ok(match kind {
            0x00 => DexValue::Byte(signed(self.reader, cursor, width)? as i8),
            0x02 => DexValue::Short(signed(self.reader, cursor, width)? as i16),
            0x03 => DexValue::Char(unsigned(self.reader, cursor, width)? as u16),
            0x04 => DexValue::Int(signed(self.reader, cursor, width)? as i32),
            0x06 => DexValue::Long(signed(self.reader, cursor, width)?),
            0x10 => {
                DexValue::Float((unsigned(self.reader, cursor, width)? << (8 * (4 - width))) as u32)
            }
            0x11 => DexValue::Double(unsigned(self.reader, cursor, width)? << (8 * (8 - width))),
            0x15..=0x1b => {
                let index = unsigned(self.reader, cursor, width)? as u32;
                let count = match kind {
                    0x15 => self.tables.protos,
                    0x16 => {
                        if self.handles.is_none() {
                            self.handles = Some(method_handles(
                                self.reader,
                                self.tables.fields,
                                self.tables.methods,
                            )?);
                        }
                        self.handles.unwrap()
                    }
                    0x17 => self.tables.strings,
                    0x18 => self.tables.types.len(),
                    0x19 | 0x1b => self.tables.fields,
                    0x1a => self.tables.methods,
                    _ => unreachable!(),
                };
                ensure!(
                    (index as usize) < count,
                    "Encoded value symbol index out of range"
                );
                match kind {
                    0x15 => DexValue::MethodType(index),
                    0x16 => DexValue::MethodHandle(index),
                    0x17 => DexValue::String(index),
                    0x18 => DexValue::Type(index),
                    0x19 => DexValue::Field(index),
                    0x1a => DexValue::Method(index),
                    _ => DexValue::Enum(index),
                }
            }
            0x1c => DexValue::Array(self.array(cursor, depth + 1)?),
            0x1d => self.annotation(cursor, depth)?,
            0x1e => DexValue::Null,
            0x1f => DexValue::Boolean(arg != 0),
            _ => unreachable!(),
        })
    }
    fn annotation(&mut self, cursor: &mut usize, depth: usize) -> Result<DexValue> {
        let type_idx = self.reader.uleb(cursor)?;
        let ty = at(self.tables.types, type_idx)?;
        ensure!(
            ty.len() > 2 && ty.starts_with('L') && ty.ends_with(';'),
            "Annotation requires class type"
        );
        let count = self.count(cursor, std::mem::size_of::<(u32, DexValue)>())?;
        let mut elements = Vec::with_capacity(count);
        let mut previous = None;
        for _ in 0..count {
            let name = self.reader.uleb(cursor)?;
            ensure!(
                (name as usize) < self.tables.strings && previous.is_none_or(|p| name > p),
                "Invalid/unsorted encoded annotation name"
            );
            previous = Some(name);
            elements.push((name, self.value(cursor, depth + 1)?));
        }
        Ok(DexValue::Annotation { type_idx, elements })
    }
}

#[derive(Clone)]
struct AnnotationSummary {
    annotation: Arc<DexAnnotation>,
    throws: Option<Arc<[Arc<str>]>>,
}

#[derive(Clone)]
struct AnnotationSetSummary {
    annotations: DexAnnotationSet,
    throws: Option<Arc<[Arc<str>]>>,
}

/// Cache shared annotation items and sets once per DEX.
#[derive(Default)]
pub(super) struct AnnotationCache {
    items: BTreeMap<u32, AnnotationSummary>,
    sets: BTreeMap<u32, AnnotationSetSummary>,
    directories: BTreeMap<u32, Arc<DexAnnotationDirectory>>,
    work: usize,
}

struct AnnotationInput<'a, 'b> {
    reader: &'a Reader<'b>,
    tables: &'a ValueTables<'a>,
    strings: &'a [String],
    data_start: usize,
}

impl AnnotationCache {
    fn tick(&mut self, count: usize) -> Result<()> {
        self.work = self
            .work
            .checked_add(count)
            .context("Annotation work overflow")?;
        ensure!(
            self.work <= 8 * MAX_ITEMS,
            "Annotation validation exceeds work budget"
        );
        Ok(())
    }
    fn item(
        &mut self,
        input: &AnnotationInput<'_, '_>,
        offset: u32,
        budget: &mut Budget,
    ) -> Result<AnnotationSummary> {
        self.tick(1)?;
        if let Some(summary) = self.items.get(&offset) {
            return Ok(summary.clone());
        }
        budget.take(96)?;
        let mut cursor = input.reader.data_offset(offset, input.data_start)?;
        let visibility = input.reader.byte(&mut cursor)?;
        ensure!(visibility <= 2, "Invalid annotation visibility");
        let mut values = Values {
            reader: input.reader,
            tables: input.tables,
            budget,
            remaining: MAX_ITEMS,
            handles: None,
        };
        let DexValue::Annotation { type_idx, elements } = values.annotation(&mut cursor, 0)? else {
            unreachable!()
        };
        let throws =
            if input.tables.types[type_idx as usize].as_ref() == "Ldalvik/annotation/Throws;" {
                ensure!(
                    visibility == 2
                        && elements.len() == 1
                        && input.strings[elements[0].0 as usize] == "value",
                    "Malformed Throws annotation"
                );
                let DexValue::Array(types) = &elements[0].1 else {
                    bail!("Throws annotation requires type array");
                };
                budget.take(types.len() * 2 * std::mem::size_of::<Arc<str>>() + 32)?;
                let mut result = Vec::with_capacity(types.len());
                for value in types {
                    let DexValue::Type(index) = value else {
                        bail!("Throws annotation contains non-type value");
                    };
                    let ty = at(input.tables.types, *index)?;
                    ensure!(
                        ty.len() > 2 && ty.starts_with('L') && ty.ends_with(';'),
                        "Throws annotation requires class types"
                    );
                    result.push(Arc::clone(ty));
                }
                Some(Arc::<[Arc<str>]>::from(result))
            } else {
                None
            };
        let summary = AnnotationSummary {
            annotation: Arc::new(DexAnnotation {
                visibility,
                type_idx,
                elements,
            }),
            throws,
        };
        self.items.insert(offset, summary.clone());
        Ok(summary)
    }

    fn set(
        &mut self,
        input: &AnnotationInput<'_, '_>,
        offset: u32,
        budget: &mut Budget,
    ) -> Result<Option<AnnotationSetSummary>> {
        self.tick(1)?;
        if offset == 0 {
            return Ok(None);
        }
        if let Some(value) = self.sets.get(&offset) {
            return Ok(Some(value.clone()));
        }
        budget.take(80)?;
        let start = input.reader.data_offset(offset, input.data_start)?;
        ensure!(start.is_multiple_of(4), "Unaligned annotation set");
        let count = input.reader.u32(start)? as usize;
        ensure!(count <= MAX_ITEMS, "Annotation set count exceeds budget");
        budget.take(count * std::mem::size_of::<Arc<DexAnnotation>>() + 32)?;
        self.tick(count)?;
        input.reader.range(start + 4, count * 4)?;
        let mut previous = None;
        let mut throws = None;
        let mut annotations = Vec::with_capacity(count);
        for i in 0..count {
            let item = self.item(input, input.reader.u32(start + 4 + i * 4)?, budget)?;
            ensure!(
                previous.is_none_or(|p| item.annotation.type_idx > p),
                "Duplicate or unsorted annotation set types"
            );
            previous = Some(item.annotation.type_idx);
            if item.throws.is_some() {
                throws = item.throws;
            }
            annotations.push(item.annotation);
        }
        let summary = AnnotationSetSummary {
            annotations: Arc::from(annotations),
            throws,
        };
        self.sets.insert(offset, summary.clone());
        Ok(Some(summary))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn populate_methods(
        &mut self,
        reader: &Reader<'_>,
        offset: u32,
        data_start: usize,
        tables: &ValueTables<'_>,
        strings: &[String],
        fields: &std::collections::HashSet<u32>,
        field_positions: &std::collections::HashMap<u32, usize>,
        method_positions: &std::collections::HashMap<u32, usize>,
        methods: &mut [super::DexMethod],
        budget: &mut Budget,
    ) -> Result<()> {
        if offset == 0 {
            return Ok(());
        }
        let input = AnnotationInput {
            reader,
            tables,
            strings,
            data_start,
        };
        let start = reader.data_offset(offset, data_start)?;
        ensure!(start.is_multiple_of(4), "Unaligned annotation directory");
        reader.range(start, 16)?;
        let counts = [
            reader.u32(start + 4)? as usize,
            reader.u32(start + 8)? as usize,
            reader.u32(start + 12)? as usize,
        ];
        let total = counts.iter().try_fold(0usize, |sum, count| {
            sum.checked_add(*count)
                .context("Annotation directory count overflow")
        })?;
        self.tick(total)?;
        ensure!(
            total <= MAX_ITEMS,
            "Annotation directory count exceeds budget"
        );
        reader.range(start + 16, total * 8)?;
        let slot_size = std::mem::size_of::<Option<DexAnnotationSet>>();
        let field_bytes = if counts[0] == 0 {
            0
        } else {
            fields
                .len()
                .checked_mul(slot_size)
                .context("Annotation field allocation overflow")?
        };
        let method_bytes = if counts[1] == 0 {
            0
        } else {
            methods
                .len()
                .checked_mul(slot_size)
                .context("Annotation method allocation overflow")?
        };
        let parameter_bytes = if counts[2] == 0 {
            0
        } else {
            methods
                .len()
                .checked_mul(std::mem::size_of::<Vec<Option<DexAnnotationSet>>>())
                .context("Annotation parameter allocation overflow")?
        };
        budget.take(
            field_bytes
                .checked_add(method_bytes)
                .and_then(|bytes| bytes.checked_add(parameter_bytes))
                .and_then(|bytes| bytes.checked_add(128))
                .context("Annotation directory allocation overflow")?,
        )?;
        let mut directory = DexAnnotationDirectory {
            class: self
                .set(&input, reader.u32(start)?, budget)?
                .map(|summary| summary.annotations),
            fields: if counts[0] == 0 {
                Vec::new()
            } else {
                vec![None; fields.len()]
            },
            methods: if counts[1] == 0 {
                Vec::new()
            } else {
                vec![None; methods.len()]
            },
            parameters: if counts[2] == 0 {
                Vec::new()
            } else {
                vec![Vec::new(); methods.len()]
            },
        };
        let mut cursor = start + 16;
        for (kind, count) in counts.into_iter().enumerate() {
            let mut previous = None;
            for _ in 0..count {
                let index = reader.u32(cursor)?;
                let annotations = reader.u32(cursor + 4)?;
                cursor += 8;
                ensure!(
                    previous.is_none_or(|p| index > p),
                    "Duplicate or unsorted annotation directory members"
                );
                previous = Some(index);
                ensure!(annotations != 0, "Missing member annotation offset");
                if kind == 0 {
                    ensure!(
                        (index as usize) < tables.fields && fields.contains(&index),
                        "Annotation field does not belong to class"
                    );
                    let position = *field_positions
                        .get(&index)
                        .context("Annotation field does not belong to class")?;
                    directory.fields[position] = self
                        .set(&input, annotations, budget)?
                        .map(|summary| summary.annotations);
                    continue;
                }
                ensure!(
                    (index as usize) < tables.methods,
                    "Annotation method index out of range"
                );
                let position = *method_positions
                    .get(&index)
                    .context("Annotation method does not belong to class")?;
                if kind == 1 {
                    if let Some(summary) = self.set(&input, annotations, budget)? {
                        directory.methods[position] = Some(Arc::clone(&summary.annotations));
                        if let Some(throws) = summary.throws {
                            budget.take(throws.len() * std::mem::size_of::<Arc<str>>() + 32)?;
                            methods[position].thrown_types = throws.to_vec();
                        }
                    }
                } else {
                    let list = reader.data_offset(annotations, data_start)?;
                    ensure!(
                        list.is_multiple_of(4),
                        "Unaligned parameter annotation list"
                    );
                    let count = reader.u32(list)? as usize;
                    ensure!(
                        count <= methods[position].parameters.len(),
                        "Parameter annotations exceed declared parameters"
                    );
                    reader.range(list + 4, count * 4)?;
                    budget.take(
                        count
                            .checked_mul(slot_size)
                            .context("Parameter annotation allocation overflow")?,
                    )?;
                    directory.parameters[position] = vec![None; count];
                    for i in 0..count {
                        directory.parameters[position][i] = self
                            .set(&input, reader.u32(list + 4 + i * 4)?, budget)?
                            .map(|summary| summary.annotations);
                    }
                }
            }
        }
        self.directories.insert(offset, Arc::new(directory));
        Ok(())
    }

    pub(super) fn into_directories(self) -> BTreeMap<u32, Arc<DexAnnotationDirectory>> {
        self.directories
    }
}

pub(super) fn static_values(
    reader: &Reader<'_>,
    offset: u32,
    data_start: usize,
    tables: &ValueTables<'_>,
    fields: &[DexField],
    budget: &mut Budget,
) -> Result<Vec<DexValue>> {
    if offset == 0 {
        return Ok(Vec::new());
    }
    let mut cursor = reader.data_offset(offset, data_start)?;
    let mut values = Values {
        reader,
        tables,
        budget,
        remaining: MAX_ITEMS,
        handles: None,
    };
    let result = values.array(&mut cursor, 0)?;
    let fields = fields.iter().filter(|field| field.is_static);
    ensure!(
        result.len() <= fields.clone().count(),
        "More encoded static values than static fields"
    );
    for (value, field) in result.iter().zip(fields) {
        let primitive = match value {
            DexValue::Byte(_) => Some("B"),
            DexValue::Short(_) => Some("S"),
            DexValue::Char(_) => Some("C"),
            DexValue::Int(_) => Some("I"),
            DexValue::Long(_) => Some("J"),
            DexValue::Float(_) => Some("F"),
            DexValue::Double(_) => Some("D"),
            DexValue::Boolean(_) => Some("Z"),
            _ => None,
        };
        ensure!(
            primitive.map_or_else(
                || field.field_type.starts_with('L') || field.field_type.starts_with('['),
                |ty| field.field_type.as_ref() == ty
            ),
            "Encoded static value does not match field type"
        );
    }
    Ok(result)
}

pub(super) fn try_regions(
    reader: &Reader<'_>,
    start: usize,
    count: u16,
    instructions: usize,
    types: &[Arc<str>],
    budget: &mut Budget,
) -> Result<Vec<DexTryRegion>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    reader.range(start, usize::from(count) * 8)?;
    let base = start + usize::from(count) * 8;
    let mut cursor = base;
    let handlers = reader.uleb(&mut cursor)? as usize;
    ensure!(
        handlers > 0 && handlers <= MAX_ITEMS && handlers <= reader.bytes.len() - cursor,
        "Invalid catch-handler list size"
    );
    budget.take(handlers * 96 + usize::from(count) * std::mem::size_of::<DexTryRegion>() + 32)?;
    let mut decoded = BTreeMap::new();
    for _ in 0..handlers {
        let offset = cursor - base;
        let signed_count = sleb(reader, &mut cursor)?;
        let typed = signed_count.unsigned_abs() as usize;
        ensure!(
            typed <= MAX_ITEMS && typed <= (reader.bytes.len() - cursor) / 2,
            "Catch-handler count exceeds input or budget"
        );
        let total = typed + usize::from(signed_count <= 0);
        budget.take(total * (2 * std::mem::size_of::<(Option<Arc<str>>, u32)>()) + 32)?;
        let mut catches = Vec::with_capacity(total);
        for _ in 0..typed {
            let ty = at(types, reader.uleb(&mut cursor)?)?;
            ensure!(
                ty.starts_with('L') && ty.ends_with(';'),
                "Catch requires class type"
            );
            let address = reader.uleb(&mut cursor)?;
            ensure!(
                (address as usize) < instructions,
                "Catch target outside instructions"
            );
            catches.push((Some(Arc::clone(ty)), address));
        }
        if signed_count <= 0 {
            let address = reader.uleb(&mut cursor)?;
            ensure!(
                (address as usize) < instructions,
                "Catch-all target outside instructions"
            );
            catches.push((None, address));
        }
        decoded.insert(offset, Arc::<[(Option<Arc<str>>, u32)]>::from(catches));
    }
    let mut result = Vec::with_capacity(usize::from(count));
    let mut previous_end = 0;
    for i in 0..usize::from(count) {
        let p = start + i * 8;
        let begin = reader.u32(p)?;
        let length = u32::from(reader.u16(p + 4)?);
        let end = begin
            .checked_add(length)
            .context("Try-region range overflow")?;
        ensure!(
            length != 0 && begin >= previous_end && end as usize <= instructions,
            "Invalid or overlapping try-region range"
        );
        previous_end = end;
        let offset = usize::from(reader.u16(p + 6)?);
        let catches = Arc::clone(
            decoded
                .get(&offset)
                .context("Try-region offset is not a handler boundary")?,
        );
        result.push(DexTryRegion {
            start: begin,
            end,
            catches,
        });
    }
    Ok(result)
}
