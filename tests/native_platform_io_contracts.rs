use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn method(name: &str, ret: &str, words: Vec<u16>, registers: u16, outs: u16) -> DexMethod {
    DexMethod {
        declaring_type: "Lsample/Probe;".into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x11,
        code: Some(DexCode {
            registers,
            ins: 1,
            outs,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    }
}

fn raw_fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Probe;".into(),
        superclass: Some("Ljava/io/InputStream;".into()),
        interfaces: vec!["Ljava/io/DataInput;".into()],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec![
                "COUNT".into(),
                "SENTINEL".into(),
                "readInt".into(),
                "readLong".into(),
                "intBitsToFloat".into(),
                "longBitsToDouble".into(),
            ],
            types: vec![
                "Lsample/Probe;".into(),
                "I".into(),
                "Ljava/io/EOFException;".into(),
                "Ljava/lang/Float;".into(),
                "Ljava/lang/Double;".into(),
                "J".into(),
            ],
            fields: vec![(0, 1, 0), (0, 2, 1)],
            protos: vec![
                ("I".into(), vec![]),
                ("F".into(), vec!["I".into()]),
                ("J".into(), vec![]),
                ("D".into(), vec!["J".into()]),
            ],
            methods: vec![(0, 0, 2), (3, 1, 4), (0, 2, 3), (4, 3, 5)],
            ..Default::default()
        }),
        methods: vec![
            method(
                "readInt",
                "I",
                vec![0x0060, 0, 0x00d8, 0x0100, 0x0067, 0, 0x0062, 1, 0x0027],
                2,
                0,
            ),
            method(
                "readFloat",
                "F",
                vec![0x106e, 0, 1, 0x000a, 0x1071, 1, 0, 0x000a, 0x000f],
                2,
                1,
            ),
            method(
                "readDouble",
                "D",
                vec![0x106e, 2, 2, 0x000b, 0x2071, 3, 0x0010, 0x000b, 0x0010],
                3,
                2,
            ),
        ],
    }
}

fn fixture() -> DexClass {
    let class = raw_fixture();
    class
        .symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&class]).unwrap()))
        .unwrap();
    class
}

fn caller_fixture(declares_io: bool, catches_io: bool) -> (DexClass, DexClass) {
    let probe = raw_fixture();
    let caller = DexClass {
        descriptor: "Lsample/Caller;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["readInt".into()],
            types: vec!["Lsample/Probe;".into(), "I".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Caller;".into(),
            name: "call".into(),
            return_type: "V".into(),
            parameters: vec!["Lsample/Probe;".into()],
            thrown_types: if declares_io {
                vec!["Ljava/io/IOException;".into()]
            } else {
                vec![]
            },
            access_flags: 9,
            code: Some(DexCode {
                registers: 1,
                ins: 1,
                outs: 1,
                tries: u16::from(catches_io),
                try_regions: if catches_io {
                    vec![DexTryRegion {
                        start: 0,
                        end: 3,
                        catches: Arc::from([(Some(Arc::from("Ljava/io/IOException;")), 4)]),
                    }]
                } else {
                    vec![]
                },
                instructions: if catches_io {
                    vec![0x106e, 0, 0, 0x000e, 0x000d, 0x000e]
                } else {
                    vec![0x106e, 0, 0, 0x000e]
                },
                offset: 0,
            }),
        }],
    };
    let hierarchy = Arc::new(TypeHierarchy::from_classes([&probe, &caller]).unwrap());
    probe.symbols.hierarchy.set(hierarchy.clone()).unwrap();
    caller.symbols.hierarchy.set(hierarchy).unwrap();
    (probe, caller)
}

fn rendered_methods(class: &DexClass) -> String {
    class
        .methods
        .iter()
        .map(|method| {
            native_java::render_method("sample.Probe", class, method)
                .unwrap()
                .source
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn exact_data_input_contract_declares_callee_and_callers() {
    let class = fixture();
    let source = rendered_methods(&class);
    for signature in ["int readInt()", "float readFloat()", "double readDouble()"] {
        assert!(
            source.contains(&format!("{signature} throws java.io.IOException")),
            "{source}"
        );
    }
    assert!(source.contains("throw"), "{source}");
}

#[test]
fn unhandled_exact_loaded_caller_blocks_new_checked_declaration() {
    let (probe, caller) = caller_fixture(false, false);
    assert!(native_java::render_method("sample.Caller", &caller, &caller.methods[0]).is_ok());
    let error = native_java::render_method("sample.Probe", &probe, &probe.methods[0])
        .unwrap_err()
        .to_string();
    assert!(error.contains("Throw requires"), "{error}");
}

#[test]
fn declared_exact_loaded_caller_allows_checked_declaration() {
    let (probe, _) = caller_fixture(true, false);
    let source = native_java::render_method("sample.Probe", &probe, &probe.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throws java.io.IOException"), "{source}");
}

#[test]
fn exact_loaded_caller_catch_allows_checked_declaration() {
    let (probe, _) = caller_fixture(false, true);
    let source = native_java::render_method("sample.Probe", &probe, &probe.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throws java.io.IOException"), "{source}");
}

#[test]
fn exact_filter_input_stream_contract_crosses_one_sdk_parent_but_loaded_shadow_blocks() {
    let mut class = raw_fixture();
    class.superclass = Some("Ljava/io/FilterInputStream;".into());
    class.interfaces.clear();
    class.methods[0].name = "available".into();
    let hierarchy = Arc::new(TypeHierarchy::from_classes([&class]).unwrap());
    class.symbols.hierarchy.set(hierarchy).unwrap();
    let source = native_java::render_method("sample.Probe", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throws java.io.IOException"), "{source}");

    let mut probe = raw_fixture();
    probe.superclass = Some("Ljava/io/FilterInputStream;".into());
    probe.interfaces.clear();
    probe.methods[0].name = "available".into();
    let shadow = DexClass {
        descriptor: "Ljava/io/FilterInputStream;".into(),
        superclass: Some("Ljava/io/InputStream;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols::default()),
        methods: vec![DexMethod {
            declaring_type: "Ljava/io/FilterInputStream;".into(),
            name: "available".into(),
            return_type: "I".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 1,
            code: None,
        }],
    };
    let hierarchy = Arc::new(TypeHierarchy::from_classes([&probe, &shadow]).unwrap());
    probe.symbols.hierarchy.set(hierarchy).unwrap();
    assert!(native_java::render_method("sample.Probe", &probe, &probe.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn exact_data_input_contract_preserves_caller_effects_and_exception_identity() {
    use std::{fs, process::Command};
    let methods = rendered_methods(&fixture());
    let source = format!(
        r#"package sample;
public final class Probe extends java.io.InputStream implements java.io.DataInput {{
    static final java.io.EOFException SENTINEL = new java.io.EOFException("same");
    static int COUNT;
    {methods}
    public long readLong() throws java.io.IOException {{ COUNT++; throw SENTINEL; }}
    public int read() {{ return -1; }}
    public void readFully(byte[] b) {{ throw new UnsupportedOperationException(); }}
    public void readFully(byte[] b, int o, int n) {{ throw new UnsupportedOperationException(); }}
    public int skipBytes(int n) {{ throw new UnsupportedOperationException(); }}
    public boolean readBoolean() {{ throw new UnsupportedOperationException(); }}
    public byte readByte() {{ throw new UnsupportedOperationException(); }}
    public int readUnsignedByte() {{ throw new UnsupportedOperationException(); }}
    public short readShort() {{ throw new UnsupportedOperationException(); }}
    public int readUnsignedShort() {{ throw new UnsupportedOperationException(); }}
    public char readChar() {{ throw new UnsupportedOperationException(); }}
    public String readLine() {{ throw new UnsupportedOperationException(); }}
    public String readUTF() {{ throw new UnsupportedOperationException(); }}
    public static void main(String[] args) throws Exception {{
        Probe p = new Probe();
        try {{ p.readFloat(); throw new AssertionError("float did not throw"); }}
        catch (java.io.EOFException e) {{ if (e != SENTINEL) throw new AssertionError("float identity"); }}
        try {{ p.readDouble(); throw new AssertionError("double did not throw"); }}
        catch (java.io.EOFException e) {{ if (e != SENTINEL) throw new AssertionError("double identity"); }}
        if (COUNT != 2) throw new AssertionError("effects=" + COUNT);
    }}
}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-platform-io-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Probe.java"), &source).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Probe.java"]),
        ("java", vec!["sample.Probe"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{source}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
