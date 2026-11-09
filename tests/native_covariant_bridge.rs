use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

const PARSER: &str = "Lsample/Parser;";
const PROXY: &str = "Lsample/Proxy;";
const IMAGE: &str = "Lsample/Image;";
const NODE: &str = "Lsample/Node;";
const KEY: &str = "Lsample/Key;";

/// `Parser.parse(Node)` wraps a private `parseInternal` call in a trace scope
/// whose cleanup runs on both exits, the Jackson generated-parser shape, and
/// `javac` adds the covariant `Object parse(Node)` bridge for the erased
/// superclass contract.
fn fixture() -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec![PARSER.into(), PROXY.into(), KEY.into()],
        strings: vec![
            "getInstance".into(),
            "beginTrace".into(),
            "endTrace".into(),
            "parseInternal".into(),
            "parse".into(),
            "Image:Parse".into(),
        ],
        protos: vec![
            (PROXY.into(), vec![]),
            (KEY.into(), vec!["Ljava/lang/String;".into()]),
            ("V".into(), vec![KEY.into()]),
            (IMAGE.into(), vec![NODE.into()]),
            ("Ljava/lang/Object;".into(), vec![NODE.into()]),
        ],
        methods: vec![
            (1, 0, 0),
            (1, 1, 1),
            (1, 2, 2),
            (0, 3, 3),
            (0, 3, 4),
            (0, 4, 4),
        ],
        ..Default::default()
    });
    let class = |descriptor: &str, methods| DexClass {
        descriptor: descriptor.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
        symbols: symbols.clone(),
    };
    let declaration = |name: &str, return_type: &str, parameters: Vec<Arc<str>>, flags| DexMethod {
        declaring_type: PROXY.into(),
        name: name.into(),
        return_type: return_type.into(),
        parameters,
        thrown_types: vec![],
        access_flags: flags,
        code: None,
    };
    let proxy = class(
        PROXY,
        vec![
            declaration("getInstance", PROXY, vec![], 0x109),
            declaration("beginTrace", KEY, vec!["Ljava/lang/String;".into()], 0x101),
            declaration("endTrace", "V", vec![KEY.into()], 0x101),
        ],
    );
    let body = |registers, ins, outs, tries, try_regions, instructions: &[u16]| DexCode {
        registers,
        ins,
        outs,
        tries,
        try_regions,
        instructions: instructions.to_vec(),
        offset: 0,
    };
    let parser = class(
        PARSER,
        vec![
            DexMethod {
                declaring_type: PARSER.into(),
                name: "parseInternal".into(),
                return_type: IMAGE.into(),
                parameters: vec![NODE.into()],
                thrown_types: vec!["Ljava/io/IOException;".into()],
                access_flags: 2,
                code: Some(body(2, 2, 0, 0, vec![], &[0x0012, 0x0011])),
            },
            DexMethod {
                declaring_type: PARSER.into(),
                name: "parse".into(),
                return_type: IMAGE.into(),
                parameters: vec![NODE.into()],
                thrown_types: vec!["Ljava/io/IOException;".into()],
                access_flags: 1,
                code: Some(body(
                    4,
                    2,
                    2,
                    1,
                    vec![DexTryRegion {
                        start: 0xa,
                        end: 0xe,
                        catches: vec![(None, 0x12)].into(),
                    }],
                    &[
                        0x0071, 0, 0, 0x000c, 0x011a, 5, 0x206e, 1, 0x0010, 0x010c, 0x2070, 3,
                        0x0032, 0x020c, 0x206e, 2, 0x0010, 0x0211, 0x020d, 0x206e, 2, 0x0010,
                        0x0227,
                    ],
                )),
            },
            DexMethod {
                declaring_type: PARSER.into(),
                name: "parse".into(),
                return_type: "Ljava/lang/Object;".into(),
                parameters: vec![NODE.into()],
                thrown_types: vec!["Ljava/io/IOException;".into()],
                access_flags: 0x1041,
                code: Some(body(
                    2,
                    2,
                    2,
                    0,
                    vec![],
                    &[0x206e, 4, 0x0010, 0x000c, 0x0011],
                )),
            },
        ],
    );
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&parser, &proxy]).unwrap(),
        ))
        .unwrap();
    parser
}

fn declarations(source: &str, needle: &str) -> usize {
    source
        .lines()
        .filter(|line| line.contains(needle) && line.trim_end().ends_with('{'))
        .count()
}

#[test]
fn covariant_bridge_folds_out_of_class_source_and_frees_the_header() {
    let class = fixture();
    let source = native_java::render("sample.Parser", &class).unwrap().source;
    assert_eq!(declarations(&source, " parse("), 1, "{source}");
    assert!(source.contains("Image parse("), "{source}");
    assert!(!source.contains("Object parse("), "{source}");
    assert!(!source.contains("DEX bridge method"), "{source}");
    assert!(!source.contains(".method "), "{source}");
    assert!(source.contains("parseInternal("), "{source}");
    assert!(source.contains("endTrace("), "{source}");
    assert!(source.contains("throw "), "{source}");
    assert!(native_java::source_header_authority(&class));
}

#[test]
fn standalone_bridge_rendering_is_unchanged() {
    let class = fixture();
    let source = native_java::render_method("sample.Parser", &class, &class.methods[2])
        .unwrap()
        .source;
    assert!(source.contains("DEX bridge method"), "{source}");
    assert!(source.contains("Object parse("), "{source}");
}

#[test]
fn only_exact_forwarding_bodies_prove_a_bridge() {
    let rejected: [&[u16]; 4] = [
        // Not a forward: returns null.
        &[0x0012, 0x0011],
        // Forwards to a different declaration.
        &[0x2070, 3, 0x0010, 0x000c, 0x0011],
        // Receiver and argument swapped.
        &[0x206e, 4, 0x0001, 0x000c, 0x0011],
        // Trailing instruction after the return.
        &[0x206e, 4, 0x0010, 0x000c, 0x0011, 0x0012],
    ];
    for words in rejected {
        let mut class = fixture();
        class.methods[2].code.as_mut().unwrap().instructions = words.to_vec();
        let error = native_java::render("sample.Parser", &class).unwrap_err();
        assert!(
            format!("{error:#}").contains("Duplicate Java method signatures"),
            "{words:x?}: {error:#}"
        );
        assert!(!native_java::source_header_authority(&class));
    }
    for flags in [0x41u32, 0x1001, 0x1049] {
        let mut class = fixture();
        class.methods[2].access_flags = flags;
        assert!(
            native_java::render("sample.Parser", &class).is_err(),
            "flags {flags:x}"
        );
    }
    let mut class = fixture();
    class.methods[2].return_type = IMAGE.into();
    assert!(native_java::render("sample.Parser", &class).is_err());
}
