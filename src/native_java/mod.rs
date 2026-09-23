//! Conservative native Java reconstruction with exact emitted-source mappings.
mod annotations;
mod condition_cleanup;
mod display_names;
mod liveness;
mod method;
mod names;
mod readable;
mod strings;
mod varargs_cleanup;
use crate::{
    engine::{CodeDefinition, CodeLink, DecompiledCode},
    native_dex::{DexClass, DexField, DexMethod, DexValue},
};
use anyhow::{Result, bail, ensure};

pub(crate) fn java_type(descriptor: &str) -> Result<String> {
    let dimensions = descriptor.bytes().take_while(|b| *b == b'[').count();
    ensure!(dimensions <= 255, "Array dimension limit exceeded");
    let descriptor = &descriptor[dimensions..];
    ensure!(dimensions == 0 || descriptor != "V", "Array of void");
    let primitive = match descriptor {
        "V" => "void",
        "Z" => "boolean",
        "B" => "byte",
        "S" => "short",
        "C" => "char",
        "I" => "int",
        "J" => "long",
        "F" => "float",
        "D" => "double",
        _ => "",
    };
    let base = if !primitive.is_empty() {
        primitive.into()
    } else if let Some(name) = descriptor
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
    {
        names::qualified(name, '/')?
    } else {
        bail!("Unsupported Java descriptor {descriptor}");
    };
    Ok(format!("{base}{}", "[]".repeat(dimensions)))
}

#[derive(Default)]
struct Output {
    text: String,
    chars: usize,
    links: Vec<CodeLink>,
    definitions: Vec<CodeDefinition>,
}

fn presentation(code: DecompiledCode, class: &DexClass) -> DecompiledCode {
    let code = varargs_cleanup::simplify(code, class);
    let code = method::readable_code(code);
    let code = condition_cleanup::simplify(code);
    display_names::rename(code)
}
impl Output {
    fn push(&mut self, text: &str) {
        self.chars += text.chars().count();
        self.text.push_str(text);
    }
    fn reference(&mut self, text: &str, symbol: &str) {
        let start = self.chars;
        self.push(text);
        self.links.push(CodeLink {
            start,
            end: self.chars,
            label: symbol.into(),
        });
    }
    fn definition(&mut self, text: &str, name: &str, symbol: &str, kind: &str) {
        let start = self.chars;
        self.reference(text, symbol);
        self.definitions.push(CodeDefinition {
            start,
            end: self.chars,
            kind: kind.into(),
            name: name.into(),
        });
    }
    fn append(&mut self, code: DecompiledCode) {
        let offset = self.chars;
        self.push(&code.source);
        self.links.extend(code.links.into_iter().map(|mut link| {
            link.start += offset;
            link.end += offset;
            link
        }));
        self.definitions
            .extend(code.definitions.into_iter().map(|mut def| {
                def.start += offset;
                def.end += offset;
                def
            }));
    }
    fn finish(self) -> DecompiledCode {
        DecompiledCode {
            source_hash: crate::engine::source_identity(&self.text),
            source: self.text,
            links: self.links,
            definitions: self.definitions,
        }
    }
}

fn annotation_directory(class: &DexClass) -> Option<&crate::native_dex::DexAnnotationDirectory> {
    class
        .symbols
        .annotations
        .get(&class.annotations_offset)
        .map(AsRef::as_ref)
}

fn access(flags: u32) -> Result<&'static str> {
    match flags & 7 {
        0 => Ok(""),
        1 => Ok("public "),
        2 => Ok("private "),
        4 => Ok("protected "),
        _ => bail!("Invalid visibility"),
    }
}

fn render_initializer(name: &str, class: &DexClass, method: &DexMethod) -> Result<DecompiledCode> {
    ensure!(
        method.declaring_type == class.descriptor
            && method.return_type.as_ref() == "V"
            && method.parameters.is_empty()
            && method.thrown_types.is_empty()
            && method.access_flags & 8 != 0
            && method.access_flags & !(8 | 0x1000 | 0x10000) == 0
            && method.code.is_some(),
        "Invalid static initializer"
    );
    let body = method::reconstruct(name, class, method)?;
    // Java initializer blocks cannot contain return statements. Only the single
    // terminal method return can become normal block completion; do not discard
    // branch returns or infer initializer-specific control flow from source text.
    let terminal = "        return;\n";
    let source = body
        .text
        .strip_suffix(terminal)
        .ok_or_else(|| anyhow::anyhow!("Static initializer requires a single terminal return"))?;
    ensure!(
        !source.lines().any(|line| line.trim() == "return;"),
        "Static initializer early returns not reconstructed"
    );
    let chars = source.chars().count();
    ensure!(
        body.links.iter().all(|link| link.end <= chars),
        "Static initializer return has unexpected references"
    );
    let mut out = Output::default();
    out.push("    ");
    out.definition(
        "static",
        "<clinit>",
        &format!("{name}.<clinit>()V"),
        "method",
    );
    out.push(" {\n");
    let offset = out.chars;
    out.push(source);
    out.links.extend(body.links.into_iter().map(|mut link| {
        link.start += offset;
        link.end += offset;
        link
    }));
    out.push("    }\n");
    Ok(out.finish())
}

// Exact inherited framework contracts, used identically by throw validation and
// declaration emission. Only a direct framework parent is accepted: an unknown
// intervening override may have narrowed its checked exception declaration.
// AOSP android-15.0.0_r1 BackupAgent.java:346-347,380-381 and
// BackupAgentHelper.java:64-75 declare these public instance contracts.
// https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-15.0.0_r1/core/java/android/app/backup/BackupAgent.java
pub(super) fn inherited_override_exception(
    class: &DexClass,
    method: &DexMethod,
) -> Option<&'static str> {
    if !method.thrown_types.is_empty()
        || method.declaring_type != class.descriptor
        || method.access_flags & 7 != 1
        || method.access_flags & 8 != 0
        || method.return_type.as_ref() != "V"
        || !matches!(
            class.superclass.as_deref(),
            Some("Landroid/app/backup/BackupAgent;" | "Landroid/app/backup/BackupAgentHelper;")
        )
    {
        return None;
    }
    let expected: &[&str] = match method.name.as_ref() {
        "onRestore" => &[
            "Landroid/app/backup/BackupDataInput;",
            "I",
            "Landroid/os/ParcelFileDescriptor;",
        ],
        "onBackup" => &[
            "Landroid/os/ParcelFileDescriptor;",
            "Landroid/app/backup/BackupDataOutput;",
            "Landroid/os/ParcelFileDescriptor;",
        ],
        _ => return None,
    };
    (method.parameters.len() == expected.len()
        && method
            .parameters
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.as_ref() == *expected))
    .then_some("Ljava/io/IOException;")
}

pub fn render_method(name: &str, class: &DexClass, method: &DexMethod) -> Result<DecompiledCode> {
    if method.name.as_ref() == "<clinit>" {
        return render_initializer(name, class, method);
    }
    let constructor = method.name.as_ref() == "<init>";
    let display_class = names::qualified(name, '.')?;
    let simple = display_class.rsplit('.').next().unwrap_or(&display_class);
    ensure!(
        method.access_flags & !0x31dff == 0,
        "Unsupported method modifiers"
    );
    ensure!(
        !constructor
            || (method.return_type.as_ref() == "V"
                && method.access_flags & !(7 | 0x10000 | 0x1000 | 0x80) == 0),
        "Invalid constructor"
    );
    ensure!(
        method.parameters.iter().all(|p| p.as_ref() != "V"),
        "Void parameter"
    );
    ensure!(
        method.declaring_type == class.descriptor,
        "Method owner mismatch"
    );
    ensure!(
        method.access_flags & 0x400 == 0
            || method.access_flags & (2 | 8 | 0x10 | 0x20 | 0x100 | 0x800 | 0x20000) == 0,
        "Invalid abstract modifiers"
    );
    ensure!(
        method.access_flags & 0x80 == 0
            || method.parameters.last().is_some_and(|p| p.starts_with('[')),
        "Varargs requires a final array parameter"
    );
    let declaration_only = method.access_flags & (0x400 | 0x100) != 0;
    ensure!(
        !declaration_only || method.code.is_none(),
        "Declaration has code"
    );
    let body = if declaration_only {
        None
    } else {
        Some(method::reconstruct(name, class, method)?)
    };
    let mut out = Output::default();
    let method_position = class
        .methods
        .iter()
        .position(|candidate| std::ptr::eq(candidate, method));
    let directory = annotation_directory(class);
    let method_annotations = method_position
        .and_then(|position| directory.and_then(|directory| directory.methods.get(position)))
        .and_then(Option::as_ref);
    out.append(annotations::lines(class, method_annotations, "    "));
    let unsupported_parameter_annotation = method_position
        .and_then(|position| directory.and_then(|directory| directory.parameters.get(position)))
        .is_some_and(|parameters| {
            parameters
                .iter()
                .any(|set| annotations::has_unsupported(class, set.as_ref()))
        });
    if unsupported_parameter_annotation {
        out.push(
            "    // One or more DEX parameter annotations are retained but cannot be expressed as Java.\n",
        );
    }
    if method.access_flags & 0x40 != 0 {
        out.push("    // DEX bridge method\n");
    }
    out.push("    ");
    out.push(access(method.access_flags)?);
    for (flag, keyword) in [
        (8, "static "),
        (0x10, "final "),
        (0x400, "abstract "),
        (0x100, "native "),
        (0x800, "strictfp "),
    ] {
        if method.access_flags & flag != 0 {
            out.push(keyword);
        }
    }
    if method.access_flags & (0x20 | 0x20000) != 0 {
        out.push("synchronized ");
    }
    if !constructor {
        out.push(&java_type(&method.return_type)?);
        out.push(" ");
    }
    let display_name = if constructor {
        simple.into()
    } else {
        names::member(&method.name)?
    };
    let id = format!(
        "{name}.{}({}){}",
        method.name,
        method.parameters.join(""),
        method.return_type
    );
    out.definition(&display_name, &method.name, &id, "method");
    out.push("(");
    for (index, ty) in method.parameters.iter().enumerate() {
        if index > 0 {
            out.push(", ");
        }
        let parameter_annotations = method_position
            .and_then(|position| directory.and_then(|directory| directory.parameters.get(position)))
            .and_then(|parameters| parameters.get(index))
            .and_then(Option::as_ref);
        out.append(annotations::inline(class, parameter_annotations));
        let display = if method.access_flags & 0x80 != 0 && index + 1 == method.parameters.len() {
            format!("{}...", java_type(&ty[1..])?)
        } else {
            java_type(ty)?
        };
        out.push(&format!("{display} p{index}"));
    }
    out.push(")");
    let declared_throws: Vec<&str> = method
        .thrown_types
        .iter()
        .map(AsRef::as_ref)
        .chain(inherited_override_exception(class, method))
        .collect();
    if !declared_throws.is_empty() {
        out.push(" throws ");
        for (index, ty) in declared_throws.iter().enumerate() {
            method::validate_exception_type(class, ty)?;
            ensure!(
                ty.starts_with('L') && ty.ends_with(';'),
                "Throws annotation requires a class type"
            );
            if index != 0 {
                out.push(", ");
            }
            let display = java_type(ty)?;
            out.reference(
                &display,
                &names::label(ty).unwrap_or_else(|| display.clone()),
            );
        }
    }
    if let Some(body) = body {
        out.push(" {\n");
        let offset = out.chars;
        out.push(&body.text);
        out.links.extend(body.links.into_iter().map(|mut link| {
            link.start += offset;
            link.end += offset;
            link
        }));
        out.push("    }\n");
    } else {
        out.push(";\n");
    }
    Ok(out.finish())
}

fn static_initializer_writes(class: &DexClass, field: &DexField) -> Result<bool> {
    for method in class
        .methods
        .iter()
        .filter(|m| m.name.as_ref() == "<clinit>")
    {
        let code = method
            .code
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing static initializer code"))?;
        let words = &code.instructions;
        let mut pc = 0;
        while pc < words.len() {
            let width = crate::native_engine::disassembly::width(words, pc)
                .ok_or_else(|| anyhow::anyhow!("Invalid static initializer instruction"))?;
            if matches!(words[pc] as u8, 0x67..=0x6d) {
                let &(owner, ty, name) = class
                    .symbols
                    .fields
                    .get(words[pc + 1] as usize)
                    .ok_or_else(|| anyhow::anyhow!("Invalid static initializer field"))?;
                if class.symbols.types.get(owner as usize) == Some(&field.declaring_type)
                    && class.symbols.types.get(ty as usize) == Some(&field.field_type)
                    && class
                        .symbols
                        .strings
                        .get(name as usize)
                        .is_some_and(|name| name == field.name.as_ref())
                {
                    return Ok(true);
                }
            }
            pc += width;
        }
    }
    Ok(false)
}

/// Reconstruct a declaration only when its actual encoded initializer is known.
pub fn render_field(name: &str, class: &DexClass, field: &DexField) -> Result<DecompiledCode> {
    ensure!(field.field_type.as_ref() != "V", "Unsupported field type");
    ensure!(
        field.declaring_type == class.descriptor,
        "Field owner mismatch"
    );
    ensure!(
        field.access_flags & !0x10df == 0,
        "Unsupported field modifiers"
    );
    ensure!(
        field.is_static == (field.access_flags & 8 != 0),
        "Field static flag mismatch"
    );
    ensure!(field.access_flags & 0x50 != 0x50, "Final volatile field");
    let index = class
        .fields
        .iter()
        .filter(|f| f.is_static)
        .position(|f| std::ptr::eq(f, field));
    let value = index.and_then(|i| class.static_values.get(i));
    if field.access_flags & 0x10 != 0 {
        ensure!(value.is_some(), "Final field initializer not reconstructed");
        ensure!(
            !static_initializer_writes(class, field)?,
            "Final field also assigned in static initializer"
        );
    }
    let mut out = Output::default();
    let field_annotations = class
        .fields
        .iter()
        .position(|candidate| std::ptr::eq(candidate, field))
        .and_then(|position| annotation_directory(class)?.fields.get(position))
        .and_then(Option::as_ref);
    out.append(annotations::lines(class, field_annotations, "    "));
    out.push("    ");
    out.push(access(field.access_flags)?);
    for (flag, keyword) in [
        (8, "static "),
        (0x10, "final "),
        (0x40, "volatile "),
        (0x80, "transient "),
    ] {
        if field.access_flags & flag != 0 {
            out.push(keyword);
        }
    }
    let display = java_type(&field.field_type)?;
    if let Some(owner) = field
        .field_type
        .trim_start_matches('[')
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
    {
        let owner = owner.replace('/', ".");
        out.reference(display.trim_end_matches("[]"), &owner);
        out.push(&"[]".repeat(field.field_type.bytes().take_while(|b| *b == b'[').count()));
    } else {
        out.push(&display);
    }
    out.push(" ");
    let display_name = names::member(&field.name)?;
    out.definition(
        &display_name,
        &field.name,
        &format!("{name}.{}:{}", field.name, field.field_type),
        "field",
    );
    if let Some(value) = value {
        out.push(" = ");
        match (field.field_type.as_ref(), value) {
            ("B", DexValue::Byte(v)) => out.push(&v.to_string()),
            ("S", DexValue::Short(v)) => out.push(&v.to_string()),
            ("C", DexValue::Char(v)) => out.push(&format!("(char) 0x{v:04x}")),
            ("I", DexValue::Int(v)) => out.push(&v.to_string()),
            ("J", DexValue::Long(v)) => out.push(&format!("{v}L")),
            ("F", DexValue::Float(bits)) => {
                let value = f32::from_bits(*bits);
                ensure!(
                    !value.is_nan(),
                    "NaN encoded bits not reconstructed as Java constants"
                );
                if value.is_infinite() {
                    out.push(if value.is_sign_negative() {
                        "(-1.0f / 0.0f)"
                    } else {
                        "(1.0f / 0.0f)"
                    });
                } else {
                    let literal = format!("{value:?}");
                    ensure!(
                        literal.parse::<f32>()?.to_bits() == *bits,
                        "Float literal changed encoded bits"
                    );
                    out.push(&format!("{literal}f"));
                }
            }
            ("D", DexValue::Double(bits)) => {
                let value = f64::from_bits(*bits);
                ensure!(
                    !value.is_nan(),
                    "NaN encoded bits not reconstructed as Java constants"
                );
                if value.is_infinite() {
                    out.push(if value.is_sign_negative() {
                        "(-1.0d / 0.0d)"
                    } else {
                        "(1.0d / 0.0d)"
                    });
                } else {
                    let literal = format!("{value:?}");
                    ensure!(
                        literal.parse::<f64>()?.to_bits() == *bits,
                        "Double literal changed encoded bits"
                    );
                    out.push(&format!("{literal}d"));
                }
            }
            ("Z", DexValue::Boolean(v)) => out.push(if *v { "true" } else { "false" }),
            (ty, DexValue::Null) if ty.starts_with('L') || ty.starts_with('[') => out.push("null"),
            ("Ljava/lang/String;", DexValue::String(index)) => {
                let value = class
                    .symbols
                    .strings
                    .get(*index as usize)
                    .ok_or_else(|| anyhow::anyhow!("Static string index"))?;
                out.push(&strings::literal(value)?);
            }
            ("Ljava/lang/Class;", DexValue::Type(index)) => {
                let ty = class
                    .symbols
                    .types
                    .get(*index as usize)
                    .ok_or_else(|| anyhow::anyhow!("Static class index"))?;
                let display = java_type(ty)?;
                if let Some(owner) = ty
                    .trim_start_matches('[')
                    .strip_prefix('L')
                    .and_then(|s| s.strip_suffix(';'))
                {
                    let owner = owner.replace('/', ".");
                    out.reference(display.trim_end_matches("[]"), &owner);
                    out.push(&"[]".repeat(ty.bytes().take_while(|b| *b == b'[').count()));
                } else {
                    out.push(&display);
                }
                out.push(".class");
            }
            _ => bail!("Encoded field initializer not reconstructed"),
        }
    }
    out.push(";\n");
    ensure!(
        out.text.len() <= 4 * 1024 * 1024,
        "Field source exceeds output budget"
    );
    Ok(out.finish())
}

fn render_mixed_field_fallback(name: &str, class: &DexClass, field: &DexField) -> DecompiledCode {
    let id = format!("{name}.{}:{}", field.name, field.field_type);
    let encoded_initializer = class
        .fields
        .iter()
        .filter(|candidate| candidate.is_static)
        .position(|candidate| std::ptr::eq(candidate, field))
        .is_some_and(|index| class.static_values.get(index).is_some());
    let declaration = (|| -> Result<DecompiledCode> {
        ensure!(field.field_type.as_ref() != "V", "Unsupported field type");
        ensure!(
            field.declaring_type == class.descriptor,
            "Field owner mismatch"
        );
        ensure!(
            field.access_flags & !0x10df == 0,
            "Unsupported field modifiers"
        );
        ensure!(
            field.is_static == (field.access_flags & 8 != 0),
            "Field static flag mismatch"
        );
        ensure!(field.access_flags & 0x50 != 0x50, "Final volatile field");
        if class.access_flags & 0x200 != 0 {
            ensure!(
                field.access_flags & 0x19 == 0x19 && field.access_flags & !(0x19 | 0x1000) == 0,
                "Illegal interface field modifiers"
            );
        }
        let mut out = Output::default();
        out.push("    ");
        out.push(access(field.access_flags)?);
        for (flag, keyword) in [
            (8, "static "),
            (0x10, "final "),
            (0x40, "volatile "),
            (0x80, "transient "),
        ] {
            if field.access_flags & flag != 0 {
                out.push(keyword);
            }
        }
        let display = java_type(&field.field_type)?;
        if let Some(owner) = field
            .field_type
            .trim_start_matches('[')
            .strip_prefix('L')
            .and_then(|s| s.strip_suffix(';'))
        {
            let owner = owner.replace('/', ".");
            out.reference(display.trim_end_matches("[]"), &owner);
            out.push(&"[]".repeat(field.field_type.bytes().take_while(|b| *b == b'[').count()));
        } else {
            out.push(&display);
        }
        out.push(" ");
        let display_name = names::member(&field.name)?;
        out.definition(&display_name, &field.name, &id, "field");
        out.push(if encoded_initializer {
            "; // Encoded initializer not shown.\n"
        } else {
            ";\n"
        });
        Ok(out.finish())
    })();
    let declaration = declaration.unwrap_or_else(|_| {
        let mut out = Output::default();
        out.push("    // DEX field unavailable: ");
        out.definition(&id, &field.name, &id, "field");
        out.push(&format!(" // access=0x{:x}\n", field.access_flags));
        out.finish()
    });
    let field_annotations = class
        .fields
        .iter()
        .position(|candidate| std::ptr::eq(candidate, field))
        .and_then(|position| annotation_directory(class)?.fields.get(position))
        .and_then(Option::as_ref);
    let mut out = Output::default();
    out.append(annotations::lines(class, field_annotations, "    "));
    out.append(declaration);
    out.finish()
}

fn render_header(name: &str, class: &DexClass) -> Result<Output> {
    let interface = class.access_flags & 0x200 != 0;
    ensure!(
        // ACC_SYNTHETIC is compiler metadata, not a Java source modifier.
        class.access_flags & !0x1611 == 0,
        "Unsupported class modifiers"
    );
    ensure!(
        class.access_flags & 0x410 != 0x410,
        "Class cannot be abstract and final"
    );
    ensure!(
        !interface || class.access_flags & 0x400 != 0,
        "Interface must be abstract"
    );
    let display_class = names::qualified(name, '.')?;
    let (package, simple) = display_class
        .rsplit_once('.')
        .unwrap_or(("", &display_class));
    let mut out = Output::default();
    if !package.is_empty() {
        out.push(&format!("package {package};\n\n"));
    }
    let class_annotations =
        annotation_directory(class).and_then(|directory| directory.class.as_ref());
    out.append(annotations::lines(class, class_annotations, ""));
    out.push(access(class.access_flags)?);
    if !interface && class.access_flags & 0x10 != 0 {
        out.push("final ");
    }
    if !interface && class.access_flags & 0x400 != 0 {
        out.push("abstract ");
    }
    out.push(if interface { "interface " } else { "class " });
    out.definition(simple, name, name, "class");
    if let Some(parent) = &class.superclass {
        ensure!(parent.starts_with('L'), "Invalid superclass type");
        if interface {
            ensure!(
                parent.as_ref() == "Ljava/lang/Object;",
                "Invalid interface superclass"
            );
        } else if parent.as_ref() != "Ljava/lang/Object;" {
            let ty = java_type(parent)?;
            out.push(" extends ");
            out.reference(&ty, &names::label(parent).unwrap());
        }
    } else {
        ensure!(
            !interface && name == "java.lang.Object",
            "Missing superclass"
        );
    }
    if !class.interfaces.is_empty() {
        out.push(if interface {
            " extends "
        } else {
            " implements "
        });
        for (index, ty) in class.interfaces.iter().enumerate() {
            ensure!(ty.starts_with('L'), "Invalid interface type");
            if index > 0 {
                out.push(", ");
            }
            let display = java_type(ty)?;
            out.reference(&display, &names::label(ty).unwrap());
        }
    }
    out.push(" {\n");
    Ok(out)
}

fn interface_method_default(method: &DexMethod) -> Result<bool> {
    ensure!(method.name.as_ref() != "<init>", "Interface constructor");
    if method.name.as_ref() == "<clinit>" {
        return Ok(false);
    }
    ensure!(
        method.access_flags & (4 | 0x10 | 0x20 | 0x100 | 0x20000) == 0,
        "Illegal interface method modifiers"
    );
    if method.access_flags & 0x400 != 0 {
        ensure!(
            method.access_flags & 1 != 0 && method.access_flags & (2 | 8) == 0,
            "Illegal abstract interface method"
        );
        return Ok(false);
    }
    if method.access_flags & 8 != 0 {
        ensure!(
            method.access_flags & (1 | 2) != 0,
            "Static interface method visibility"
        );
        return Ok(false);
    }
    ensure!(
        method.access_flags & (1 | 2) != 0,
        "Interface instance method visibility"
    );
    Ok(method.access_flags & 1 != 0)
}

fn add_default_modifier(mut code: DecompiledCode) -> DecompiledCode {
    let Some(definition) = code.definitions.iter().find(|d| d.kind == "method") else {
        return code;
    };
    let line = code
        .source
        .chars()
        .take(definition.start)
        .collect::<Vec<_>>();
    let at = line.iter().rposition(|c| *c == '\n').map_or(0, |n| n + 1) + 4;
    code = readable::insert(code, at, "default ");
    code
}

fn class_prefix(name: &str, class: &DexClass) -> Result<Output> {
    ensure!(
        class.annotations_offset == 0 || annotation_directory(class).is_some(),
        "Annotations not reconstructed"
    );
    ensure!(
        class.access_flags & !0x611 == 0,
        "Unsupported class modifiers"
    );
    ensure!(
        class.access_flags & 0x410 != 0x410,
        "Class cannot be abstract and final"
    );
    ensure!(
        class.access_flags & 0x400 != 0
            || !class.methods.iter().any(|m| m.access_flags & 0x400 != 0),
        "Abstract method in concrete class"
    );
    let mut signatures = std::collections::HashSet::new();
    ensure!(
        class
            .methods
            .iter()
            .all(|m| signatures.insert((m.name.as_ref(), m.parameters.as_slice()))),
        "Duplicate Java method signatures"
    );
    ensure!(
        class.static_values.len() <= class.fields.iter().filter(|f| f.is_static).count(),
        "Encoded values exceed static fields"
    );
    ensure!(
        class.static_values_offset == 0 || !class.static_values.is_empty(),
        "Unresolved encoded static values"
    );
    let mut fields = std::collections::HashSet::new();
    ensure!(
        class.fields.iter().all(|f| fields.insert(f.name.as_ref())),
        "Duplicate Java fields"
    );
    let interface = class.access_flags & 0x200 != 0;
    if interface {
        ensure!(
            class
                .fields
                .iter()
                .all(|field| field.access_flags & 0x19 == 0x19
                    && field.access_flags & !(0x19 | 0x1000) == 0),
            "Illegal interface field modifiers"
        );
        for method in &class.methods {
            interface_method_default(method)?;
        }
    }
    let mut out = render_header(name, class)?;
    for field in &class.fields {
        out.append(render_field(name, class, field)?);
    }
    Ok(out)
}

fn append_presented_method(
    out: &mut Output,
    class: &DexClass,
    method: &DexMethod,
    code: DecompiledCode,
) -> Result<()> {
    let mut code = presentation(code, class);
    if class.access_flags & 0x200 != 0 && interface_method_default(method)? {
        code = add_default_modifier(code);
    }
    out.append(code);
    Ok(())
}
fn finish_class(name: &str, class: &DexClass, mut out: Output) -> DecompiledCode {
    out.push("}\n");
    let shortened = readable::shorten(name, class, vec![out.finish()]);
    readable::add_imports(
        shortened.codes.into_iter().next().unwrap(),
        &shortened.imports,
    )
}

pub fn render(name: &str, class: &DexClass) -> Result<DecompiledCode> {
    let mut out = class_prefix(name, class)?;
    for method in &class.methods {
        append_presented_method(&mut out, class, method, render_method(name, class, method)?)?;
    }
    Ok(finish_class(name, class, out))
}

/// A failing class keeps its completed member work. Neither successful methods
/// before the failure nor the failed method itself are decompiled a second time.
pub(crate) fn render_mixed(name: &str, class: &DexClass) -> DecompiledCode {
    render_mixed_with(name, class, |method| render_method(name, class, method))
}
fn render_mixed_with(
    name: &str,
    class: &DexClass,
    render_member: impl FnMut(&DexMethod) -> Result<DecompiledCode>,
) -> DecompiledCode {
    render_mixed_controlled(name, class, render_member, &|| false)
        .expect("non-cancellable class rendering")
}

pub(crate) fn render_mixed_cancellable(
    name: &str,
    class: &DexClass,
    cancelled: &impl Fn() -> bool,
) -> Result<DecompiledCode> {
    render_mixed_controlled(
        name,
        class,
        |method| render_method(name, class, method),
        cancelled,
    )
}

fn render_mixed_controlled(
    name: &str,
    class: &DexClass,
    mut render_member: impl FnMut(&DexMethod) -> Result<DecompiledCode>,
    cancelled: &impl Fn() -> bool,
) -> Result<DecompiledCode> {
    ensure!(!cancelled(), "decompilation cancelled");
    let mut attempted = Vec::new();
    if let Ok(mut out) = class_prefix(name, class) {
        for method in &class.methods {
            ensure!(!cancelled(), "decompilation cancelled");
            let result = render_member(method);
            let failed = result.is_err();
            attempted.push(Some(result));
            if failed {
                break;
            }
        }
        if attempted.len() == class.methods.len()
            && attempted
                .iter()
                .all(|result| result.as_ref().unwrap().is_ok())
        {
            for (method, result) in class.methods.iter().zip(attempted) {
                ensure!(!cancelled(), "decompilation cancelled");
                // Prefix validation already checked every interface modifier.
                append_presented_method(&mut out, class, method, result.unwrap().unwrap())
                    .expect("validated interface method");
            }
            ensure!(!cancelled(), "decompilation cancelled");
            return Ok(finish_class(name, class, out));
        }
    }
    ensure!(!cancelled(), "decompilation cancelled");
    let raw = crate::native_engine::disassembly::render(name, class);
    upgrade_methods_controlled(
        name,
        class,
        raw,
        |index, method| {
            attempted
                .get_mut(index)
                .and_then(Option::take)
                .unwrap_or_else(|| render_member(method))
        },
        cancelled,
    )
}

/// Upgrade individual supported fields and methods without hiding remaining DEX code.
/// Each instruction/reference is represented once, so usages are not duplicated.
#[cfg(test)]
pub(crate) fn upgrade_methods(name: &str, class: &DexClass, raw: DecompiledCode) -> DecompiledCode {
    upgrade_methods_with(name, class, raw, |_, method| {
        render_method(name, class, method)
    })
}
#[cfg(test)]
fn upgrade_methods_with(
    name: &str,
    class: &DexClass,
    raw: DecompiledCode,
    render_member: impl FnMut(usize, &DexMethod) -> Result<DecompiledCode>,
) -> DecompiledCode {
    upgrade_methods_controlled(name, class, raw, render_member, &|| false)
        .expect("non-cancellable member rendering")
}

fn upgrade_methods_controlled(
    name: &str,
    class: &DexClass,
    raw: DecompiledCode,
    mut render_member: impl FnMut(usize, &DexMethod) -> Result<DecompiledCode>,
    cancelled: &impl Fn() -> bool,
) -> Result<DecompiledCode> {
    ensure!(!cancelled(), "decompilation cancelled");
    if raw.source.len() > 4 * 1024 * 1024 {
        return Ok(raw);
    }
    let boundaries: Vec<_> = raw
        .source
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(raw.source.len()))
        .collect();
    let mut replacements = Vec::new();
    let mut imports = std::collections::BTreeSet::new();
    for (index, (definition, method)) in raw
        .definitions
        .iter()
        .filter(|d| d.kind == "method")
        .zip(&class.methods)
        .enumerate()
    {
        ensure!(!cancelled(), "decompilation cancelled");
        let byte = boundaries[definition.start];
        let begin = raw.source[..byte].rfind('\n').map_or(0, |n| n + 1);
        let Ok(start) = boundaries.binary_search(&begin) else {
            continue;
        };
        let Ok(mut code) = render_member(index, method) else {
            let annotation = class
                .methods
                .iter()
                .position(|candidate| std::ptr::eq(candidate, method))
                .and_then(|position| annotation_directory(class)?.methods.get(position))
                .and_then(Option::as_ref);
            let prefix = annotations::lines(class, annotation, "");
            if !prefix.source.is_empty() {
                replacements.push((start, start, prefix));
            }
            continue;
        };
        code = presentation(code, class);
        if class.access_flags & 0x200 != 0 {
            let Ok(default) = interface_method_default(method) else {
                continue;
            };
            if default {
                code = add_default_modifier(code);
            }
        }
        let Some(end) = raw.source[byte..]
            .find(".end method\n")
            .map(|n| byte + n + ".end method\n".len())
        else {
            continue;
        };
        let Ok(end) = boundaries.binary_search(&end) else {
            continue;
        };
        replacements.push((start, end, code));
    }
    for (definition, field) in raw
        .definitions
        .iter()
        .filter(|d| d.kind == "field")
        .zip(&class.fields)
    {
        ensure!(!cancelled(), "decompilation cancelled");
        let code = render_field(name, class, field)
            .unwrap_or_else(|_| render_mixed_field_fallback(name, class, field));
        let byte = boundaries[definition.start];
        let begin = raw.source[..byte].rfind('\n').map_or(0, |n| n + 1);
        let Some(end) = raw.source[byte..].find('\n').map(|n| byte + n + 1) else {
            continue;
        };
        let (Ok(start), Ok(end)) = (
            boundaries.binary_search(&begin),
            boundaries.binary_search(&end),
        ) else {
            continue;
        };
        replacements.push((start, end, code));
    }
    ensure!(!cancelled(), "decompilation cancelled");
    replacements.sort_by_key(|(start, _, _)| *start);
    let (bounds, codes): (Vec<_>, Vec<_>) = replacements
        .into_iter()
        .map(|(start, end, code)| ((start, end), code))
        .unzip();
    let header = render_header(name, class).map(Output::finish).ok();
    let (mut out, replacements) = if let Some(header) = header {
        let mut all = Vec::with_capacity(codes.len() + 1);
        all.push(header);
        all.extend(codes);
        let shortened = readable::shorten(name, class, all);
        imports.extend(shortened.imports);
        let mut codes = shortened.codes.into_iter();
        let header = codes.next().unwrap();
        let replacements: Vec<_> = bounds
            .into_iter()
            .zip(codes)
            .map(|((start, end), code)| (start, end, code))
            .collect();
        let mut out = Output::default();
        out.append(header);
        (out, replacements)
    } else {
        let replacements: Vec<_> = bounds
            .into_iter()
            .zip(codes)
            .map(|((start, end), code)| (start, end, code))
            .collect();
        (Output::default(), replacements)
    };
    let java_header = out.chars != 0;
    let field = raw.source.find("\n.field ");
    let method = raw.source.find("\n.method ");
    let header_end = match (field, method) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let mut cursor = if java_header {
        header_end.map_or(boundaries.len() - 1, |byte| {
            raw.source[..byte + 1].chars().count()
        })
    } else {
        0
    };
    let copy = |out: &mut Output, start: usize, end: usize| {
        let offset = out.chars;
        out.push(&raw.source[boundaries[start]..boundaries[end]]);
        out.links.extend(
            raw.links
                .iter()
                .filter(|l| l.start >= start && l.end <= end)
                .cloned()
                .map(|mut l| {
                    l.start = l.start - start + offset;
                    l.end = l.end - start + offset;
                    l
                }),
        );
        out.definitions.extend(
            raw.definitions
                .iter()
                .filter(|d| d.start >= start && d.end <= end)
                .cloned()
                .map(|mut d| {
                    d.start = d.start - start + offset;
                    d.end = d.end - start + offset;
                    d
                }),
        );
    };
    for (start, end, code) in replacements {
        ensure!(!cancelled(), "decompilation cancelled");
        copy(&mut out, cursor, start);
        out.append(code);
        cursor = end;
    }
    copy(&mut out, cursor, boundaries.len() - 1);
    if java_header {
        out.push("}\n");
    }
    ensure!(!cancelled(), "decompilation cancelled");
    Ok(readable::add_imports(out.finish(), &imports))
}

pub(crate) fn identifier(name: &str) -> bool {
    // Until Java identifier/keyword escaping is ported, reject ambiguous source names.
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !matches!(
            name,
            "abstract"
                | "assert"
                | "boolean"
                | "break"
                | "byte"
                | "case"
                | "catch"
                | "char"
                | "class"
                | "const"
                | "continue"
                | "default"
                | "do"
                | "double"
                | "else"
                | "enum"
                | "extends"
                | "final"
                | "finally"
                | "float"
                | "for"
                | "goto"
                | "if"
                | "implements"
                | "import"
                | "instanceof"
                | "int"
                | "interface"
                | "long"
                | "native"
                | "new"
                | "package"
                | "private"
                | "protected"
                | "public"
                | "return"
                | "short"
                | "static"
                | "strictfp"
                | "super"
                | "switch"
                | "synchronized"
                | "this"
                | "throw"
                | "throws"
                | "transient"
                | "try"
                | "void"
                | "volatile"
                | "while"
                | "true"
                | "false"
                | "null"
                | "_"
                | "var"
                | "yield"
                | "record"
                | "sealed"
                | "permits"
        )
}

#[cfg(test)]
mod initializer_tests {
    use super::*;

    #[test]
    fn synthetic_classes_use_java_headers_and_short_types() {
        let mut class = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.access_flags = 0x1011;
        class.interfaces = vec!["Lsample/Callback;".into()];
        let code = render_mixed("sample.Hello", &class);
        assert!(
            code.source
                .contains("public final class Hello implements Callback {"),
            "{}",
            code.source
        );
        assert!(!code.source.contains(".class "));
        assert!(!code.source.contains(".super "));
        assert!(!code.source.contains(".implements "));
        assert!(
            code.definitions
                .iter()
                .any(|d| d.kind == "class" && d.name == "sample.Hello")
        );
    }

    #[test]
    fn interface_header_uses_extends_and_exact_navigation() {
        let mut class = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.access_flags = 0x601;
        class.superclass = Some("Ljava/lang/Object;".into());
        class.interfaces = vec!["Lsample/BaseFeatureConfigurationInterface;".into()];
        let mut out = render_header("sample.ProductFlavourFeatureConfig", &class).unwrap();
        out.push("}\n");
        let code = out.finish();
        assert!(code.source.contains(
            "public interface ProductFlavourFeatureConfig extends sample.BaseFeatureConfigurationInterface {"
        ));
        let class_def = code.definitions.iter().find(|d| d.kind == "class").unwrap();
        assert_eq!(
            code.source
                .chars()
                .skip(class_def.start)
                .take(class_def.end - class_def.start)
                .collect::<String>(),
            "ProductFlavourFeatureConfig"
        );
        let base = code
            .links
            .iter()
            .find(|l| l.label == "sample.BaseFeatureConfigurationInterface")
            .unwrap();
        assert_eq!(
            code.source
                .chars()
                .skip(base.start)
                .take(base.end - base.start)
                .collect::<String>(),
            "sample.BaseFeatureConfigurationInterface"
        );
    }

    #[test]
    fn interface_rules_reject_illegal_members_and_place_default_after_bridge_comment() {
        let mut class = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.methods[0].name = "value".into();
        class.methods[0].access_flags = 0x41;
        assert!(interface_method_default(&class.methods[0]).unwrap());
        let source = "    // DEX bridge method\n    public int value() {\n    }\n";
        let start = source.find("value").unwrap();
        let rendered = add_default_modifier(DecompiledCode {
            source: source.into(),
            links: Vec::new(),
            definitions: vec![CodeDefinition {
                start,
                end: start + "value".len(),
                kind: "method".into(),
                name: "value".into(),
            }],
            source_hash: String::new(),
        });
        assert!(
            rendered
                .source
                .starts_with("    // DEX bridge method\n    default public ")
        );
        class.methods[0].access_flags = 4;
        assert!(interface_method_default(&class.methods[0]).is_err());
        class.methods[0].access_flags = 0x11;
        assert!(interface_method_default(&class.methods[0]).is_err());

        class.access_flags = 0x601;
        class.superclass = Some("Ljava/lang/Object;".into());
        class.methods.clear();
        class.fields = vec![DexField {
            declaring_type: class.descriptor.clone(),
            name: "bad".into(),
            field_type: "I".into(),
            access_flags: 1,
            is_static: false,
        }];
        class.static_values.clear();
        assert!(render("sample.Contract", &class).is_err());
    }

    #[test]
    fn mixed_field_fallback_is_java_shaped_without_inventing_an_initializer() {
        let mut class = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.fields = vec![DexField {
            declaring_type: class.descriptor.clone(),
            name: "CONFIG".into(),
            field_type: "Ljava/lang/String;".into(),
            access_flags: 0x19,
            is_static: true,
        }];
        class.static_values.clear();
        let code = render_mixed_field_fallback("sample.Hello", &class, &class.fields[0]);
        assert_eq!(
            code.source,
            "    public static final java.lang.String CONFIG;\n"
        );
        let definition = &code.definitions[0];
        assert_eq!(
            code.source
                .chars()
                .skip(definition.start)
                .take(definition.end - definition.start)
                .collect::<String>(),
            "CONFIG"
        );
        assert_eq!(
            code.links
                .iter()
                .find(|link| link.label == "sample.Hello.CONFIG:Ljava/lang/String;")
                .map(|link| (link.start, link.end)),
            Some((definition.start, definition.end))
        );

        class.static_values = vec![DexValue::Boolean(true)];
        let code = render_mixed_field_fallback("sample.Hello", &class, &class.fields[0]);
        assert!(
            code.source
                .ends_with("CONFIG; // Encoded initializer not shown.\n")
        );
        let definition = &code.definitions[0];
        assert_eq!(
            code.links
                .iter()
                .find(|link| link.label == "sample.Hello.CONFIG:Ljava/lang/String;")
                .map(|link| (link.start, link.end)),
            Some((definition.start, definition.end))
        );
    }

    #[test]
    fn mixed_field_upgrade_keeps_unsupported_method_and_unicode_offsets() {
        let mut class = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.fields = vec![DexField {
            declaring_type: class.descriptor.clone(),
            name: "TEXT".into(),
            field_type: "Ljava/lang/String;".into(),
            access_flags: 0x19,
            is_static: true,
        }];
        class.symbols = std::sync::Arc::new(crate::native_dex::DexSymbols {
            strings: vec!["é😀".into()],
            ..Default::default()
        });
        class.static_values = vec![DexValue::String(0)];
        class.methods[0].code.as_mut().unwrap().instructions = vec![0x0028];
        let field_id = "sample.Hello.TEXT:Ljava/lang/String;";
        let method_id = "sample.Hello.answer()I";
        let source = format!(
            "// DEX\n.field {field_id} // access=0x19\n.method {method_id}\n    goto +0\n.end method\n# opaque metadata\n"
        );
        let mut definitions = Vec::new();
        let mut links = Vec::new();
        for (id, name, kind) in [(field_id, "TEXT", "field"), (method_id, "answer", "method")] {
            let start = source[..source.find(id).unwrap()].chars().count();
            let end = start + id.chars().count();
            definitions.push(CodeDefinition {
                start,
                end,
                name: name.into(),
                kind: kind.into(),
            });
            links.push(CodeLink {
                start,
                end,
                label: id.into(),
            });
        }
        let raw = DecompiledCode {
            source,
            source_hash: String::new(),
            links,
            definitions,
        };
        let code = upgrade_methods("sample.Hello", &class, raw);
        assert!(code.source.contains("TEXT = \"é😀\";"));
        assert!(!code.source.contains("import java.lang.String;"));
        assert!(code.source.contains("String TEXT"));
        assert!(!code.source.contains(".field"));
        assert!(code.source.contains(".method sample.Hello.answer()I"));
        assert!(code.source.contains("goto +0"));
        assert!(code.source.contains("# opaque metadata"));
        for link in &code.links {
            let text: String = code
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect();
            assert!(
                text == "Hello" || text == "TEXT" || text == "String" || text == method_id,
                "{text}"
            );
        }
    }

    #[test]
    fn mixed_initializer_replacement_keeps_original_symbol_and_opaque_metadata() {
        let mut class = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let method = &mut class.methods[0];
        method.name = "<clinit>".into();
        method.return_type = "V".into();
        method.access_flags = 0x10008;
        method.code.as_mut().unwrap().instructions = vec![0x000e];
        class.annotations_offset = 1;
        let symbol = "sample.Hello.<clinit>()V";
        let source = format!(
            "// DEX\n.method static {symbol}\n    return-void\n.end method\n# opaque metadata\n"
        );
        let start = source.find(symbol).unwrap();
        let raw = DecompiledCode {
            source,
            source_hash: String::new(),
            links: vec![CodeLink {
                start,
                end: start + symbol.len(),
                label: symbol.into(),
            }],
            definitions: vec![CodeDefinition {
                start,
                end: start + symbol.len(),
                name: "<clinit>".into(),
                kind: "method".into(),
            }],
        };
        let code = upgrade_methods("sample.Hello", &class, raw);
        assert!(code.source.contains("static {\n    }"));
        assert!(code.source.contains("# opaque metadata"));
        assert!(!code.source.contains(".method"));
        assert!(!code.source.contains("return-void"));
        let definition = code
            .definitions
            .iter()
            .find(|definition| definition.kind == "method")
            .unwrap();
        let declaration = code
            .source
            .chars()
            .skip(definition.start)
            .take(definition.end - definition.start)
            .collect::<String>();
        assert_eq!(declaration, "static");
        let link = code.links.iter().find(|link| link.label == symbol).unwrap();
        assert_eq!(link.start, definition.start);
        assert_eq!(link.end, definition.end);
    }
}

#[cfg(test)]
mod single_pass_tests {
    use super::*;
    fn legacy(name: &str, class: &DexClass) -> DecompiledCode {
        render(name, class).unwrap_or_else(|_| {
            upgrade_methods(
                name,
                class,
                crate::native_engine::disassembly::render(name, class),
            )
        })
    }
    fn assert_same(left: &DecompiledCode, right: &DecompiledCode) {
        assert_eq!(left.source, right.source);
        assert_eq!(left.source_hash, right.source_hash);
        assert_eq!(
            left.links
                .iter()
                .map(|l| (l.start, l.end, &l.label))
                .collect::<Vec<_>>(),
            right
                .links
                .iter()
                .map(|l| (l.start, l.end, &l.label))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            left.definitions
                .iter()
                .map(|d| (d.start, d.end, &d.name, &d.kind))
                .collect::<Vec<_>>(),
            right
                .definitions
                .iter()
                .map(|d| (d.start, d.end, &d.name, &d.kind))
                .collect::<Vec<_>>()
        );
    }
    fn fixture(failure: Option<usize>) -> DexClass {
        let mut class = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let mut second = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0)
            .methods
            .remove(0);
        second.name = "later".into();
        class.methods.push(second);
        if let Some(index) = failure {
            class.methods.insert(
                index,
                DexMethod {
                    declaring_type: class.descriptor.clone(),
                    name: "unsupported".into(),
                    return_type: "V".into(),
                    parameters: vec![],
                    thrown_types: vec![],
                    access_flags: 9,
                    code: Some(crate::native_dex::DexCode {
                        registers: 1,
                        ins: 0,
                        outs: 0,
                        tries: 0,
                        try_regions: vec![],
                        instructions: vec![0x001d, 0x000e],
                        offset: 0,
                    }),
                },
            );
        }
        class
    }
    fn counted(class: &DexClass) -> DecompiledCode {
        let mut counts = vec![0; class.methods.len()];
        let code = render_mixed_with("sample.Hello", class, |method| {
            let index = class
                .methods
                .iter()
                .position(|candidate| std::ptr::eq(candidate, method))
                .unwrap();
            counts[index] += 1;
            render_method("sample.Hello", class, method)
        });
        assert_eq!(
            counts,
            vec![1; class.methods.len()],
            "each method must be attempted exactly once"
        );
        code
    }
    #[test]
    fn each_method_is_rendered_once_across_success_and_early_or_late_failure() {
        for class in [
            fixture(None),
            fixture(Some(0)),
            fixture(Some(1)),
            fixture(Some(2)),
        ] {
            assert_same(&counted(&class), &legacy("sample.Hello", &class));
        }
    }
    #[test]
    fn cancelled_class_stops_between_members_without_returning_partial_source() {
        for mut class in [fixture(None), fixture(Some(0)), fixture(Some(1))] {
            for invalid_header in [false, true] {
                if invalid_header {
                    class.access_flags |= 0x8000;
                }
                let calls = std::cell::Cell::new(0usize);
                let result = render_mixed_controlled(
                    "sample.Hello",
                    &class,
                    |method| {
                        calls.set(calls.get() + 1);
                        render_method("sample.Hello", &class, method)
                    },
                    &|| calls.get() >= 1,
                );
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("decompilation cancelled")
                );
                assert_eq!(calls.get(), 1);
                assert_same(
                    &render_mixed("sample.Hello", &class),
                    &legacy("sample.Hello", &class),
                );
            }
        }
    }
    #[test]
    fn invalid_class_header_still_renders_each_member_once_with_identical_mappings() {
        let mut class = fixture(Some(1));
        class.access_flags |= 0x8000;
        assert_same(&counted(&class), &legacy("sample.Hello", &class));
    }
    #[test]
    #[ignore = "Set RDX_TEST_APK to benchmark already-open Play Store VendingBackupAgent"]
    fn benchmark_backup_class_without_repeated_method_generation() {
        use crate::{engine::DecompilerEngine, native_engine::NativeDexEngine};
        let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
        let mut engine = NativeDexEngine::default();
        engine.open(std::path::Path::new(&path)).unwrap();
        let name = "com.google.android.finsky.setup.VendingBackupAgent";
        let class = engine.class(name).unwrap();
        let expected = legacy(name, class);
        assert_same(&render_mixed(name, class), &expected);
        let mut old = Vec::new();
        let mut new = Vec::new();
        for index in 0..20 {
            for single in if index % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let start = std::time::Instant::now();
                let code = if single {
                    render_mixed(name, class)
                } else {
                    legacy(name, class)
                };
                let micros = start.elapsed().as_micros();
                assert_same(&code, &expected);
                if single {
                    new.push(micros);
                } else {
                    old.push(micros);
                }
            }
        }
        old.sort_unstable();
        new.sort_unstable();
        let result = serde_json::json!({"class":name,"debug_assertions":cfg!(debug_assertions),"samples_each":20,"legacy_median_us":old[10],"single_pass_median_us":new[10],"source_bytes":expected.source.len(),"source_and_metadata_equal":true});
        println!("{result}");
        if let Some(path) = std::env::var_os("RDX_RENDER_BENCHMARK_OUTPUT") {
            std::fs::write(path, serde_json::to_string_pretty(&result).unwrap()).unwrap();
        }
    }
}

#[cfg(test)]
mod cold_phase_profile {
    use super::*;
    #[test]
    #[ignore = "Set RDX_TEST_APK and optionally RDX_PROFILE_CLASSES for renderer phase timings"]
    fn profile_renderer_phases() {
        use crate::{engine::DecompilerEngine, native_engine::NativeDexEngine};
        use std::time::Instant;
        let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
        let mut engine = NativeDexEngine::default();
        engine.open(std::path::Path::new(&path)).unwrap();
        for name in std::env::var("RDX_PROFILE_CLASSES")
            .unwrap_or_else(|_| "abvc,absi,aefa".into())
            .split(',')
        {
            let class = engine.class(name).unwrap();
            let before = Instant::now();
            let total = engine.render(name).unwrap();
            let total_us = before.elapsed().as_micros();
            let before = Instant::now();
            let prefix = class_prefix(name, class);
            let prefix_us = before.elapsed().as_micros();
            let prefix_error = prefix.as_ref().err().map(ToString::to_string);
            let mut raw_methods_us = 0;
            let mut presentation_us = 0;
            let mut failed = 0;
            let mut codes = Vec::new();
            let mut slow = Vec::new();
            for method in &class.methods {
                let before = Instant::now();
                let code = render_method(name, class, method);
                let elapsed = before.elapsed().as_micros();
                raw_methods_us += elapsed;
                if elapsed >= 10_000 {
                    slow.push((
                        elapsed,
                        method.name.to_string(),
                        code.as_ref().err().map(ToString::to_string),
                    ));
                }
                if let Ok(code) = code {
                    let before = Instant::now();
                    codes.push(presentation(code, class));
                    presentation_us += before.elapsed().as_micros();
                } else {
                    failed += 1;
                }
            }
            let before = Instant::now();
            for field in &class.fields {
                codes.push(
                    render_field(name, class, field)
                        .unwrap_or_else(|_| render_mixed_field_fallback(name, class, field)),
                );
            }
            let fields_us = before.elapsed().as_micros();
            if let Ok(header) = render_header(name, class) {
                codes.push(header.finish());
            }
            let before = Instant::now();
            let raw = crate::native_engine::disassembly::render(name, class);
            let disassembly_us = before.elapsed().as_micros();
            let before = Instant::now();
            let shortened = readable::shorten(name, class, codes);
            let shorten_us = before.elapsed().as_micros();
            std::hint::black_box(shortened);
            std::hint::black_box(raw);
            println!(
                "PHASE {}",
                serde_json::json!({"class":name,"total_us":total_us,"prefix_us":prefix_us,"prefix_error":prefix_error,"methods":class.methods.len(),"failed_methods":failed,"fields":class.fields.len(),"raw_methods_us":raw_methods_us,"presentation_us":presentation_us,"fields_us":fields_us,"disassembly_us":disassembly_us,"shorten_us":shorten_us,"source_bytes":total.source.len(),"source_hash":total.source_hash,"links":total.links.len(),"slow_methods":slow})
            );
        }
    }
}
