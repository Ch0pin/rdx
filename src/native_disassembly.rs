//! Native DEX instruction rendering. This is disassembly, not reconstructed Java.
use crate::{
    engine::{CodeDefinition, CodeLink, DecompiledCode},
    native_dex::{DexClass, DexMethod, DexSymbols},
};
pub fn type_name(value: &str) -> String {
    value
        .strip_prefix('L')
        .and_then(|v| v.strip_suffix(';'))
        .unwrap_or(value)
        .replace('/', ".")
}
pub fn method_id(method: &DexMethod) -> String {
    format!(
        "{}.{}({}){}",
        type_name(&method.declaring_type),
        method.name,
        method.parameters.join(""),
        method.return_type
    )
}
fn symbol(s: &DexSymbols, kind: char, index: usize) -> Option<String> {
    match kind {
        's' => s.strings.get(index).map(|v| format!("{v:?}")),
        't' => s.types.get(index).map(|v| type_name(v)),
        'f' => s.fields.get(index).map(|&(owner, ty, name)| {
            format!(
                "{}.{}:{}",
                type_name(&s.types[owner as usize]),
                s.strings[name as usize],
                s.types[ty as usize]
            )
        }),
        'm' => s.methods.get(index).map(|&(owner, proto, name)| {
            let (ret, args) = &s.protos[proto as usize];
            format!(
                "{}.{}({}){}",
                type_name(&s.types[owner as usize]),
                s.strings[name as usize],
                args.join(""),
                ret
            )
        }),
        'p' => s
            .protos
            .get(index)
            .map(|(ret, args)| format!("({}){ret}", args.join(""))),
        _ => None,
    }
}
#[derive(Default)]
struct Output {
    text: String,
    chars: usize,
    links: Vec<CodeLink>,
    definitions: Vec<CodeDefinition>,
}
impl Output {
    fn push(&mut self, value: &str) {
        self.chars += value.chars().count();
        self.text.push_str(value);
    }
    fn reference(&mut self, value: &str) {
        let start = self.chars;
        self.push(value);
        self.links.push(CodeLink {
            start,
            end: self.chars,
            label: value.into(),
        });
    }
    fn definition(&mut self, kind: &str, name: &str, display: &str) {
        let start = self.chars;
        self.push(display);
        self.definitions.push(CodeDefinition {
            start,
            end: self.chars,
            kind: kind.into(),
            name: name.into(),
        });
    }
}
pub fn render(name: &str, class: &DexClass) -> DecompiledCode {
    let mut out = Output::default();
    out.push(
        "// Native DEX disassembly. Java control-flow/type reconstruction is not yet available.\n",
    );
    out.push(".class ");
    out.definition("class", name, name);
    out.push(&format!("  // access=0x{:x}\n", class.access_flags));
    if let Some(parent) = &class.superclass {
        out.push(".super ");
        out.reference(&type_name(parent));
        out.push("\n");
    }
    for interface in &class.interfaces {
        out.push(".implements ");
        out.reference(&type_name(interface));
        out.push("\n");
    }
    let mut static_index = 0;
    for field in &class.fields {
        out.push(".field ");
        out.definition(
            "field",
            &field.name,
            &format!("{}.{}:{}", name, field.name, field.field_type),
        );
        out.push(&format!(" // access=0x{:x}", field.access_flags));
        if field.is_static {
            if let Some(value) = class.static_values.get(static_index) {
                out.push(&format!("; encoded={}", value_preview(value)));
            }
            static_index += 1;
        }
        out.push("\n");
    }
    for method in &class.methods {
        out.push("\n.method ");
        out.definition("method", &method.name, &method_id(method));
        out.push(&format!(" // access=0x{:x}\n", method.access_flags));
        for ty in &method.thrown_types {
            out.push("    .throws ");
            out.reference(&type_name(ty));
            out.push("\n");
        }
        if let Some(code) = &method.code {
            out.push(&format!(
                "    .registers {} // inputs={}, outputs={}, try_blocks={}\n",
                code.registers, code.ins, code.outs, code.tries
            ));
            let words = &code.instructions;
            let mut pc = 0;
            while pc < words.len() {
                let w = words[pc];
                let op = w as u8;
                let Some(width) = width(words, pc) else {
                    out.push(&format!("    {pc:04x}: .malformed 0x{w:04x} // truncated or invalid payload; remaining words follow\n"));
                    for (i, word) in words[pc + 1..].iter().enumerate() {
                        out.push(&format!("    {:04x}: .word 0x{word:04x}\n", pc + i + 1));
                    }
                    break;
                };
                let raw = words[pc..pc + width]
                    .iter()
                    .take(8)
                    .map(|v| format!("{v:04x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push(&format!("    {pc:04x}: {:<24} ", mnemonic(op, w)));
                if op == 0 && w != 0 {
                    out.push(&format!("// {width} code units: {raw}"));
                    for chunk in words[pc + width.min(8)..pc + width].chunks(16) {
                        out.push("\n        .data ");
                        for word in chunk {
                            out.push(&format!("{word:04x} "));
                        }
                    }
                } else {
                    out.push(&operands(op, &words[pc..pc + width], pc));
                    let reference = match op {
                        0x1a => Some(('s', words[pc + 1] as usize)),
                        0x1b => Some((
                            's',
                            (words[pc + 1] as u32 | (words[pc + 2] as u32) << 16) as usize,
                        )),
                        0x1c | 0x1f | 0x20 | 0x22..=0x25 => Some(('t', words[pc + 1] as usize)),
                        0x52..=0x6d => Some(('f', words[pc + 1] as usize)),
                        0x6e..=0x72 | 0x74..=0x78 | 0xfa | 0xfb => {
                            Some(('m', words[pc + 1] as usize))
                        }
                        0xff => Some(('p', words[pc + 1] as usize)),
                        _ => None,
                    };
                    if let Some((kind, index)) = reference {
                        out.push(", ");
                        if let Some(value) = symbol(&class.symbols, kind, index) {
                            if kind == 's' || kind == 'p' {
                                out.push(&value)
                            } else {
                                out.reference(&value)
                            }
                        } else {
                            out.push(&format!("<invalid {kind} index {index}>"));
                        }
                    }
                    out.push(&format!(" // [{raw}]"));
                }
                out.push("\n");
                pc += width;
            }
            for region in &code.try_regions {
                for (ty, handler) in region.catches.iter() {
                    out.push("    ");
                    if let Some(ty) = ty {
                        out.push(".catch ");
                        out.reference(&type_name(ty));
                    } else {
                        out.push(".catchall");
                    }
                    out.push(&format!(
                        " {{ @{start:04x} .. @{end:04x} }} @{handler:04x}\n",
                        start = region.start,
                        end = region.end
                    ));
                }
            }
        } else {
            out.push("    // No DEX code item (abstract/native declaration).\n");
        }
        out.push(".end method\n");
    }
    let source_hash = crate::engine::source_identity(&out.text);
    DecompiledCode {
        source: out.text,
        source_hash,
        links: out.links,
        definitions: out.definitions,
    }
}
/// Stop Debug formatting at a bounded prefix, including nested encoded values.
fn value_preview(value: &crate::native_dex::DexValue) -> String {
    use std::fmt::Write;
    struct Preview {
        text: String,
        truncated: bool,
    }
    impl std::fmt::Write for Preview {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            let available = 512usize.saturating_sub(self.text.len());
            if s.len() <= available {
                self.text.push_str(s);
                return Ok(());
            }
            let mut end = available;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            self.text.push_str(&s[..end]);
            self.truncated = true;
            Err(std::fmt::Error)
        }
    }
    let mut out = Preview {
        text: String::new(),
        truncated: false,
    };
    let _ = write!(&mut out, "{value:?}");
    if out.truncated {
        out.text.push_str("… (truncated)");
    }
    out.text
}

pub(crate) fn width(words: &[u16], pc: usize) -> Option<usize> {
    let w = *words.get(pc)?;
    let op = w as u8;
    let size = if op == 0 {
        match w >> 8 {
            0 => 1,
            1 => 4usize.checked_add(usize::from(*words.get(pc + 1)?).checked_mul(2)?)?,
            2 => 2usize.checked_add(usize::from(*words.get(pc + 1)?).checked_mul(4)?)?,
            3 => {
                let n = u32::from(*words.get(pc + 2)?) | (u32::from(*words.get(pc + 3)?) << 16);
                4usize.checked_add(
                    usize::from(*words.get(pc + 1)?)
                        .checked_mul(n as usize)?
                        .checked_add(1)?
                        / 2,
                )?
            }
            _ => 1,
        }
    } else {
        match op {
            0x18 => 5,
            0xfa | 0xfb => 4,
            0x03
            | 0x06
            | 0x09
            | 0x14
            | 0x17
            | 0x1b
            | 0x24..=0x26
            | 0x2a..=0x2c
            | 0x6e..=0x72
            | 0x74..=0x78
            | 0xfc
            | 0xfd => 3,
            0x02
            | 0x05
            | 0x08
            | 0x13
            | 0x15
            | 0x16
            | 0x19
            | 0x1a
            | 0x1c
            | 0x1f
            | 0x20
            | 0x22
            | 0x23
            | 0x29
            | 0x2d..=0x3d
            | 0x44..=0x6d
            | 0x90..=0xaf
            | 0xd0..=0xe2
            | 0xfe
            | 0xff => 2,
            _ => 1,
        }
    };
    (pc.checked_add(size)? <= words.len()).then_some(size)
}
fn operands(op: u8, w: &[u16], pc: usize) -> String {
    let a = w[0] >> 8;
    let lo = a & 15;
    let hi = a >> 4;
    match op {
        0x00 | 0x0e => String::new(),
        0x01 | 0x04 | 0x07 | 0x21 | 0x7b..=0x8f | 0xb0..=0xcf => format!("v{lo}, v{hi}"),
        0x02 | 0x05 | 0x08 => format!("v{a}, v{}", w[1]),
        0x03 | 0x06 | 0x09 => format!("v{}, v{}", w[1], w[2]),
        0x12 => format!("v{lo}, {}", (w[0] as i16) >> 12),
        0x13 | 0x16 => format!("v{a}, {}", w[1] as i16),
        0x14 | 0x17 => format!(
            "v{a}, {}",
            (u32::from(w[1]) | (u32::from(w[2]) << 16)) as i32
        ),
        0x15 => format!("v{a}, {}", (u32::from(w[1]) << 16) as i32),
        0x18 => format!(
            "v{a}, {}",
            (u64::from(w[1])
                | (u64::from(w[2]) << 16)
                | (u64::from(w[3]) << 32)
                | (u64::from(w[4]) << 48)) as i64
        ),
        0x19 => format!("v{a}, {}", (u64::from(w[1]) << 48) as i64),
        0x28 => format!("@{:04x}", pc as i64 + (a as u8 as i8) as i64),
        0x29 => format!("@{:04x}", pc as i64 + w[1] as i16 as i64),
        0x2a => format!(
            "@{:04x}",
            pc as i64 + (u32::from(w[1]) | (u32::from(w[2]) << 16)) as i32 as i64
        ),
        0x26 | 0x2b | 0x2c => format!(
            "v{a}, @{:04x}",
            pc as i64 + (u32::from(w[1]) | (u32::from(w[2]) << 16)) as i32 as i64
        ),
        0x32..=0x37 => format!("v{lo}, v{hi}, @{:04x}", pc as i64 + w[1] as i16 as i64),
        0x38..=0x3d => format!("v{a}, @{:04x}", pc as i64 + w[1] as i16 as i64),
        0x20 | 0x23 | 0x52..=0x5f => format!("v{lo}, v{hi}"),
        0x24 | 0x6e..=0x72 | 0xfa | 0xfc => {
            let regs = [
                w[2] & 15,
                (w[2] >> 4) & 15,
                (w[2] >> 8) & 15,
                w[2] >> 12,
                lo,
            ];
            format!(
                "{{{}}}",
                regs.iter()
                    .take(hi.min(5) as usize)
                    .map(|r| format!("v{r}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        0x25 | 0x74..=0x78 | 0xfb | 0xfd => format!(
            "{{v{} .. v{}}}",
            w[2],
            u32::from(w[2]) + u32::from(a).saturating_sub(1)
        ),
        0x2d..=0x31 | 0x44..=0x51 | 0x90..=0xaf => format!("v{a}, v{}, v{}", w[1] & 255, w[1] >> 8),
        0xd0..=0xd7 => format!("v{lo}, v{hi}, {}", w[1] as i16),
        0xd8..=0xe2 => format!("v{a}, v{}, {}", w[1] & 255, (w[1] >> 8) as u8 as i8),
        _ => format!("v{a}"),
    }
}
fn mnemonic(op: u8, word: u16) -> &'static str {
    match op {
        0x00 => match word >> 8 {
            1 => ".packed-switch-payload",
            2 => ".sparse-switch-payload",
            3 => ".array-data-payload",
            0 => "nop",
            _ => ".unknown-payload",
        },
        0x01 => "move",
        0x02 => "move/from16",
        0x03 => "move/16",
        0x04 => "move-wide",
        0x05 => "move-wide/from16",
        0x06 => "move-wide/16",
        0x07 => "move-object",
        0x08 => "move-object/from16",
        0x09 => "move-object/16",
        0x0a => "move-result",
        0x0b => "move-result-wide",
        0x0c => "move-result-object",
        0x0d => "move-exception",
        0x0e => "return-void",
        0x0f => "return",
        0x10 => "return-wide",
        0x11 => "return-object",
        0x12 => "const/4",
        0x13 => "const/16",
        0x14 => "const",
        0x15 => "const/high16",
        0x16 => "const-wide/16",
        0x17 => "const-wide/32",
        0x18 => "const-wide",
        0x19 => "const-wide/high16",
        0x1a => "const-string",
        0x1b => "const-string/jumbo",
        0x1c => "const-class",
        0x1d => "monitor-enter",
        0x1e => "monitor-exit",
        0x1f => "check-cast",
        0x20 => "instance-of",
        0x21 => "array-length",
        0x22 => "new-instance",
        0x23 => "new-array",
        0x24 => "filled-new-array",
        0x25 => "filled-new-array/range",
        0x26 => "fill-array-data",
        0x27 => "throw",
        0x28 => "goto",
        0x29 => "goto/16",
        0x2a => "goto/32",
        0x2b => "packed-switch",
        0x2c => "sparse-switch",
        0x2d..=0x31 => [
            "cmpl-float",
            "cmpg-float",
            "cmpl-double",
            "cmpg-double",
            "cmp-long",
        ][(op - 0x2d) as usize],
        0x32..=0x3d => [
            "if-eq", "if-ne", "if-lt", "if-ge", "if-gt", "if-le", "if-eqz", "if-nez", "if-ltz",
            "if-gez", "if-gtz", "if-lez",
        ][(op - 0x32) as usize],
        0x44..=0x6d => [
            "aget",
            "aget-wide",
            "aget-object",
            "aget-boolean",
            "aget-byte",
            "aget-char",
            "aget-short",
            "aput",
            "aput-wide",
            "aput-object",
            "aput-boolean",
            "aput-byte",
            "aput-char",
            "aput-short",
            "iget",
            "iget-wide",
            "iget-object",
            "iget-boolean",
            "iget-byte",
            "iget-char",
            "iget-short",
            "iput",
            "iput-wide",
            "iput-object",
            "iput-boolean",
            "iput-byte",
            "iput-char",
            "iput-short",
            "sget",
            "sget-wide",
            "sget-object",
            "sget-boolean",
            "sget-byte",
            "sget-char",
            "sget-short",
            "sput",
            "sput-wide",
            "sput-object",
            "sput-boolean",
            "sput-byte",
            "sput-char",
            "sput-short",
        ][(op - 0x44) as usize],
        0x6e..=0x72 => [
            "invoke-virtual",
            "invoke-super",
            "invoke-direct",
            "invoke-static",
            "invoke-interface",
        ][(op - 0x6e) as usize],
        0x74..=0x78 => [
            "invoke-virtual/range",
            "invoke-super/range",
            "invoke-direct/range",
            "invoke-static/range",
            "invoke-interface/range",
        ][(op - 0x74) as usize],
        0x7b..=0x8f => [
            "neg-int",
            "not-int",
            "neg-long",
            "not-long",
            "neg-float",
            "neg-double",
            "int-to-long",
            "int-to-float",
            "int-to-double",
            "long-to-int",
            "long-to-float",
            "long-to-double",
            "float-to-int",
            "float-to-long",
            "float-to-double",
            "double-to-int",
            "double-to-long",
            "double-to-float",
            "int-to-byte",
            "int-to-char",
            "int-to-short",
        ][(op - 0x7b) as usize],
        0x90..=0xaf => BINARY[(op - 0x90) as usize],
        0xb0..=0xcf => BINARY_TWO[(op - 0xb0) as usize],
        0xd0..=0xd7 => [
            "add-int/lit16",
            "rsub-int",
            "mul-int/lit16",
            "div-int/lit16",
            "rem-int/lit16",
            "and-int/lit16",
            "or-int/lit16",
            "xor-int/lit16",
        ][(op - 0xd0) as usize],
        0xd8..=0xe2 => [
            "add-int/lit8",
            "rsub-int/lit8",
            "mul-int/lit8",
            "div-int/lit8",
            "rem-int/lit8",
            "and-int/lit8",
            "or-int/lit8",
            "xor-int/lit8",
            "shl-int/lit8",
            "shr-int/lit8",
            "ushr-int/lit8",
        ][(op - 0xd8) as usize],
        0xfa => "invoke-polymorphic",
        0xfb => "invoke-polymorphic/range",
        0xfc => "invoke-custom",
        0xfd => "invoke-custom/range",
        0xfe => "const-method-handle",
        0xff => "const-method-type",
        _ => ".unused-opcode",
    }
}
const BINARY: [&str; 32] = [
    "add-int",
    "sub-int",
    "mul-int",
    "div-int",
    "rem-int",
    "and-int",
    "or-int",
    "xor-int",
    "shl-int",
    "shr-int",
    "ushr-int",
    "add-long",
    "sub-long",
    "mul-long",
    "div-long",
    "rem-long",
    "and-long",
    "or-long",
    "xor-long",
    "shl-long",
    "shr-long",
    "ushr-long",
    "add-float",
    "sub-float",
    "mul-float",
    "div-float",
    "rem-float",
    "add-double",
    "sub-double",
    "mul-double",
    "div-double",
    "rem-double",
];
const BINARY_TWO: [&str; 32] = [
    "add-int/2addr",
    "sub-int/2addr",
    "mul-int/2addr",
    "div-int/2addr",
    "rem-int/2addr",
    "and-int/2addr",
    "or-int/2addr",
    "xor-int/2addr",
    "shl-int/2addr",
    "shr-int/2addr",
    "ushr-int/2addr",
    "add-long/2addr",
    "sub-long/2addr",
    "mul-long/2addr",
    "div-long/2addr",
    "rem-long/2addr",
    "and-long/2addr",
    "or-long/2addr",
    "xor-long/2addr",
    "shl-long/2addr",
    "shr-long/2addr",
    "ushr-long/2addr",
    "add-float/2addr",
    "sub-float/2addr",
    "mul-float/2addr",
    "div-float/2addr",
    "rem-float/2addr",
    "add-double/2addr",
    "sub-double/2addr",
    "mul-double/2addr",
    "div-double/2addr",
    "rem-double/2addr",
];
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn payload_boundaries_and_signed_branches() {
        assert_eq!(width(&[0x0100, 1, 0, 0, 2, 0], 0), Some(6));
        assert_eq!(width(&[0x0300, 8, 0xffff, 0xffff], 0), None);
        assert_eq!(operands(0x28, &[0xff28], 9), "@0008");
    }
    #[test]
    fn method_and_field_operands_resolve_overloaded_symbols() {
        let dex =
            crate::native_dex::parse(include_bytes!("../tests/fixtures/navigation.dex")).unwrap();
        let class = dex
            .classes
            .iter()
            .find(|c| c.descriptor.as_ref() == "Lsample/Caller;")
            .unwrap();
        let code = render("sample.Caller", class);
        for symbol in [
            "sample.Target.doubleValue(I)I",
            "sample.Target.doubleValue(Ljava/lang/String;)Ljava/lang/String;",
            "sample.Target.value:I",
        ] {
            assert!(code.links.iter().any(|link| link.label == symbol));
        }
        assert!(code.source.contains("000a: move-result-object"));
        assert!(code.source.contains("Ελληνικά 🦀"));
    }
    #[test]
    fn references_use_exact_unicode_positions() {
        let dex =
            crate::native_dex::parse(include_bytes!("../tests/fixtures/navigation.dex")).unwrap();
        let class = dex
            .classes
            .iter()
            .find(|c| c.descriptor.as_ref() == "Lsample/Target;")
            .unwrap();
        let result = render("sample.Target", class);
        assert!(result.source.contains(".method"));
        for link in result.links {
            assert_eq!(
                result
                    .source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>(),
                link.label
            );
        }
    }
    #[test]
    fn decoded_exception_regions_remain_visible_in_fallback_and_link_types() {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let code = class
            .methods
            .iter_mut()
            .find(|m| m.name.as_ref() == "answer")
            .unwrap()
            .code
            .as_mut()
            .unwrap();
        code.tries = 1;
        code.try_regions = vec![crate::native_dex::DexTryRegion {
            start: 0,
            end: 2,
            catches: std::sync::Arc::from([
                (Some(std::sync::Arc::from("Ljava/lang/Exception;")), 2),
                (None, 2),
            ]),
        }];
        let raw = render("sample.Hello", &class);
        assert!(
            raw.source
                .contains(".catch java.lang.Exception { @0000 .. @0002 } @0002")
        );
        assert!(raw.source.contains(".catchall { @0000 .. @0002 } @0002"));
        let link = raw
            .links
            .iter()
            .find(|l| l.label == "java.lang.Exception")
            .unwrap();
        assert_eq!(
            raw.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "java.lang.Exception"
        );
    }
    #[test]
    fn encoded_value_debug_preview_is_bounded_even_for_composite_values() {
        let value =
            crate::native_dex::DexValue::Array(vec![crate::native_dex::DexValue::Int(42); 10_000]);
        let text = value_preview(&value);
        assert!(text.len() < 550);
        assert!(text.ends_with("(truncated)"));
    }
}
