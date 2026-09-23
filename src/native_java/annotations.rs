use super::{Output, java_type, strings};
use crate::{
    engine::DecompiledCode,
    native_dex::{DexAnnotation, DexAnnotationSet, DexClass, DexValue},
};
use anyhow::{Context, Result, bail};

fn type_name(class: &DexClass, index: u32) -> Result<String> {
    java_type(
        class
            .symbols
            .types
            .get(index as usize)
            .context("Annotation type index")?,
    )
}

fn value(out: &mut Output, class: &DexClass, encoded: &DexValue) -> Result<()> {
    match encoded {
        DexValue::Byte(v) => out.push(&v.to_string()),
        DexValue::Short(v) => out.push(&v.to_string()),
        DexValue::Char(v) => {
            let literal = match *v {
                8 => "'\\b'".into(),
                9 => "'\\t'".into(),
                10 => "'\\n'".into(),
                12 => "'\\f'".into(),
                13 => "'\\r'".into(),
                39 => "'\\\''".into(),
                92 => "'\\\\'".into(),
                0..=31 | 127 => format!("'\\{:03o}'", v),
                _ => format!("'\\u{v:04x}'"),
            };
            out.push(&literal);
        }
        DexValue::Int(v) => out.push(&v.to_string()),
        DexValue::Long(v) => out.push(&format!("{v}L")),
        DexValue::Float(bits) => {
            let v = f32::from_bits(*bits);
            if !v.is_finite() {
                bail!("non-finite float")
            }
            out.push(&format!("{v:?}f"));
        }
        DexValue::Double(bits) => {
            let v = f64::from_bits(*bits);
            if !v.is_finite() {
                bail!("non-finite double")
            }
            out.push(&format!("{v:?}"));
        }
        DexValue::Boolean(v) => out.push(if *v { "true" } else { "false" }),
        DexValue::Null => bail!("null annotation value"),
        DexValue::String(index) => out.push(&strings::literal(
            class
                .symbols
                .strings
                .get(*index as usize)
                .context("Annotation string index")?,
        )?),
        DexValue::Type(index) => {
            let display = type_name(class, *index)?;
            out.reference(
                &display,
                &super::names::label(&class.symbols.types[*index as usize])
                    .unwrap_or_else(|| display.clone()),
            );
            out.push(".class");
        }
        DexValue::Enum(index) => {
            let (owner, field_type, name) = *class
                .symbols
                .fields
                .get(*index as usize)
                .context("Annotation enum index")?;
            let owner_label = super::names::label(
                class
                    .symbols
                    .types
                    .get(owner as usize)
                    .context("Annotation enum owner index")?,
            )
            .context("Annotation enum owner")?;
            let owner = type_name(class, u32::from(owner))?;
            let member = class
                .symbols
                .strings
                .get(name as usize)
                .context("Annotation enum name")?;
            let display_member = super::names::member(member)?;
            let descriptor = class
                .symbols
                .types
                .get(field_type as usize)
                .context("Annotation enum type")?;
            out.reference(&owner, &owner_label);
            out.push(".");
            out.reference(
                &display_member,
                &format!("{owner_label}.{member}:{descriptor}"),
            );
        }
        DexValue::Array(values) => {
            out.push("{");
            for (i, item) in values.iter().enumerate() {
                if i != 0 {
                    out.push(", ");
                }
                value(out, class, item)?;
            }
            out.push("}");
        }
        DexValue::Annotation { type_idx, elements } => {
            annotation_body(out, class, *type_idx, elements)?;
        }
        DexValue::MethodType(_)
        | DexValue::MethodHandle(_)
        | DexValue::Field(_)
        | DexValue::Method(_) => bail!("non-Java annotation value"),
    }
    Ok(())
}

fn annotation_body(
    out: &mut Output,
    class: &DexClass,
    type_idx: u32,
    elements: &[(u32, DexValue)],
) -> Result<()> {
    let ty = type_name(class, type_idx)?;
    out.push("@");
    out.reference(
        &ty,
        &super::names::label(&class.symbols.types[type_idx as usize]).context("Annotation type")?,
    );
    if !elements.is_empty() {
        out.push("(");
        for (i, (name, item)) in elements.iter().enumerate() {
            if i != 0 {
                out.push(", ");
            }
            let name = class
                .symbols
                .strings
                .get(*name as usize)
                .context("Annotation element name")?;
            if !super::identifier(name) {
                bail!("Invalid Java annotation element name");
            }
            out.push(name);
            out.push(" = ");
            value(out, class, item)?;
        }
        out.push(")");
    }
    Ok(())
}

fn render_one(class: &DexClass, annotation: &DexAnnotation) -> Result<DecompiledCode> {
    let mut out = Output::default();
    annotation_body(&mut out, class, annotation.type_idx, &annotation.elements)?;
    Ok(out.finish())
}

pub(super) fn has_unsupported(class: &DexClass, set: Option<&DexAnnotationSet>) -> bool {
    set.is_some_and(|set| {
        set.iter()
            .filter(|annotation| annotation.visibility != 2)
            .any(|annotation| render_one(class, annotation).is_err())
    })
}

pub(super) fn lines(
    class: &DexClass,
    set: Option<&DexAnnotationSet>,
    indent: &str,
) -> DecompiledCode {
    let mut out = Output::default();
    let Some(set) = set else { return out.finish() };
    for annotation in set.iter().filter(|annotation| annotation.visibility != 2) {
        out.push(indent);
        match render_one(class, annotation) {
            Ok(code) => out.append(code),
            Err(_) => {
                let ty = type_name(class, annotation.type_idx).unwrap_or_else(|_| "unknown".into());
                out.push("// DEX annotation retained but cannot be expressed as Java: @");
                out.push(&ty);
            }
        }
        out.push("\n");
    }
    out.finish()
}

pub(super) fn inline(class: &DexClass, set: Option<&DexAnnotationSet>) -> DecompiledCode {
    let mut out = Output::default();
    let Some(set) = set else { return out.finish() };
    for annotation in set.iter().filter(|annotation| annotation.visibility != 2) {
        match render_one(class, annotation) {
            Ok(code) => out.append(code),
            Err(_) => {
                let ty = type_name(class, annotation.type_idx).unwrap_or_else(|_| "unknown".into());
                out.push("/* DEX annotation retained but cannot be expressed as Java: @");
                out.push(&ty);
                out.push(" */");
            }
        }
        out.push(" ");
    }
    out.finish()
}
