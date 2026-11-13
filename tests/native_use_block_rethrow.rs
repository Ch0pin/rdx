//! Kotlin `use {}` lowering: a catch-all handler rethrows into a second
//! catch-all that closes the resource with the first throwable and rethrows.
//! Every contract keeps the original header and rethrows through the
//! caught-source helper. An unprotected static call declared `throws` ahead
//! of the block is forwarded through the checked-call bridge, so the retry
//! never adds a declaration to the root header.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture(access_flags: u32, instrumented: bool) -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Hooks;".into(),
            "Ljava/io/OutputStream;".into(),
            "[B".into(),
            "Ljava/io/Closeable;".into(),
            "Ljava/lang/Throwable;".into(),
            "Ljava/io/IOException;".into(),
        ],
        strings: vec!["write".into(), "closeFinally".into(), "instrument".into()],
        protos: vec![
            ("V".into(), vec!["[B".into()]),
            (
                "V".into(),
                vec!["Ljava/io/Closeable;".into(), "Ljava/lang/Throwable;".into()],
            ),
            (
                "Ljava/io/OutputStream;".into(),
                vec!["Ljava/io/OutputStream;".into()],
            ),
        ],
        methods: vec![(1, 0, 0), (0, 1, 1), (0, 2, 2)],
        ..Default::default()
    });
    // Instance methods shift the parameters behind `this`; v0 stays the local.
    let instance = access_flags & 8 == 0;
    let stream: u16 = if instance { 2 } else { 1 };
    let bytes = stream + 1;
    // An unprotected static call declared `throws IOException` precedes the
    // try, as instrumentation wrappers do.
    let prefix: Vec<u16> = if instrumented {
        vec![
            // invoke-static {stream}, Hooks.instrument(OutputStream)OutputStream
            0x1071,
            2,
            stream,
            // move-result-object stream
            0x000c | stream << 8,
        ]
    } else {
        vec![]
    };
    let shift = prefix.len() as u32;
    let root = DexClass {
        descriptor: "Lsample/Caller;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Caller;".into(),
            name: "run".into(),
            parameters: vec!["Ljava/io/OutputStream;".into(), "[B".into()],
            return_type: "V".into(),
            thrown_types: vec![],
            access_flags,
            code: Some(DexCode {
                registers: bytes + 1,
                ins: bytes,
                outs: 2,
                tries: 2,
                try_regions: vec![
                    DexTryRegion {
                        start: shift,
                        end: shift + 3,
                        catches: vec![(None, shift + 8)].into(),
                    },
                    DexTryRegion {
                        start: shift + 9,
                        end: shift + 10,
                        catches: vec![(None, shift + 10)].into(),
                    },
                ],
                instructions: prefix
                    .into_iter()
                    .chain([
                        // invoke-virtual {stream, bytes}, OutputStream.write([B)V
                        0x206e,
                        0,
                        bytes << 4 | stream,
                        // const/4 v0, 0
                        0x0012,
                        // invoke-static {stream, v0}, Hooks.closeFinally(Closeable, Throwable)V
                        0x2071,
                        1,
                        stream,
                        // return-void
                        0x000e,
                        // move-exception v0
                        0x000d,
                        // throw v0
                        0x0027,
                        // move-exception bytes
                        0x000d | bytes << 8,
                        // invoke-static {stream, v0}, Hooks.closeFinally(Closeable, Throwable)V
                        0x2071,
                        1,
                        stream,
                        // throw bytes
                        0x0027 | bytes << 8,
                    ])
                    .collect(),
                offset: 0,
            }),
        }],
        symbols: symbols.clone(),
    };
    let hooks = DexClass {
        descriptor: "Lsample/Hooks;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![
            DexMethod {
                declaring_type: "Lsample/Hooks;".into(),
                name: "closeFinally".into(),
                parameters: vec!["Ljava/io/Closeable;".into(), "Ljava/lang/Throwable;".into()],
                return_type: "V".into(),
                access_flags: 0x109,
                code: None,
                thrown_types: vec![],
            },
            DexMethod {
                declaring_type: "Lsample/Hooks;".into(),
                name: "instrument".into(),
                parameters: vec!["Ljava/io/OutputStream;".into()],
                return_type: "Ljava/io/OutputStream;".into(),
                access_flags: 0x109,
                code: None,
                thrown_types: vec!["Ljava/io/IOException;".into()],
            },
        ],
        symbols: symbols.clone(),
    };
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&root, &hooks]).unwrap(),
        ))
        .unwrap();
    root
}

fn source(class: &DexClass) -> String {
    match native_java::render_method("sample.Caller", class, &class.methods[0]) {
        Ok(code) => code.source,
        Err(error) => panic!("{error:#}"),
    }
}

fn header(source: &str) -> &str {
    source.lines().find(|line| line.contains(" run(")).unwrap()
}

#[test]
fn overridable_use_block_close_handler_rethrows_through_helper() {
    let source = source(&fixture(1, false));
    assert!(source.contains("throw _rdx_caught_throw_"), "{source}");
    assert_eq!(source.matches("Hooks.closeFinally(").count(), 2, "{source}");
    assert!(source.contains(".write("), "{source}");
    assert!(!header(&source).contains("throws"), "{source}");
}

#[test]
fn use_block_rethrow_keeps_the_original_header_for_every_contract() {
    for (access_flags, instrumented) in [
        (0x0a, false),
        (0x0a, true),
        (9, false),
        (9, true),
        (1, true),
    ] {
        let source = source(&fixture(access_flags, instrumented));
        assert!(!header(&source).contains("throws"), "{source}");
        assert!(source.contains("throw _rdx_caught_throw_"), "{source}");
        assert_eq!(source.matches("Hooks.closeFinally(").count(), 2, "{source}");
        assert!(source.contains(".write("), "{source}");
        assert_eq!(
            source.matches("_rdx_checked_").count(),
            usize::from(instrumented),
            "{source}"
        );
        assert!(!source.contains("Hooks.instrument("), "{source}");
    }
}

#[test]
fn instrumented_call_is_forwarded_by_a_private_checked_bridge() {
    let class = fixture(0x0a, true);
    let source = native_java::render("sample.Caller", &class).unwrap().source;
    assert!(
        source.contains("private static <RdxCallThrowable"),
        "{source}"
    );
    assert_eq!(source.matches("Hooks.instrument(").count(), 1, "{source}");
    assert!(source.contains("throw _rdx_caught_throw_"), "{source}");
    assert!(!header(&source).contains("throws"), "{source}");
}
