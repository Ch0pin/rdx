//! Encoded Java fields reconstructed without Java tooling.
use rdx::{
    native_dex::{self, DexClass, DexField, DexSymbols, DexValue},
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.fields.clear();
    class.methods.clear();
    class
}
fn field(class: &mut DexClass, name: &str, ty: &str, flags: u32) {
    class.fields.push(DexField {
        declaring_type: class.descriptor.clone(),
        name: name.into(),
        field_type: ty.into(),
        access_flags: flags,
        is_static: flags & 8 != 0,
    });
}
fn text(source: &str, start: usize, end: usize) -> String {
    source.chars().skip(start).take(end - start).collect()
}

#[test]
fn encoded_string_constants_keep_values_and_exact_unicode_navigation() {
    let mut class = fixture();
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["engagement_id".into(), "confirm é😀\n\"quote\"".into()],
        ..Default::default()
    });
    field(&mut class, "ARG_ENGAGEMENT_ID", "Ljava/lang/String;", 0x19);
    field(
        &mut class,
        "ARG_ERROR_CONFIRM_MSG",
        "Ljava/lang/String;",
        0x19,
    );
    field(&mut class, "AFTER", "I", 0x19);
    class.static_values = vec![DexValue::String(0), DexValue::String(1), DexValue::Int(42)];
    class.static_values_offset = 1;
    let code = native_java::render("sample.Hello", &class).unwrap();
    assert!(
        code.source
            .contains("public static final String ARG_ENGAGEMENT_ID = \"engagement_id\";")
    );
    assert!(
        code.source
            .contains("ARG_ERROR_CONFIRM_MSG = \"confirm é😀\\n\\\"quote\\\"\";")
    );
    for def in code.definitions.iter().filter(|d| d.kind == "field") {
        assert_eq!(text(&code.source, def.start, def.end), def.name);
        assert!(code.links.iter().any(|l| l.start == def.start
            && l.end == def.end
            && l.label.starts_with(&format!("sample.Hello.{}:", def.name))));
    }
    assert_eq!(
        code.definitions
            .iter()
            .filter(|d| d.kind == "field")
            .count(),
        3
    );
}
#[test]
fn primitive_encoded_values_preserve_signedness_width_and_float_bits() {
    let mut class = fixture();
    let cases = [
        ("B", DexValue::Byte(i8::MIN), "-128"),
        ("S", DexValue::Short(i16::MIN), "-32768"),
        ("C", DexValue::Char(0xffff), "(char) 0xffff"),
        ("I", DexValue::Int(i32::MIN), "-2147483648"),
        ("J", DexValue::Long(i64::MIN), "-9223372036854775808L"),
        ("F", DexValue::Float(0x80000000), "-0.0f"),
        ("D", DexValue::Double(0x8000000000000000), "-0.0d"),
        ("Z", DexValue::Boolean(true), "true"),
    ];
    for (i, (ty, value, _)) in cases.iter().enumerate() {
        field(&mut class, &format!("VALUE{i}"), ty, 0x19);
        class.static_values.push(value.clone());
    }
    let code = native_java::render("sample.Hello", &class).unwrap();
    for (i, (_, _, expected)) in cases.iter().enumerate() {
        assert!(
            code.source.contains(&format!("VALUE{i} = {expected};")),
            "{}",
            code.source
        );
    }
}
#[test]
fn default_and_unsupported_final_initializers_are_never_fabricated() {
    let mut class = fixture();
    field(&mut class, "value", "I", 0x9);
    assert!(
        native_java::render("sample.Hello", &class)
            .unwrap()
            .source
            .contains("static int value;")
    );
    class.fields[0].access_flags |= 0x10;
    assert!(native_java::render_field("sample.Hello", &class, &class.fields[0]).is_err());
    class.static_values = vec![DexValue::String(0)];
    assert!(native_java::render_field("sample.Hello", &class, &class.fields[0]).is_err());
    class.static_values = vec![DexValue::Array(vec![])];
    assert!(native_java::render_field("sample.Hello", &class, &class.fields[0]).is_err());
}
#[test]
fn escaped_backslashes_surrogates_and_null_are_preserved() {
    let mut class = fixture();
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["slash\\\\u000a lone\\u{d800}\0".into()],
        ..Default::default()
    });
    field(&mut class, "TEXT", "Ljava/lang/String;", 0x19);
    field(&mut class, "EMPTY", "Ljava/lang/Object;", 0x19);
    class.static_values = vec![DexValue::String(0), DexValue::Null];
    let code = native_java::render("sample.Hello", &class).unwrap();
    assert!(
        code.source.contains("\"slash\\\\u000a lone\\ud800\\000\""),
        "{}",
        code.source
    );
    assert!(code.source.contains("EMPTY = null;"));
}
#[test]
fn final_field_reassigned_in_clinit_keeps_dex_declaration() {
    let mut class = fixture();
    field(&mut class, "VALUE", "I", 0x19);
    class.static_values = vec![DexValue::Int(1)];
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["VALUE".into()],
        types: vec![class.descriptor.clone(), "I".into()],
        fields: vec![(0, 1, 0)],
        ..Default::default()
    });
    let mut method = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0)
        .methods
        .remove(0);
    method.name = "<clinit>".into();
    method.access_flags = 8;
    method.return_type = "V".into();
    method.parameters.clear();
    method.code.as_mut().unwrap().instructions = vec![0x1012, 0x0067, 0, 0x000e];
    class.methods.push(method);
    assert!(native_java::render_field("sample.Hello", &class, &class.fields[0]).is_err());
    class.methods[0].code.as_mut().unwrap().instructions = vec![0x000e];
    assert!(native_java::render_field("sample.Hello", &class, &class.fields[0]).is_ok());
}

#[test]
fn encoded_prefix_indexes_static_fields_only_and_preserves_type_links() {
    let mut class = fixture();
    class.symbols = Arc::new(DexSymbols {
        types: vec!["[Ljava/lang/String;".into()],
        ..Default::default()
    });
    field(&mut class, "instance", "I", 1);
    field(&mut class, "TYPE", "Ljava/lang/Class;", 0x19);
    field(&mut class, "trailingDefault", "I", 9);
    class.static_values = vec![DexValue::Type(0)];
    let code = native_java::render("sample.Hello", &class).unwrap();
    assert!(code.source.contains("int instance;"));
    assert!(code.source.contains("TYPE = String[].class;"));
    assert!(code.source.contains("static int trailingDefault;"));
    let link = code
        .links
        .iter()
        .find(|l| l.label == "java.lang.String")
        .unwrap();
    assert_eq!(text(&code.source, link.start, link.end), "String");
}

#[test]
fn float_encoded_constants_keep_constant_expression_semantics() {
    let mut class = fixture();
    for (i, (ty, value)) in [
        ("F", DexValue::Float(1)),
        ("F", DexValue::Float(f32::INFINITY.to_bits())),
        ("D", DexValue::Double(f64::NEG_INFINITY.to_bits())),
        ("D", DexValue::Double(1)),
    ]
    .into_iter()
    .enumerate()
    {
        field(&mut class, &format!("VALUE{i}"), ty, 0x19);
        class.static_values.push(value);
    }
    let code = native_java::render("sample.Hello", &class).unwrap();
    assert!(code.source.contains("VALUE0 = 1e-45f;"));
    assert!(code.source.contains("VALUE1 = (1.0f / 0.0f);"));
    assert!(code.source.contains("VALUE2 = (-1.0d / 0.0d);"));
    assert!(code.source.contains("VALUE3 = 5e-324d;"));
    assert!(!code.source.contains("BitsTo"));
    class.static_values[0] = DexValue::Float(0x7fc00001);
    assert!(native_java::render_field("sample.Hello", &class, &class.fields[0]).is_err());
}
