//! Bounded decoding of original DEX annotation values for native projections.
use crate::{
    native_dex::{DexSymbols, DexValue},
    native_engine::disassembly,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

fn value(v: &DexValue, s: &DexSymbols, remaining: &mut usize) -> Result<Value> {
    ensure!(
        *remaining > 0,
        "ANNOTATION_VALUE_LIMIT: nested value exceeds response budget"
    );
    *remaining -= 1;
    if let DexValue::String(index) = v {
        let bytes = s
            .strings
            .get(*index as usize)
            .context("Invalid annotation string")?
            .len();
        ensure!(
            bytes <= *remaining,
            "ANNOTATION_VALUE_LIMIT: string exceeds response budget"
        );
        *remaining -= bytes;
    }
    let (kind, data) = match v {
        DexValue::Byte(v) => ("byte", json!(v)),
        DexValue::Short(v) => ("short", json!(v)),
        DexValue::Char(v) => ("char", json!(v)),
        DexValue::Int(v) => ("int", json!(v)),
        DexValue::Long(v) => ("long", json!(v.to_string())),
        DexValue::Float(v) => ("float_bits", json!(format!("0x{v:08x}"))),
        DexValue::Double(v) => ("double_bits", json!(format!("0x{v:016x}"))),
        DexValue::Boolean(v) => ("boolean", json!(v)),
        DexValue::Null => ("null", Value::Null),
        DexValue::String(i) => (
            "string",
            json!(
                s.strings
                    .get(*i as usize)
                    .context("Invalid annotation string")?
            ),
        ),
        DexValue::Type(i) => (
            "type",
            json!(
                s.types
                    .get(*i as usize)
                    .context("Invalid annotation type")?
                    .as_ref()
            ),
        ),
        DexValue::Field(i) | DexValue::Enum(i) => (
            if matches!(v, DexValue::Enum(_)) {
                "enum"
            } else {
                "field"
            },
            json!(disassembly::symbol(s, 'f', *i as usize).context("Invalid annotation field")?),
        ),
        DexValue::Method(i) => (
            "method",
            json!(disassembly::symbol(s, 'm', *i as usize).context("Invalid annotation method")?),
        ),
        DexValue::MethodType(i) => (
            "method_type",
            json!(
                s.protos
                    .get(*i as usize)
                    .map(|(r, p)| format!("({}){}", p.join(""), r))
                    .context("Invalid annotation prototype")?
            ),
        ),
        DexValue::MethodHandle(i) => ("method_handle_index", json!(i)),
        DexValue::Array(v) => (
            "array",
            Value::Array(
                v.iter()
                    .map(|v| value(v, s, remaining))
                    .collect::<Result<_>>()?,
            ),
        ),
        DexValue::Annotation { type_idx, elements } => (
            "annotation",
            json!({"type":s.types.get(*type_idx as usize).context("Invalid nested annotation type")?.as_ref(),"values":elements_value(elements,s,remaining)?}),
        ),
    };
    // Charge composite values too: this conservatively bounds symbol text,
    // escaped strings and array/object overhead before building the next value.
    let bytes = serde_json::to_vec(&data)?.len();
    ensure!(
        bytes <= *remaining,
        "ANNOTATION_VALUE_LIMIT: decoded value exceeds response budget"
    );
    *remaining -= bytes;
    Ok(json!({"kind":kind,"value":data}))
}
pub(crate) fn elements_value(
    elements: &[(u32, DexValue)],
    s: &DexSymbols,
    remaining: &mut usize,
) -> Result<Value> {
    let mut out = serde_json::Map::new();
    for (i, v) in elements {
        let name = s
            .strings
            .get(*i as usize)
            .context("Invalid annotation name")?;
        ensure!(
            name.len() <= *remaining,
            "ANNOTATION_VALUE_LIMIT: names exceed response budget"
        );
        *remaining -= name.len();
        out.insert(name.clone(), value(v, s, remaining)?);
    }
    Ok(Value::Object(out))
}
