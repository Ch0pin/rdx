use rdx::native_dex::{
    DexAnnotation, DexAnnotationDirectory, DexClass, DexCode, DexMethod, DexSymbols, DexValue,
};
use rdx::native_java;
use std::{collections::BTreeMap, sync::Arc};

fn set(annotation: DexAnnotation) -> Arc<[Arc<DexAnnotation>]> {
    Arc::from(vec![Arc::new(annotation)])
}

fn span(source: &str, start: usize, end: usize) -> String {
    source.chars().skip(start).take(end - start).collect()
}

fn annotated_class() -> DexClass {
    let marker = set(DexAnnotation {
        visibility: 1,
        type_idx: 3,
        elements: vec![],
    });
    let values = set(DexAnnotation {
        visibility: 1,
        type_idx: 4,
        elements: vec![
            (1, DexValue::String(2)),
            (5, DexValue::Boolean(true)),
            (6, DexValue::Array(vec![DexValue::Int(1), DexValue::Int(2)])),
            (7, DexValue::Char(10)),
        ],
    });
    let parameter: Arc<[Arc<DexAnnotation>]> = Arc::from(vec![
        Arc::new(DexAnnotation {
            visibility: 0,
            type_idx: 6,
            elements: vec![],
        }),
        Arc::new(DexAnnotation {
            visibility: 1,
            type_idx: 7,
            elements: vec![(1, DexValue::MethodHandle(0))],
        }),
    ]);
    let directory = DexAnnotationDirectory {
        class: Some(values),
        fields: vec![],
        methods: vec![Some(marker)],
        parameters: vec![vec![Some(parameter)]],
    };
    DexClass {
        symbols: Arc::new(DexSymbols {
            strings: vec![
                "unused".into(),
                "value".into(),
                "bridge".into(),
                "Landroid/webkit/JavascriptInterface;".into(),
                "Lsample/Config;".into(),
                "enabled".into(),
                "numbers".into(),
                "newline".into(),
                "Lsample/Parameter;".into(),
                "Lsample/Unsupported;".into(),
            ],
            types: vec![
                "V".into(),
                "Lsample/Target;".into(),
                "Ljava/lang/Object;".into(),
                "Landroid/webkit/JavascriptInterface;".into(),
                "Lsample/Config;".into(),
                "I".into(),
                "Lsample/Parameter;".into(),
                "Lsample/Unsupported;".into(),
                "Ljava/lang/String;".into(),
                "Lsample/Parameter;".into(),
            ],
            annotations: BTreeMap::from([(1, Arc::new(directory))]),
            ..Default::default()
        }),
        descriptor: "Lsample/Target;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 1,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Target;".into(),
            name: "send".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 2,
                ins: 2,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x000e],
                offset: 1,
            }),
        }],
    }
}

#[test]
fn renders_class_method_parameter_annotations_values_imports_and_spans() {
    let class = annotated_class();
    let code = native_java::render("sample.Target", &class).unwrap();
    assert!(
        code.source
            .contains("import android.webkit.JavascriptInterface;"),
        "{}",
        code.source
    );
    assert!(
        code.source.contains("@JavascriptInterface\n"),
        "{}",
        code.source
    );
    assert!(code.source.contains("public void send("), "{}", code.source);
    assert!(code.source.contains("@Parameter "), "{}", code.source);
    assert!(
        code.source
            .contains("DEX parameter annotations are retained"),
        "{}",
        code.source
    );
    assert!(
        code.source.contains(
            "DEX annotation retained but cannot be expressed as Java: @sample.Unsupported"
        ),
        "{}",
        code.source
    );
    assert!(code.source.contains("newline = '\\n'"), "{}", code.source);
    assert!(code.source.contains("numbers = {1, 2}"), "{}", code.source);
    let method = code
        .definitions
        .iter()
        .find(|definition| definition.kind == "method")
        .unwrap();
    assert_eq!(span(&code.source, method.start, method.end), "send");
    let link = code
        .links
        .iter()
        .find(|link| link.label == "android.webkit.JavascriptInterface")
        .unwrap();
    assert_eq!(
        span(&code.source, link.start, link.end),
        "JavascriptInterface"
    );
}

#[test]
fn aliased_annotation_names_keep_original_navigation_identity() {
    let mut class = annotated_class();
    Arc::get_mut(&mut class.symbols).unwrap().types[3] = "Lsample/Bad-Marker;".into();
    let code = native_java::render("sample.Target", &class).unwrap();
    let link = code
        .links
        .iter()
        .find(|link| link.label == "sample.Bad-Marker")
        .unwrap();
    assert_eq!(
        span(&code.source, link.start, link.end),
        "_rdx_4261642d4d61726b6572"
    );
    assert!(!code.source.contains("@Bad-Marker"));
}
