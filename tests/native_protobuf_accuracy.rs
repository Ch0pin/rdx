//! Boolean register merges feeding protobuf byte-state fields.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

#[test]
fn merged_boolean_state_is_written_as_byte_and_boxed_without_losing_bits() {
    let class = DexClass {
        descriptor: "Lsample/Message;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["state".into(), "valueOf".into()],
            types: vec![
                "Lsample/Message;".into(),
                "B".into(),
                "Ljava/lang/Byte;".into(),
            ],
            fields: vec![(0, 1, 0)],
            methods: vec![(2, 0, 1)],
            protos: vec![("Ljava/lang/Byte;".into(), vec!["B".into()])],
            ..Default::default()
        }),
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Message;".into(),
            name: "update".into(),
            return_type: "Ljava/lang/Object;".into(),
            parameters: vec!["I".into(), "Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 4,
                ins: 3,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                // if object is non-null: state=1; else state=0; then byte store/read/box.
                instructions: vec![
                    0x0338, 4, 0x1012, 0x0228, 0x0012, 0x105d, 0, 0x1056, 0, 0x1071, 0, 0, 0x000c,
                    0x0011,
                ],
            }),
        }],
    };
    let code = native_java::render_method("sample.Message", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("(byte) ("), "{}", code.source);
    assert!(code.source.contains("? 1 : 0"), "{}", code.source);
    assert!(
        code.source.contains("java.lang.Byte.valueOf"),
        "{}",
        code.source
    );
    assert!(
        code.links
            .iter()
            .any(|l| l.label == "sample.Message.state:B")
    );
    assert!(
        code.links
            .iter()
            .any(|l| l.label == "java.lang.Byte.valueOf(B)Ljava/lang/Byte;")
    );
    assert_eq!(code.source_hash, rdx::engine::source_identity(&code.source));
}

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Play Store APK and run --ignored"]
fn reported_brgk_dynamic_method_reconstructs_protobuf_state_and_schema() {
    use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeDexEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let class = engine.class("brgk").expect("reported class brgk");
    let method = class
        .methods
        .iter()
        .find(|m| {
            m.name.as_ref() == "mw"
                && m.parameters.iter().map(AsRef::as_ref).collect::<Vec<_>>()
                    == ["I", "Ljava/lang/Object;"]
        })
        .unwrap();
    let code =
        native_java::render_method("brgk", class, method).expect("brgk.mw Java reconstruction");
    assert!(!code.source.contains(".end method"));
    assert!(code.source.contains("? 1 : 0"), "{}", code.source);
    for symbol in ["brgk.F:B", "java.lang.Byte.valueOf(B)Ljava/lang/Byte;"] {
        assert!(
            code.links.iter().any(|l| l.label == symbol),
            "missing {symbol}: {}",
            code.source
        );
    }
    assert!(
        code.links
            .iter()
            .any(|l| l.label.starts_with("cauj.<init>(")),
        "schema constructor missing: {}",
        code.source
    );
    assert!(
        code.source.contains("new java.lang.Object[]"),
        "schema array missing: {}",
        code.source
    );
    let schema_line = code
        .source
        .lines()
        .find(|line| line.contains("new java.lang.Object[]"))
        .unwrap();
    let schema_values = schema_line
        .split_once('{')
        .unwrap()
        .1
        .strip_suffix("};")
        .unwrap();
    assert_eq!(
        schema_values.split(", ").count(),
        37,
        "protobuf schema element count changed"
    );
    assert!(schema_values.starts_with("\"b\", \"c\", \"d\", \"e\""));
    assert!(schema_values.ends_with("\"B\", \"C\", \"D\", \"E\""));
    assert_eq!(code.source_hash, rdx::engine::source_identity(&code.source));
    for link in &code.links {
        assert!(link.start < link.end && link.end <= code.source.chars().count());
    }
    if let Some(output) = std::env::var_os("RDX_PROTOBUF_OUTPUT") {
        std::fs::write(output, &code.source).unwrap();
    }
}
