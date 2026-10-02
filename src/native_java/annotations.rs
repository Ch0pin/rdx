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

// Annotation elements and their defaults live in class-level system metadata.
fn defaults(class: &DexClass) -> Result<Vec<(u32, &DexValue)>> {
    let mut result = Vec::new();
    let mut found = false;
    if let Some(set) = super::annotation_directory(class).and_then(|d| d.class.as_ref()) {
        for annotation in set.iter() {
            if class
                .symbols
                .types
                .get(annotation.type_idx as usize)
                .map(AsRef::as_ref)
                != Some("Ldalvik/annotation/AnnotationDefault;")
            {
                continue;
            }
            anyhow::ensure!(
                !found && annotation.visibility == 2,
                "Invalid annotation defaults"
            );
            found = true;
            anyhow::ensure!(
                annotation.elements.len() == 1,
                "Invalid annotation defaults wrapper"
            );
            let (name, encoded) = &annotation.elements[0];
            anyhow::ensure!(
                class
                    .symbols
                    .strings
                    .get(*name as usize)
                    .map(String::as_str)
                    == Some("value"),
                "Invalid annotation defaults name"
            );
            let DexValue::Annotation { type_idx, elements } = encoded else {
                bail!("Invalid annotation defaults value")
            };
            anyhow::ensure!(
                class.symbols.types.get(*type_idx as usize) == Some(&class.descriptor),
                "Annotation defaults owner mismatch"
            );
            let mut seen = std::collections::HashSet::new();
            for (name, item) in elements {
                anyhow::ensure!(seen.insert(*name), "Duplicate annotation default");
                let name_text = class
                    .symbols
                    .strings
                    .get(*name as usize)
                    .context("Default element name")?;
                let method = class
                    .methods
                    .iter()
                    .find(|m| m.name.as_ref() == name_text)
                    .context("Unknown annotation default element")?;
                anyhow::ensure!(
                    default_matches(class, &method.return_type, item),
                    "Annotation default type mismatch"
                );
                let mut out = Output::default();
                value(&mut out, class, item)?;
                result.push((*name, item));
            }
        }
    }
    Ok(result)
}

pub(super) fn validate_declaration(class: &DexClass) -> Result<()> {
    if class.access_flags & 0x2000 == 0 {
        return Ok(());
    }
    anyhow::ensure!(
        class.access_flags & 0x600 == 0x600
            && class.superclass.as_deref() == Some("Ljava/lang/Object;")
            && class
                .interfaces
                .iter()
                .map(AsRef::as_ref)
                .eq(["Ljava/lang/annotation/Annotation;"]),
        "Invalid annotation declaration"
    );
    for method in &class.methods {
        anyhow::ensure!(
            method.access_flags == 0x401
                && method.code.is_none()
                && method.parameters.is_empty()
                && method.thrown_types.is_empty()
                && !method.name.starts_with('<')
                && method.return_type.as_ref() != "V"
                && !method.return_type.starts_with("[["),
            "Invalid annotation element"
        );
    }
    defaults(class)?;
    Ok(())
}

pub(super) fn default_value(
    class: &DexClass,
    method: &crate::native_dex::DexMethod,
) -> Result<DecompiledCode> {
    let mut out = Output::default();
    for (name, item) in defaults(class)? {
        if class.symbols.strings[name as usize] == method.name.as_ref() {
            out.push(" default ");
            value(&mut out, class, item)?;
        }
    }
    Ok(out.finish())
}

fn default_matches(class: &DexClass, ty: &str, item: &DexValue) -> bool {
    if let Some(element) = ty.strip_prefix('[') {
        return matches!(item, DexValue::Array(items) if items.iter().all(|item| default_matches(class, element, item)));
    }
    match (ty, item) {
        ("Z", DexValue::Boolean(_))
        | ("B", DexValue::Byte(_))
        | ("S", DexValue::Short(_))
        | ("C", DexValue::Char(_))
        | ("I", DexValue::Int(_))
        | ("J", DexValue::Long(_))
        | ("F", DexValue::Float(_))
        | ("D", DexValue::Double(_))
        | ("Ljava/lang/String;", DexValue::String(_))
        | ("Ljava/lang/Class;", DexValue::Type(_)) => true,
        (_, DexValue::Enum(index)) => {
            class
                .symbols
                .fields
                .get(*index as usize)
                .is_some_and(|(owner, field_type, _)| {
                    class
                        .symbols
                        .types
                        .get(*owner as usize)
                        .is_some_and(|t| t.as_ref() == ty)
                        && class
                            .symbols
                            .types
                            .get(*field_type as usize)
                            .is_some_and(|t| t.as_ref() == ty)
                })
        }
        (_, DexValue::Annotation { type_idx, .. }) => class
            .symbols
            .types
            .get(*type_idx as usize)
            .is_some_and(|t| t.as_ref() == ty),
        _ => false,
    }
}
