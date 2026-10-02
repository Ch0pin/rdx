use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn fixture() -> DexClass {
    let mut class = DexClass {
        descriptor: "Lsample/Mode;".into(),
        superclass: Some("Ljava/lang/Enum;".into()),
        interfaces: vec![],
        access_flags: 0x4011,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec![
                "ONE".into(),
                "TWO".into(),
                "a".into(),
                "b".into(),
                "all".into(),
                "<init>".into(),
                "clone".into(),
            ],
            types: vec![
                "Lsample/Mode;".into(),
                "Ljava/lang/Enum;".into(),
                "[Lsample/Mode;".into(),
            ],
            protos: vec![
                ("V".into(), vec!["Ljava/lang/String;".into(), "I".into()]),
                ("Ljava/lang/Object;".into(), vec![]),
            ],
            methods: vec![(1, 0, 5), (2, 1, 6)],
            fields: vec![(0, 0, 2), (0, 0, 3), (0, 2, 4)],
            ..Default::default()
        }),
    };
    for (name, ty, flags) in [
        ("a", "Lsample/Mode;", 0x4019),
        ("b", "Lsample/Mode;", 0x4019),
        ("all", "[Lsample/Mode;", 0x1a),
    ] {
        class.fields.push(DexField {
            declaring_type: class.descriptor.clone(),
            name: name.into(),
            field_type: ty.into(),
            access_flags: flags,
            is_static: true,
        });
    }
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "<clinit>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x10008,
        code: Some(DexCode {
            registers: 4,
            ins: 0,
            outs: 3,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![
                0x0022, 0, 0x011a, 0, 0x0212, 0x3070, 0, 0x0210, 0x0069, 0, 0x0122, 0, 0x021a, 1,
                0x1312, 0x3070, 0, 0x0321, 0x0169, 1, 0x2024, 2, 0x0010, 0x000c, 0x0069, 2, 0x000e,
            ],
        }),
    });
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "values".into(),
        return_type: "[Lsample/Mode;".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x0062, 2, 0x106e, 1, 0, 0x000c, 0x001f, 2, 0x0011],
        }),
    });
    class
}
#[test]
fn erased_enum_reconstructs_names_ordinals_and_obfuscated_aliases() {
    let class = fixture();
    let code = native_java::render("sample.Mode", &class).unwrap();
    assert!(code.source.contains("enum Mode"), "{}", code.source);
    assert!(code.source.contains("ONE,"), "{}", code.source);
    assert!(code.source.contains("Mode a = ONE"), "{}", code.source);
    assert!(!code.source.contains(" values("));
    assert!(
        code.definitions
            .iter()
            .any(|d| d.kind == "field" && d.name == "a")
    );
    native_java::render_method("sample.Mode", &class, &class.methods[0]).unwrap();
}
#[test]
fn malformed_enum_ordinals_and_custom_values_decline() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[14] = 0x2312;
    assert!(native_java::render("sample.Mode", &c).is_err());
    let mut c = fixture();
    c.methods[1].code.as_mut().unwrap().instructions[3] = 0;
    assert!(native_java::render("sample.Mode", &c).is_err());
}
#[test]
#[ignore = "requires javac and java"]
fn enum_jvm_names_values_identity_and_clone() {
    use std::{fs, process::Command};
    let c = fixture();
    let code = native_java::render("sample.Mode", &c).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-enum-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Mode.java"), code.source).unwrap();
    fs::write(dir.join("sample/Check.java"),r#"package sample; public class Check {public static void main(String[] x){if(!Mode.a.name().equals("ONE")||Mode.a.ordinal()!=0||!Mode.b.name().equals("TWO")||Mode.b.ordinal()!=1||Mode.valueOf("ONE")!=Mode.a)throw new AssertionError();Mode[] a=Mode.values();if(a.length!=2||a[0]!=Mode.a||a[1]!=Mode.b)throw new AssertionError();a[0]=null;if(Mode.values()[0]!=Mode.a)throw new AssertionError();}}"#).unwrap();
    let result = Command::new("javac")
        .current_dir(&dir)
        .args(["sample/Mode.java", "sample/Check.java"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new("java")
        .current_dir(&dir)
        .args(["-cp", ".", "sample.Check"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn enum_values_array_from_explicit_stores_preserves_order() {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.truncate(20);
    code.instructions.extend([
        0x2212, 0x2223, 2, 0x0312, 0x004d, 0x0302, 0x1312, 0x014d, 0x0302, 0x0269, 2, 0x000e,
    ]);
    assert!(native_java::render("sample.Mode", &c).is_ok());
    // A repeated zero index leaves an uninitialized array slot and must decline.
    c.methods[0].code.as_mut().unwrap().instructions[26] = 0x0312;
    assert!(native_java::render("sample.Mode", &c).is_err());
}

#[test]
fn exposed_or_mutable_enum_backing_storage_declines() {
    let mut c = fixture();
    c.fields[2].access_flags = 0x19;
    assert!(native_java::render("sample.Mode", &c).is_err());
    let mut c = fixture();
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "expose".into(),
        return_type: "[Lsample/Mode;".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x0062, 2, 0x0011],
        }),
    });
    assert!(native_java::render("sample.Mode", &c).is_err());
}

fn kotlin_fixture() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.strings.extend(
        [
            "$values",
            "$ENTRIES",
            "enumEntries",
            "getEntries",
            "valueOf",
        ]
        .map(String::from),
    );
    symbols.types.extend(
        [
            "Lkotlin/enums/EnumEntries;",
            "Lkotlin/enums/EnumEntriesKt;",
            "[Ljava/lang/Enum;",
            "Ljava/lang/Class;",
        ]
        .map(Into::into),
    );
    symbols.protos.extend([
        ("[Lsample/Mode;".into(), vec![]),
        (
            "Lkotlin/enums/EnumEntries;".into(),
            vec!["[Ljava/lang/Enum;".into()],
        ),
        (
            "Ljava/lang/Enum;".into(),
            vec!["Ljava/lang/Class;".into(), "Ljava/lang/String;".into()],
        ),
    ]);
    symbols
        .methods
        .extend([(0, 0, 5), (0, 2, 7), (4, 3, 9), (1, 4, 11)]);
    symbols.fields.push((0, 3, 8));
    c.fields.push(DexField {
        declaring_type: c.descriptor.clone(),
        name: "$ENTRIES".into(),
        field_type: "Lkotlin/enums/EnumEntries;".into(),
        access_flags: 0x101a,
        is_static: true,
    });
    c.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0022, 0, 0x011a, 0, 0x0212, 0x3070, 2, 0x0210, 0x0069, 0, 0x0122, 0, 0x021a, 1, 0x1312,
        0x3070, 2, 0x0321, 0x0169, 1, 0x0071, 3, 0, 0x000c, 0x0069, 2, 0x001f, 5, 0x1071, 4, 0,
        0x000c, 0x0069, 3, 0x000e,
    ];
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "$values".into(),
        return_type: "[Lsample/Mode;".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x101a,
        code: Some(DexCode {
            registers: 2,
            ins: 0,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x0062, 0, 0x0162, 1, 0x2024, 2, 0x0010, 0x000c, 0x0011],
        }),
    });
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec!["Ljava/lang/String;".into(), "I".into()],
        thrown_types: vec![],
        access_flags: 0x10002,
        code: Some(DexCode {
            registers: 3,
            ins: 3,
            outs: 3,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x3070, 0, 0x0210, 0x000e],
        }),
    });
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "getEntries".into(),
        return_type: "Lkotlin/enums/EnumEntries;".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x0062, 3, 0x0011],
        }),
    });
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "valueOf".into(),
        return_type: c.descriptor.clone(),
        parameters: vec!["Ljava/lang/String;".into()],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 2,
            ins: 1,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x001c, 0, 0x2071, 5, 0x0010, 0x010c, 0x011f, 0, 0x0111],
        }),
    });
    c
}

#[test]
fn kotlin_enum_entries_preserve_constant_and_field_navigation() {
    let c = kotlin_fixture();
    let source = native_java::render("sample.Mode", &c).unwrap();
    assert!(source.source.contains("enum Mode"), "{}", source.source);
    assert!(
        source
            .source
            .contains("$ENTRIES = kotlin.enums.EnumEntriesKt.enumEntries(values())")
    );
    assert!(!source.source.contains("$values("));
    assert!(
        source
            .definitions
            .iter()
            .any(|d| d.name == "$ENTRIES" && d.kind == "field")
    );
}

#[test]
fn kotlin_enum_declines_changed_order_constructor_and_entries_operands() {
    let mut c = kotlin_fixture();
    c.methods[3].code.as_mut().unwrap().instructions[2] = 0x0011;
    assert!(native_java::render("sample.Mode", &c).is_err());
    let mut c = kotlin_fixture();
    c.methods[2].code.as_mut().unwrap().instructions[6] = 0x0001;
    assert!(native_java::render("sample.Mode", &c).is_err());
    let mut c = kotlin_fixture();
    c.methods[0].code.as_mut().unwrap().instructions[30] = 0x0001;
    assert!(native_java::render("sample.Mode", &c).is_err());
    let mut c = kotlin_fixture();
    c.methods[5].code.as_mut().unwrap().instructions[4] = 0x0001;
    assert!(native_java::render("sample.Mode", &c).is_err());
    let mut c = kotlin_fixture();
    Arc::get_mut(&mut c.symbols).unwrap().methods[5].1 = 1;
    assert!(native_java::render("sample.Mode", &c).is_err());
    let mut c = kotlin_fixture();
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "exposeHelper".into(),
        return_type: "[Lsample/Mode;".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 0,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x0071, 3, 0, 0x000c, 0x0011],
        }),
    });
    assert!(native_java::render("sample.Mode", &c).is_err());
}

#[test]
#[ignore = "requires javac and java"]
fn kotlin_enum_jvm_entries_and_values_order() {
    use std::{fs, process::Command};
    let c = kotlin_fixture();
    let source = native_java::render("sample.Mode", &c).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-kotlin-enum-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::create_dir_all(dir.join("kotlin/enums")).unwrap();
    fs::write(dir.join("sample/Mode.java"), source.source).unwrap();
    fs::write(dir.join("kotlin/enums/EnumEntries.java"),
        "package kotlin.enums; public class EnumEntries { public final Enum[] values; EnumEntries(Enum[] v){values=v;} }").unwrap();
    fs::write(dir.join("kotlin/enums/EnumEntriesKt.java"),
        "package kotlin.enums; public class EnumEntriesKt { public static EnumEntries enumEntries(Enum[] v){return new EnumEntries(v);} }").unwrap();
    fs::write(dir.join("sample/Check.java"),
        "package sample; public class Check { public static void main(String[] x) { Mode[] v=Mode.values(); if(v.length!=2||v[0]!=Mode.a||v[1]!=Mode.b||v[0].ordinal()!=0||v[1].ordinal()!=1) throw new AssertionError(); kotlin.enums.EnumEntries e=Mode.getEntries(); if(e!=Mode.getEntries()||e.values[0]!=Mode.a||e.values[1]!=Mode.b) throw new AssertionError(); v[0]=null; if(Mode.values()[0]!=Mode.a||e.values[0]!=Mode.a) throw new AssertionError(); }}").unwrap();
    let compile = Command::new("javac")
        .current_dir(&dir)
        .args([
            "sample/Mode.java",
            "sample/Check.java",
            "kotlin/enums/EnumEntries.java",
            "kotlin/enums/EnumEntriesKt.java",
        ])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .current_dir(&dir)
        .args(["-cp", ".", "sample.Check"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}
