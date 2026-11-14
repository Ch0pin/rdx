//! Annotations directory / sets / items (DEX).

use crate::bounds::{ensure_count_fits, vec_with_capacity};
use crate::dex::encoded_value::{
    decode_encoded_annotation, encode_encoded_annotation, EncodedAnnotation, EncodedValue,
};
use crate::error::{DexError, Result};
use crate::leb128::read_u32;

pub const VISIBILITY_BUILD: u8 = 0x00;
pub const VISIBILITY_RUNTIME: u8 = 0x01;
pub const VISIBILITY_SYSTEM: u8 = 0x02;

#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationItem {
    pub visibility: u8,
    pub annotation: EncodedAnnotation,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnnotationsDirectory {
    pub class_annotations: Vec<AnnotationItem>,
    pub field_annotations: Vec<(u32, Vec<AnnotationItem>)>, // field_idx, set
    pub method_annotations: Vec<(u32, Vec<AnnotationItem>)>,
    pub parameter_annotations: Vec<(u32, Vec<Vec<AnnotationItem>>)>, // method_idx, per-param sets
}

/// Parse `annotations_directory_item` at `off` (0 → empty).
pub fn parse_annotations_directory(data: &[u8], off: u32) -> Result<AnnotationsDirectory> {
    if off == 0 {
        return Ok(AnnotationsDirectory::default());
    }
    let mut pos = off as usize;
    if pos + 16 > data.len() {
        return Err(DexError::Truncated("annotations_directory".into()));
    }
    let class_annotations_off = read_u32(data, pos).unwrap_or(0);
    let fields_size = read_u32(data, pos + 4).unwrap_or(0);
    let methods_size = read_u32(data, pos + 8).unwrap_or(0);
    let params_size = read_u32(data, pos + 12).unwrap_or(0);
    pos += 16;

    let class_annotations = if class_annotations_off != 0 {
        parse_annotation_set(data, class_annotations_off)?
    } else {
        Vec::new()
    };

    let mut field_annotations = vec_with_capacity(fields_size as usize, "field_annotations")?;
    for _ in 0..fields_size {
        if pos + 8 > data.len() {
            return Err(DexError::Truncated("field_annotation".into()));
        }
        let field_idx = read_u32(data, pos).unwrap_or(0);
        let annotations_off = read_u32(data, pos + 4).unwrap_or(0);
        pos += 8;
        field_annotations.push((field_idx, parse_annotation_set(data, annotations_off)?));
    }

    let mut method_annotations = vec_with_capacity(methods_size as usize, "method_annotations")?;
    for _ in 0..methods_size {
        if pos + 8 > data.len() {
            return Err(DexError::Truncated("method_annotation".into()));
        }
        let method_idx = read_u32(data, pos).unwrap_or(0);
        let annotations_off = read_u32(data, pos + 4).unwrap_or(0);
        pos += 8;
        method_annotations.push((method_idx, parse_annotation_set(data, annotations_off)?));
    }

    let mut parameter_annotations = vec_with_capacity(params_size as usize, "parameter_annotations")?;
    for _ in 0..params_size {
        if pos + 8 > data.len() {
            return Err(DexError::Truncated("parameter_annotation".into()));
        }
        let method_idx = read_u32(data, pos).unwrap_or(0);
        let annotations_off = read_u32(data, pos + 4).unwrap_or(0);
        pos += 8;
        parameter_annotations.push((method_idx, parse_annotation_set_ref_list(data, annotations_off)?));
    }

    Ok(AnnotationsDirectory {
        class_annotations,
        field_annotations,
        method_annotations,
        parameter_annotations,
    })
}

fn parse_annotation_set(data: &[u8], off: u32) -> Result<Vec<AnnotationItem>> {
    if off == 0 {
        return Ok(Vec::new());
    }
    let pos = off as usize;
    if pos + 4 > data.len() {
        return Err(DexError::Truncated("annotation_set".into()));
    }
    let size = read_u32(data, pos).unwrap_or(0) as usize;
    ensure_count_fits(size, data.len().saturating_sub(pos.saturating_add(4)) / 4, "annotation_set")?;
    let mut out = vec_with_capacity(size, "annotation_set")?;
    for i in 0..size {
        let item_off = read_u32(data, pos + 4 + i * 4)
            .ok_or(DexError::Truncated("annotation_off".into()))?;
        out.push(parse_annotation_item(data, item_off)?);
    }
    Ok(out)
}

fn parse_annotation_set_ref_list(data: &[u8], off: u32) -> Result<Vec<Vec<AnnotationItem>>> {
    if off == 0 {
        return Ok(Vec::new());
    }
    let pos = off as usize;
    if pos + 4 > data.len() {
        return Err(DexError::Truncated("annotation_set_ref_list".into()));
    }
    let size = read_u32(data, pos).unwrap_or(0) as usize;
    ensure_count_fits(size, data.len().saturating_sub(pos.saturating_add(4)) / 4, "annotation_set_ref_list")?;
    let mut out = vec_with_capacity(size, "annotation_set_ref_list")?;
    for i in 0..size {
        let set_off = read_u32(data, pos + 4 + i * 4).unwrap_or(0);
        out.push(parse_annotation_set(data, set_off)?);
    }
    Ok(out)
}

fn parse_annotation_item(data: &[u8], off: u32) -> Result<AnnotationItem> {
    let pos = off as usize;
    if pos >= data.len() {
        return Err(DexError::Truncated("annotation_item".into()));
    }
    let visibility = data[pos];
    let (annotation, _) = decode_encoded_annotation(data, pos + 1)?;
    Ok(AnnotationItem {
        visibility,
        annotation,
    })
}

/// Write annotations into `data` (appended); returns directory offset (0 if empty).
pub fn write_annotations_directory(
    data: &mut Vec<u8>,
    data_off: u32,
    dir: &AnnotationsDirectory,
) -> Result<u32> {
    if dir.class_annotations.is_empty()
        && dir.field_annotations.is_empty()
        && dir.method_annotations.is_empty()
        && dir.parameter_annotations.is_empty()
    {
        return Ok(0);
    }

    let class_set_off = write_annotation_set(data, data_off, &dir.class_annotations)?;

    let mut field_entries = Vec::new();
    for (idx, set) in &dir.field_annotations {
        let off = write_annotation_set(data, data_off, set)?;
        field_entries.push((*idx, off));
    }
    let mut method_entries = Vec::new();
    for (idx, set) in &dir.method_annotations {
        let off = write_annotation_set(data, data_off, set)?;
        method_entries.push((*idx, off));
    }
    let mut param_entries = Vec::new();
    for (idx, sets) in &dir.parameter_annotations {
        let off = write_annotation_set_ref_list(data, data_off, sets)?;
        param_entries.push((*idx, off));
    }

    align4(data);
    let dir_off = data_off + data.len() as u32;
    data.extend_from_slice(&class_set_off.to_le_bytes());
    data.extend_from_slice(&(field_entries.len() as u32).to_le_bytes());
    data.extend_from_slice(&(method_entries.len() as u32).to_le_bytes());
    data.extend_from_slice(&(param_entries.len() as u32).to_le_bytes());
    for (idx, off) in field_entries {
        data.extend_from_slice(&idx.to_le_bytes());
        data.extend_from_slice(&off.to_le_bytes());
    }
    for (idx, off) in method_entries {
        data.extend_from_slice(&idx.to_le_bytes());
        data.extend_from_slice(&off.to_le_bytes());
    }
    for (idx, off) in param_entries {
        data.extend_from_slice(&idx.to_le_bytes());
        data.extend_from_slice(&off.to_le_bytes());
    }
    Ok(dir_off)
}

fn write_annotation_set(data: &mut Vec<u8>, data_off: u32, set: &[AnnotationItem]) -> Result<u32> {
    if set.is_empty() {
        return Ok(0);
    }
    let mut item_offs = Vec::with_capacity(set.len());
    for item in set {
        item_offs.push(write_annotation_item(data, data_off, item));
    }
    align4(data);
    let set_off = data_off + data.len() as u32;
    data.extend_from_slice(&(set.len() as u32).to_le_bytes());
    for o in item_offs {
        data.extend_from_slice(&o.to_le_bytes());
    }
    Ok(set_off)
}

fn write_annotation_set_ref_list(
    data: &mut Vec<u8>,
    data_off: u32,
    sets: &[Vec<AnnotationItem>],
) -> Result<u32> {
    let mut offs = Vec::with_capacity(sets.len());
    for set in sets {
        offs.push(write_annotation_set(data, data_off, set)?);
    }
    align4(data);
    let list_off = data_off + data.len() as u32;
    data.extend_from_slice(&(sets.len() as u32).to_le_bytes());
    for o in offs {
        data.extend_from_slice(&o.to_le_bytes());
    }
    Ok(list_off)
}

fn write_annotation_item(data: &mut Vec<u8>, data_off: u32, item: &AnnotationItem) -> u32 {
    let off = data_off + data.len() as u32;
    data.push(item.visibility);
    data.extend(encode_encoded_annotation(&item.annotation));
    off
}

fn align4(data: &mut Vec<u8>) {
    while data.len() % 4 != 0 {
        data.push(0);
    }
}

pub fn visibility_name(v: u8) -> &'static str {
    match v {
        VISIBILITY_BUILD => "build",
        VISIBILITY_RUNTIME => "runtime",
        VISIBILITY_SYSTEM => "system",
        _ => "runtime",
    }
}

pub fn parse_visibility(s: &str) -> u8 {
    match s {
        "build" => VISIBILITY_BUILD,
        "system" => VISIBILITY_SYSTEM,
        _ => VISIBILITY_RUNTIME,
    }
}

/// Helper to build a simple runtime annotation with string/int elements by type index.
pub fn simple_annotation(type_idx: u32, elements: Vec<(u32, EncodedValue)>, visibility: u8) -> AnnotationItem {
    AnnotationItem {
        visibility,
        annotation: EncodedAnnotation { type_idx, elements },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dex::encoded_value::EncodedValue;

    #[test]
    fn write_parse_class_annotation() {
        let mut data = Vec::new();
        let data_off = 0u32;
        let dir = AnnotationsDirectory {
            class_annotations: vec![simple_annotation(
                1,
                vec![(2, EncodedValue::Int(7))],
                VISIBILITY_RUNTIME,
            )],
            ..Default::default()
        };
        let off = write_annotations_directory(&mut data, data_off, &dir).unwrap();
        assert_ne!(off, 0);
        let parsed = parse_annotations_directory(&data, off).unwrap();
        assert_eq!(parsed.class_annotations.len(), 1);
        assert_eq!(parsed.class_annotations[0].annotation.type_idx, 1);
        assert_eq!(
            parsed.class_annotations[0].annotation.elements[0].1,
            EncodedValue::Int(7)
        );
    }
}
