use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexValue},
    native_java,
};
use std::sync::Arc;

fn field(name: &str, ty: &str, flags: u32) -> DexField {
    DexField {
        declaring_type: "Lsample/Defaults;".into(),
        name: name.into(),
        field_type: ty.into(),
        access_flags: flags,
        is_static: flags & 8 != 0,
    }
}

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Defaults;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![
            field("I0", "I", 0x1019),
            field("Z0", "Z", 0x19),
            field("R0", "Ljava/lang/Object;", 0x19),
            field("A0", "[I", 0x19),
            field("J0", "J", 0x19),
            field("F0", "F", 0x19),
            field("D0", "D", 0x19),
            field("C0", "C", 0x19),
            field("B0", "B", 0x19),
            field("S0", "S", 0x19),
        ],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    }
}

#[test]
fn omitted_dex_trailing_values_emit_exact_java_defaults() {
    let class = fixture();
    for (index, expected) in [
        " = 0;",
        " = false;",
        " = null;",
        " = null;",
        " = 0L;",
        " = 0.0f;",
        " = 0.0d;",
        " = (char) 0;",
        " = 0;",
        " = 0;",
    ]
    .iter()
    .enumerate()
    {
        let source = native_java::render_field("sample.Defaults", &class, &class.fields[index])
            .unwrap()
            .source;
        assert!(source.contains(expected), "{source}");
    }
}

#[test]
fn encoded_prefix_remains_exact_and_invalid_metadata_rejects_implicit_default() {
    let mut class = fixture();
    class.static_values_offset = 1;
    class.static_values = vec![DexValue::Int(7)];
    let explicit = native_java::render_field("sample.Defaults", &class, &class.fields[0])
        .unwrap()
        .source;
    let omitted = native_java::render_field("sample.Defaults", &class, &class.fields[1])
        .unwrap()
        .source;
    assert!(explicit.contains(" = 7;"), "{explicit}");
    assert!(omitted.contains(" = false;"), "{omitted}");

    class.static_values_offset = 0;
    assert!(native_java::render_field("sample.Defaults", &class, &class.fields[1]).is_err());
    class.static_values_offset = 1;
    class.static_values.clear();
    assert!(native_java::render_field("sample.Defaults", &class, &class.fields[1]).is_err());
    class.static_values = vec![DexValue::String(0)];
    assert!(native_java::render_field("sample.Defaults", &class, &class.fields[1]).is_err());
}

#[test]
fn clinit_assignment_and_unresolved_clinit_reject_implicit_final_initializer() {
    let mut class = fixture();
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Defaults;".into(), "I".into()],
        strings: vec!["I0".into()],
        fields: vec![(0, 1, 0)],
        ..Default::default()
    });
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "<clinit>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 8,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x0012, 0x0067, 0, 0x000e],
            offset: 0,
        }),
    });
    assert!(native_java::render_field("sample.Defaults", &class, &class.fields[0]).is_err());
    class.methods[0].code = None;
    assert!(native_java::render_field("sample.Defaults", &class, &class.fields[0]).is_err());
    class.methods[0].code = Some(DexCode {
        registers: 0,
        ins: 0,
        outs: 0,
        tries: 0,
        try_regions: vec![],
        instructions: vec![0xffff],
        offset: 0,
    });
    assert!(native_java::render_field("sample.Defaults", &class, &class.fields[0]).is_err());
    Arc::get_mut(&mut class.symbols)
        .unwrap()
        .types
        .push("Lsample/Child;".into());
    Arc::get_mut(&mut class.symbols).unwrap().fields[0].0 = 2;
    class.methods[0].code.as_mut().unwrap().registers = 1;
    class.methods[0].code.as_mut().unwrap().instructions = vec![0x0012, 0x0067, 0, 0x000e];
    assert!(native_java::render_field("sample.Defaults", &class, &class.fields[0]).is_err());
    Arc::get_mut(&mut class.symbols).unwrap().strings[0] = "OTHER".into();
    let unrelated = native_java::render_field("sample.Defaults", &class, &class.fields[0]);
    assert!(unrelated.is_ok(), "{unrelated:?}");

    let class = fixture();
    let instance_final = field("instance", "I", 0x11);
    assert!(native_java::render_field("sample.Defaults", &class, &instance_final).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn omitted_defaults_compile_and_retain_zero_null_values_on_jvm() {
    use std::{fs, process::Command};
    let class = fixture();
    let fields = class
        .fields
        .iter()
        .map(|field| {
            native_java::render_field("sample.Defaults", &class, field)
                .unwrap()
                .source
        })
        .collect::<String>();
    let dir = std::env::temp_dir().join(format!("rdx-static-defaults-{}", std::process::id()));
    let package = dir.join("sample");
    fs::create_dir_all(&package).unwrap();
    let source = format!(
        "package sample; public final class Defaults {{\n{fields}\npublic static void main(String[] ignored) {{ if (I0 != 0 || Z0 || R0 != null || A0 != null || J0 != 0L || F0 != 0.0f || D0 != 0.0d || C0 != 0 || B0 != 0 || S0 != 0) throw new AssertionError(); }}\n}}"
    );
    fs::write(package.join("Defaults.java"), &source).unwrap();
    let compiled = Command::new("javac")
        .arg(package.join("Defaults.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Defaults")
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}
