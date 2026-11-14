//! DEX `encoded_value` / `encoded_array` decode + encode.

use crate::bounds::{ensure_count_fits, vec_with_capacity};
use crate::error::{DexError, Result};
use crate::leb128::{read_uleb128, write_uleb128};

/// DEX encoded_value kinds (value_type nibble).
#[derive(Clone, Debug, PartialEq)]
pub enum EncodedValue {
    Byte(i8),
    Short(i16),
    Char(u16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    MethodType(u32),
    MethodHandle(u32),
    String(u32),
    Type(u32),
    Field(u32),
    Method(u32),
    Enum(u32),
    Array(Vec<EncodedValue>),
    Annotation(EncodedAnnotation),
    Null,
    Boolean(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncodedAnnotation {
    pub type_idx: u32,
    pub elements: Vec<(u32, EncodedValue)>, // (name_idx, value)
}

/// Decode one encoded_value; returns (value, next offset).
pub fn decode_encoded_value(data: &[u8], mut pos: usize) -> Result<(EncodedValue, usize)> {
    if pos >= data.len() {
        return Err(DexError::Truncated("encoded_value".into()));
    }
    let header = data[pos];
    pos += 1;
    let value_type = header & 0x1f;
    let value_arg = (header >> 5) as usize;
    match value_type {
        0x00 => {
            let (v, np) = read_signed(data, pos, value_arg, 1)?;
            Ok((EncodedValue::Byte(v as i8), np))
        }
        0x02 => {
            let (v, np) = read_signed(data, pos, value_arg, 2)?;
            Ok((EncodedValue::Short(v as i16), np))
        }
        0x03 => {
            let (v, np) = read_unsigned(data, pos, value_arg, 2)?;
            Ok((EncodedValue::Char(v as u16), np))
        }
        0x04 => {
            let (v, np) = read_signed(data, pos, value_arg, 4)?;
            Ok((EncodedValue::Int(v as i32), np))
        }
        0x06 => {
            let (v, np) = read_signed(data, pos, value_arg, 8)?;
            Ok((EncodedValue::Long(v), np))
        }
        0x10 => {
            let (bits, np) = read_unsigned(data, pos, value_arg, 4)?;
            Ok((EncodedValue::Float(f32::from_bits(bits as u32)), np))
        }
        0x11 => {
            let (bits, np) = read_unsigned(data, pos, value_arg, 8)?;
            Ok((EncodedValue::Double(f64::from_bits(bits as u64)), np))
        }
        0x15 => {
            let (idx, np) = read_unsigned(data, pos, value_arg, 4)?;
            Ok((EncodedValue::MethodType(idx as u32), np))
        }
        0x16 => {
            let (idx, np) = read_unsigned(data, pos, value_arg, 4)?;
            Ok((EncodedValue::MethodHandle(idx as u32), np))
        }
        0x17 => {
            let (idx, np) = read_unsigned(data, pos, value_arg, 4)?;
            Ok((EncodedValue::String(idx as u32), np))
        }
        0x18 => {
            let (idx, np) = read_unsigned(data, pos, value_arg, 4)?;
            Ok((EncodedValue::Type(idx as u32), np))
        }
        0x19 => {
            let (idx, np) = read_unsigned(data, pos, value_arg, 4)?;
            Ok((EncodedValue::Field(idx as u32), np))
        }
        0x1a => {
            let (idx, np) = read_unsigned(data, pos, value_arg, 4)?;
            Ok((EncodedValue::Method(idx as u32), np))
        }
        0x1b => {
            let (idx, np) = read_unsigned(data, pos, value_arg, 4)?;
            Ok((EncodedValue::Enum(idx as u32), np))
        }
        0x1c => {
            let (arr, np) = decode_encoded_array(data, pos)?;
            Ok((EncodedValue::Array(arr), np))
        }
        0x1d => {
            let (ann, np) = decode_encoded_annotation(data, pos)?;
            Ok((EncodedValue::Annotation(ann), np))
        }
        0x1e => Ok((EncodedValue::Null, pos)),
        0x1f => Ok((EncodedValue::Boolean(value_arg != 0), pos)),
        _ => Err(DexError::Parse(format!(
            "unknown encoded_value type 0x{value_type:02x}"
        ))),
    }
}

pub fn decode_encoded_array(data: &[u8], mut pos: usize) -> Result<(Vec<EncodedValue>, usize)> {
    let (size, n) =
        read_uleb128(data, pos).ok_or(DexError::Truncated("encoded_array size".into()))?;
    pos += n;
    let size = size as usize;
    ensure_count_fits(size, data.len().saturating_sub(pos), "encoded_array")?;
    let mut out = vec_with_capacity(size, "encoded_array")?;
    for _ in 0..size {
        let (v, np) = decode_encoded_value(data, pos)?;
        pos = np;
        out.push(v);
    }
    Ok((out, pos))
}

pub fn decode_encoded_annotation(data: &[u8], mut pos: usize) -> Result<(EncodedAnnotation, usize)> {
    let (type_idx, n) =
        read_uleb128(data, pos).ok_or(DexError::Truncated("annotation type".into()))?;
    pos += n;
    let (size, n) =
        read_uleb128(data, pos).ok_or(DexError::Truncated("annotation size".into()))?;
    pos += n;
    let size = size as usize;
    ensure_count_fits(size, data.len().saturating_sub(pos), "annotation elements")?;
    let mut elements = vec_with_capacity(size, "annotation elements")?;
    for _ in 0..size {
        let (name_idx, n) =
            read_uleb128(data, pos).ok_or(DexError::Truncated("annotation name".into()))?;
        pos += n;
        let (val, np) = decode_encoded_value(data, pos)?;
        pos = np;
        elements.push((name_idx, val));
    }
    Ok((
        EncodedAnnotation {
            type_idx,
            elements,
        },
        pos,
    ))
}

/// Encode one encoded_value to bytes.
pub fn encode_encoded_value(v: &EncodedValue) -> Vec<u8> {
    match v {
        EncodedValue::Byte(x) => encode_integral(0x00, *x as i64, 1),
        EncodedValue::Short(x) => encode_integral(0x02, *x as i64, 2),
        EncodedValue::Char(x) => encode_unsigned(0x03, *x as u64, 2),
        EncodedValue::Int(x) => encode_integral(0x04, *x as i64, 4),
        EncodedValue::Long(x) => encode_integral(0x06, *x, 8),
        EncodedValue::Float(x) => encode_unsigned(0x10, x.to_bits() as u64, 4),
        EncodedValue::Double(x) => encode_unsigned(0x11, x.to_bits(), 8),
        EncodedValue::MethodType(i) => encode_unsigned(0x15, *i as u64, 4),
        EncodedValue::MethodHandle(i) => encode_unsigned(0x16, *i as u64, 4),
        EncodedValue::String(i) => encode_unsigned(0x17, *i as u64, 4),
        EncodedValue::Type(i) => encode_unsigned(0x18, *i as u64, 4),
        EncodedValue::Field(i) => encode_unsigned(0x19, *i as u64, 4),
        EncodedValue::Method(i) => encode_unsigned(0x1a, *i as u64, 4),
        EncodedValue::Enum(i) => encode_unsigned(0x1b, *i as u64, 4),
        EncodedValue::Array(arr) => {
            let mut out = vec![0x1c]; // value_arg=0
            out.extend(encode_encoded_array(arr));
            out
        }
        EncodedValue::Annotation(a) => {
            let mut out = vec![0x1d];
            out.extend(encode_encoded_annotation(a));
            out
        }
        EncodedValue::Null => vec![0x1e],
        EncodedValue::Boolean(b) => vec![0x1f | if *b { 1 << 5 } else { 0 }],
    }
}

pub fn encode_encoded_array(values: &[EncodedValue]) -> Vec<u8> {
    let mut out = write_uleb128(values.len() as u32);
    for v in values {
        out.extend(encode_encoded_value(v));
    }
    out
}

pub fn encode_encoded_annotation(a: &EncodedAnnotation) -> Vec<u8> {
    let mut out = write_uleb128(a.type_idx);
    out.extend(write_uleb128(a.elements.len() as u32));
    for (name, val) in &a.elements {
        out.extend(write_uleb128(*name));
        out.extend(encode_encoded_value(val));
    }
    out
}

fn encode_integral(value_type: u8, value: i64, max_bytes: usize) -> Vec<u8> {
    let mut bytes = value.to_le_bytes().to_vec();
    bytes.truncate(max_bytes);
    // Trim high sign-extension bytes while preserving sign bit requirement
    while bytes.len() > 1 {
        let last = *bytes.last().unwrap();
        let prev = bytes[bytes.len() - 2];
        let sign = (prev & 0x80) != 0;
        if (sign && last == 0xff) || (!sign && last == 0x00) {
            bytes.pop();
        } else {
            break;
        }
    }
    let arg = (bytes.len() - 1) as u8;
    let mut out = vec![value_type | (arg << 5)];
    out.extend_from_slice(&bytes);
    out
}

fn encode_unsigned(value_type: u8, value: u64, max_bytes: usize) -> Vec<u8> {
    let mut bytes = value.to_le_bytes().to_vec();
    bytes.truncate(max_bytes);
    while bytes.len() > 1 && *bytes.last().unwrap() == 0 {
        bytes.pop();
    }
    let arg = (bytes.len() - 1) as u8;
    let mut out = vec![value_type | (arg << 5)];
    out.extend_from_slice(&bytes);
    out
}

fn read_signed(data: &[u8], pos: usize, value_arg: usize, max: usize) -> Result<(i64, usize)> {
    let n = value_arg + 1;
    if n > max || pos + n > data.len() {
        return Err(DexError::Truncated("encoded signed".into()));
    }
    let mut buf = [0u8; 8];
    buf[..n].copy_from_slice(&data[pos..pos + n]);
    // sign-extend
    if n < 8 && (buf[n - 1] & 0x80) != 0 {
        for b in buf.iter_mut().skip(n) {
            *b = 0xff;
        }
    }
    Ok((i64::from_le_bytes(buf), pos + n))
}

fn read_unsigned(data: &[u8], pos: usize, value_arg: usize, max: usize) -> Result<(u64, usize)> {
    let n = value_arg + 1;
    if n > max || pos + n > data.len() {
        return Err(DexError::Truncated("encoded unsigned".into()));
    }
    let mut buf = [0u8; 8];
    buf[..n].copy_from_slice(&data[pos..pos + n]);
    Ok((u64::from_le_bytes(buf), pos + n))
}

/// Parse a dex-txt `.value` literal into an EncodedValue using string/type indices from callbacks.
pub fn parse_value_literal(
    lit: &str,
    intern_string: &mut dyn FnMut(&str) -> u32,
    intern_type: &mut dyn FnMut(&str) -> u32,
) -> Result<EncodedValue> {
    let lit = lit.trim();
    if lit == "null" {
        return Ok(EncodedValue::Null);
    }
    if lit == "true" {
        return Ok(EncodedValue::Boolean(true));
    }
    if lit == "false" {
        return Ok(EncodedValue::Boolean(false));
    }
    if lit.starts_with('"') {
        let inner = unquote_string_literal(lit)?;
        return Ok(EncodedValue::String(intern_string(&inner)));
    }
    if is_type_descriptor(lit) {
        return Ok(EncodedValue::Type(intern_type(lit)));
    }
    if let Some(rest) = lit.strip_suffix('L').or_else(|| lit.strip_suffix('l')) {
        if let Ok(v) = rest.parse::<i64>() {
            return Ok(EncodedValue::Long(v));
        }
    }
    if let Some(rest) = lit.strip_suffix('f').or_else(|| lit.strip_suffix('F')) {
        if let Ok(v) = rest.parse::<f32>() {
            return Ok(EncodedValue::Float(v));
        }
    }
    if let Some(rest) = lit.strip_suffix('d').or_else(|| lit.strip_suffix('D')) {
        if let Ok(v) = rest.parse::<f64>() {
            return Ok(EncodedValue::Double(v));
        }
    }
    if lit.starts_with('{') && lit.ends_with('}') {
        let inner = &lit[1..lit.len() - 1];
        let mut elems = Vec::new();
        for part in split_array_elems(inner) {
            elems.push(parse_value_literal(part, intern_string, intern_type)?);
        }
        return Ok(EncodedValue::Array(elems));
    }
    // Indexed forms (legacy emit) — prefer symbolic refs when editing.
    if let Some(idx) = lit.strip_prefix("method@").and_then(|s| s.parse().ok()) {
        return Ok(EncodedValue::Method(idx));
    }
    if let Some(idx) = lit.strip_prefix("field@").and_then(|s| s.parse().ok()) {
        return Ok(EncodedValue::Field(idx));
    }
    if let Some(idx) = lit.strip_prefix("enum@").and_then(|s| s.parse().ok()) {
        return Ok(EncodedValue::Enum(idx));
    }
    if let Some(idx) = lit.strip_prefix("type@").and_then(|s| s.parse().ok()) {
        return Ok(EncodedValue::Type(idx));
    }
    if let Some(idx) = lit.strip_prefix("method_type@").and_then(|s| s.parse().ok()) {
        return Ok(EncodedValue::MethodType(idx));
    }
    if let Some(idx) = lit.strip_prefix("method_handle@").and_then(|s| s.parse().ok()) {
        return Ok(EncodedValue::MethodHandle(idx));
    }
    if let Some(rest) = lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
        if let Ok(v) = i32::from_str_radix(rest, 16) {
            return Ok(EncodedValue::Int(v));
        }
    }
    if let Ok(v) = lit.parse::<i32>() {
        return Ok(EncodedValue::Int(v));
    }
    Err(DexError::Parse(format!("unsupported .value literal: {lit}")))
}

/// Escape a string for use inside a dex-txt `"..."` literal.
pub fn escape_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // ASCII/C1 controls and Unicode line-breaks must stay escaped so dex-txt
            // stays single-line and so `\"` inside binary strings is unambiguous.
            c if (c as u32) < 0x20
                || (0x7f..=0x9f).contains(&(c as u32))
                || c == '\u{2028}'
                || c == '\u{2029}' =>
            {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn is_type_descriptor(s: &str) -> bool {
    matches!(s, "Z" | "B" | "S" | "C" | "I" | "J" | "F" | "D" | "V")
        || s.starts_with('[')
        || (s.starts_with('L') && s.ends_with(';'))
}

/// Unescape dex-txt string content (no surrounding quotes).
pub fn unescape_string_content(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(n) = chars.next() else {
            out.push('\\');
            break;
        };
        match n {
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'x' => {
                let mut hex = String::new();
                for _ in 0..2 {
                    let Some(h) = chars.next() else {
                        return Err(DexError::Parse("bad \\x escape".into()));
                    };
                    hex.push(h);
                }
                let code = u8::from_str_radix(&hex, 16)
                    .map_err(|_| DexError::Parse(format!("bad \\x escape: \\x{hex}")))?;
                out.push(code as char);
            }
            'u' => {
                let mut hex = String::new();
                for _ in 0..4 {
                    let Some(h) = chars.next() else {
                        return Err(DexError::Parse("bad \\u escape".into()));
                    };
                    hex.push(h);
                }
                let code = u32::from_str_radix(&hex, 16)
                    .map_err(|_| DexError::Parse(format!("bad \\u escape: \\u{hex}")))?;
                out.push(
                    char::from_u32(code)
                        .ok_or_else(|| DexError::Parse(format!("invalid unicode \\u{hex}")))?,
                );
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// Unquote a dex-txt `"..."` string literal, applying `\\` / `\"` / `\\n` / `\\uXXXX` escapes.
pub fn unquote_string_literal(lit: &str) -> Result<String> {
    let bytes = lit.as_bytes();
    if bytes.first() != Some(&b'"') {
        return Err(DexError::Parse(format!("expected string literal: {lit}")));
    }
    // Find closing quote with escape awareness, then unescape the inner slice.
    let mut chars = lit[1..].chars().peekable();
    let mut inner = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let rest: String = chars.collect();
                if rest.trim().is_empty() {
                    return unescape_string_content(&inner);
                }
                return Err(DexError::Parse(format!(
                    "trailing junk in string literal: {lit}"
                )));
            }
            '\\' => {
                inner.push('\\');
                if let Some(n) = chars.next() {
                    inner.push(n);
                    if n == 'u' {
                        for _ in 0..4 {
                            if let Some(h) = chars.next() {
                                inner.push(h);
                            }
                        }
                    } else if n == 'x' {
                        for _ in 0..2 {
                            if let Some(h) = chars.next() {
                                inner.push(h);
                            }
                        }
                    }
                }
            }
            c => inner.push(c),
        }
    }
    Err(DexError::Parse(format!("unterminated string literal: {lit}")))
}

fn split_array_elems(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escape {
                escape = false;
                continue;
            }
            if b == b'\\' {
                escape = true;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b',' if depth == 0 => {
                let tok = s[start..i].trim();
                if !tok.is_empty() {
                    out.push(tok);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let tok = s[start..].trim();
    if !tok.is_empty() {
        out.push(tok);
    }
    out
}

/// Format EncodedValue for dex-txt `.value` (index-resolved via callbacks).
pub fn format_value_literal(
    v: &EncodedValue,
    get_string: &dyn Fn(u32) -> Option<String>,
    get_type: &dyn Fn(u32) -> Option<String>,
) -> String {
    format_value_literal_full(v, get_string, get_type, &|_| None, &|_| None)
}

/// Format with optional field/method resolvers for symbolic refs.
pub fn format_value_literal_full(
    v: &EncodedValue,
    get_string: &dyn Fn(u32) -> Option<String>,
    get_type: &dyn Fn(u32) -> Option<String>,
    get_field: &dyn Fn(u32) -> Option<String>,
    get_method: &dyn Fn(u32) -> Option<String>,
) -> String {
    match v {
        EncodedValue::Null => "null".into(),
        EncodedValue::Boolean(b) => b.to_string(),
        EncodedValue::Byte(x) => x.to_string(),
        EncodedValue::Short(x) => x.to_string(),
        EncodedValue::Char(x) => (*x as i32).to_string(),
        EncodedValue::Int(x) => x.to_string(),
        EncodedValue::Long(x) => format!("{x}L"),
        EncodedValue::Float(x) => format!("{x}f"),
        EncodedValue::Double(x) => format!("{x}d"),
        EncodedValue::String(i) => {
            let s = get_string(*i).unwrap_or_default();
            format!("\"{}\"", escape_string_literal(&s))
        }
        EncodedValue::Type(i) => get_type(*i).unwrap_or_else(|| format!("type@{i}")),
        EncodedValue::Array(xs) => {
            let parts: Vec<String> = xs
                .iter()
                .map(|x| format_value_literal_full(x, get_string, get_type, get_field, get_method))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        EncodedValue::Annotation(_) => "<annotation>".into(),
        EncodedValue::Field(i) | EncodedValue::Enum(i) => get_field(*i)
            .map(|s| {
                if matches!(v, EncodedValue::Enum(_)) {
                    format!(".enum {s}")
                } else {
                    s
                }
            })
            .unwrap_or_else(|| {
                if matches!(v, EncodedValue::Enum(_)) {
                    format!("enum@{i}")
                } else {
                    format!("field@{i}")
                }
            }),
        EncodedValue::Method(i) => get_method(*i).unwrap_or_else(|| format!("method@{i}")),
        EncodedValue::MethodType(i) => get_type(*i)
            .map(|t| format!(".method_type {t}"))
            .unwrap_or_else(|| format!("method_type@{i}")),
        EncodedValue::MethodHandle(i) => format!("method_handle@{i}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_string_with_embedded_quotes_roundtrip() {
        // Binary Kotlin metadata often contains 0x22 bytes → emitted as \".
        let s = "a\"b,c";
        let lit = format!("{{\"{}\"}}", escape_string_literal(s));
        let mut got = String::new();
        let v = parse_value_literal(
            &lit,
            &mut |x| {
                got = x.to_string();
                0
            },
            &mut |_| 0,
        )
        .unwrap();
        assert_eq!(v, EncodedValue::Array(vec![EncodedValue::String(0)]));
        assert_eq!(got, s);
    }

    #[test]
    fn string_escape_roundtrip_newlines_and_quotes() {
        let original = "a\nb\t\"c\r\u{000b}d\u{85}\u{2028}\u{2029}e";
        let lit = format!("\"{}\"", escape_string_literal(original));
        assert!(!lit.contains('\n'));
        assert!(!lit.chars().any(|c| matches!(c, '\u{85}' | '\u{2028}' | '\u{2029}')));
        let mut interned = String::new();
        let v = parse_value_literal(
            &lit,
            &mut |s| {
                interned = s.to_string();
                0
            },
            &mut |_| 0,
        )
        .unwrap();
        assert_eq!(v, EncodedValue::String(0));
        assert_eq!(interned, original);
    }

    #[test]
    fn roundtrip_int_string_null_bool() {
        for v in [
            EncodedValue::Int(42),
            EncodedValue::Int(-1),
            EncodedValue::String(3),
            EncodedValue::Null,
            EncodedValue::Boolean(true),
            EncodedValue::Boolean(false),
            EncodedValue::Long(99),
            EncodedValue::Array(vec![EncodedValue::Int(1), EncodedValue::Int(2)]),
        ] {
            let bytes = encode_encoded_value(&v);
            let (decoded, end) = decode_encoded_value(&bytes, 0).unwrap();
            assert_eq!(end, bytes.len());
            assert_eq!(decoded, v);
        }
    }
}
