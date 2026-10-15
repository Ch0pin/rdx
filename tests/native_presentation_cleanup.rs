//! Final Java presentation removes only syntax that Java supplies implicitly.

use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn method(
    name: &str,
    flags: u32,
    registers: u16,
    ins: u16,
    outs: u16,
    words: Vec<u16>,
) -> DexMethod {
    DexMethod {
        declaring_type: Arc::from("Lsample/Presentation;"),
        name: Arc::from(name),
        return_type: Arc::from("V"),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: flags,
        code: Some(DexCode {
            registers,
            ins,
            outs,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    }
}

fn fixture() -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols {
            strings: vec!["<init>".into()],
            types: vec![Arc::from("Ljava/lang/Object;")],
            protos: vec![(Arc::from("V"), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
        descriptor: Arc::from("Lsample/Presentation;"),
        superclass: Some(Arc::from("Ljava/lang/Object;")),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![
            method("<init>", 0x10001, 1, 1, 1, vec![0x1070, 0, 0, 0x000e]),
            method("clear", 9, 0, 0, 0, vec![0x000e]),
        ],
    }
}

fn object_return_fixture() -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: Arc::from("Lsample/ObjectReturn;"),
        superclass: Some(Arc::from("Ljava/lang/Object;")),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: Arc::from("Lsample/ObjectReturn;"),
            name: Arc::from("identity"),
            return_type: Arc::from("Ljava/lang/Object;"),
            parameters: vec![Arc::from("Lsample/Response;")],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 1,
                ins: 1,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x0011],
                offset: 0,
            }),
        }],
    }
}

#[test]
fn full_presentation_omits_implicit_constructor_and_void_boilerplate() {
    let class = fixture();
    let raw_constructor =
        native_java::render_method("sample.Presentation", &class, &class.methods[0]).unwrap();
    let raw_void =
        native_java::render_method("sample.Presentation", &class, &class.methods[1]).unwrap();
    assert!(raw_constructor.source.contains("        super();\n"));
    assert!(raw_constructor.source.ends_with("        return;\n    }\n"));
    assert!(raw_void.source.ends_with("        return;\n    }\n"));

    let presented = native_java::render("sample.Presentation", &class).unwrap();
    assert!(
        presented
            .source
            .contains("    public Presentation() {\n    }\n")
    );
    assert!(
        presented
            .source
            .contains("    public static void clear() {\n    }\n")
    );
    assert!(!presented.source.contains("super();"));
    assert!(!presented.source.contains("return;"));
    assert!(!presented.links.iter().any(|link| {
        link.label.ends_with(".<init>()V")
            && presented
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .eq("super".chars())
    }));
    for definition in &presented.definitions {
        assert_eq!(
            presented
                .source
                .chars()
                .skip(definition.start)
                .take(definition.end - definition.start)
                .collect::<String>(),
            if definition.kind == "class" || definition.name == "<init>" {
                "Presentation"
            } else {
                "clear"
            }
        );
    }
}

#[test]
fn full_presentation_keeps_append_effect_without_unused_result_binding() {
    let mut class = object_return_fixture();
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["append".into()],
        types: vec!["Ljava/lang/StringBuilder;".into()],
        protos: vec![(
            "Ljava/lang/StringBuilder;".into(),
            vec!["Ljava/lang/String;".into()],
        )],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "append".into();
    method.return_type = "V".into();
    method.parameters = vec![
        "Ljava/lang/StringBuilder;".into(),
        "Ljava/lang/String;".into(),
    ];
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 2;
    code.outs = 2;
    code.instructions = vec![0x206e, 0, 0x0021, 0x000c, 0x000e];
    let raw = native_java::render_method("sample.ObjectReturn", &class, &class.methods[0]).unwrap();
    assert!(
        raw.source
            .contains("java.lang.StringBuilder v0 = p0.append(p1);"),
        "{}",
        raw.source
    );
    let presented = native_java::render("sample.ObjectReturn", &class).unwrap();
    assert!(
        presented
            .source
            .contains("        stringBuilder.append(text);"),
        "{}",
        presented.source
    );
    assert!(!presented.source.contains(" = "), "{}", presented.source);
    let link = presented
        .links
        .iter()
        .find(|link| {
            link.label
                == "java.lang.StringBuilder.append(Ljava/lang/String;)Ljava/lang/StringBuilder;"
        })
        .unwrap();
    assert_eq!(
        presented
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>(),
        "append"
    );
}

#[test]
fn full_presentation_removes_only_the_redundant_object_return_cast() {
    let class = object_return_fixture();
    let raw = native_java::render_method("sample.ObjectReturn", &class, &class.methods[0]).unwrap();
    assert!(
        raw.source.contains("return ((java.lang.Object) p0);"),
        "{}",
        raw.source
    );
    let presented = native_java::render("sample.ObjectReturn", &class).unwrap();
    // The cast cleanup runs while the emitter-owned name is still `p0`;
    // display-name cleanup then presents the surviving parameter as `response`.
    assert!(
        presented.source.contains("return response;"),
        "{}",
        presented.source
    );
    assert!(
        !presented.source.contains("return ((java.lang.Object)"),
        "{}",
        presented.source
    );
}

#[test]
fn full_presentation_returns_a_fresh_object_directly() {
    let mut class = fixture();
    class.methods[0] = method("create", 9, 1, 0, 1, vec![0x0022, 0, 0x1070, 0, 0, 0x0011]);
    class.methods[0].return_type = "Ljava/lang/Object;".into();
    let raw = native_java::render_method("sample.Presentation", &class, &class.methods[0]).unwrap();
    assert!(
        raw.source.contains(" = new java.lang.Object();"),
        "{}",
        raw.source
    );
    let presented = native_java::render("sample.Presentation", &class).unwrap();
    assert!(
        presented.source.contains("return new Object();"),
        "{}",
        presented.source
    );
    let link = presented
        .links
        .iter()
        .find(|link| link.label == "java.lang.Object.<init>()V")
        .unwrap();
    assert_eq!(
        presented
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect::<String>(),
        "Object"
    );
}
