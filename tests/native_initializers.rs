use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn fixture(instructions: Vec<u16>) -> DexClass {
    DexClass {
        descriptor: "Lsample/Init;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["count".into()],
            types: vec!["Lsample/Init;".into(), "I".into()],
            fields: vec![(0, 1, 0)],
            ..Default::default()
        }),
        fields: vec![DexField {
            declaring_type: "Lsample/Init;".into(),
            name: "count".into(),
            field_type: "I".into(),
            access_flags: 9,
            is_static: true,
        }],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Init;".into(),
            name: "<clinit>".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 0x10008,
            code: Some(DexCode {
                registers: 1,
                ins: 0,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions,
                offset: 0,
            }),
        }],
    }
}

#[test]
fn initializer_block_omits_only_terminal_return_and_preserves_exact_links() {
    let class = fixture(vec![0x1012, 0x0067, 0, 0x000e]);
    let code = native_java::render("sample.Init", &class).unwrap();
    assert!(
        code.source
            .contains("    static {\n        Init.count = 1;\n    }"),
        "{}",
        code.source
    );
    assert!(!code.source.contains("return;"));
    assert!(!code.source.contains("void <clinit>"));
    let declaration = code
        .definitions
        .iter()
        .find(|d| d.name == "<clinit>")
        .unwrap();
    let span = |start: usize, end: usize| {
        code.source
            .chars()
            .skip(start)
            .take(end - start)
            .collect::<String>()
    };
    assert_eq!(span(declaration.start, declaration.end), "static");
    assert!(code.links.iter().any(|link| link.start == declaration.start
        && link.end == declaration.end
        && link.label == "sample.Init.<clinit>()V"));
    let field_links: Vec<_> = code
        .links
        .iter()
        .filter(|link| link.label == "sample.Init.count:I")
        .collect();
    assert_eq!(field_links.len(), 2); // declaration and the one write
    assert!(
        field_links
            .iter()
            .all(|link| span(link.start, link.end) == "count")
    );
    assert_eq!(code.source_hash, rdx::engine::source_identity(&code.source));
}

#[test]
fn initializer_allows_branches_that_share_one_terminal_return() {
    // Both arms assign a register, then the shared tail performs one static write.
    let class = fixture(vec![
        0x0012, 0x0038, 4, 0x1012, 0x0228, 0x2012, 0x0067, 0, 0x000e,
    ]);
    let code = native_java::render_method("sample.Init", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("if ("));
    assert_eq!(code.source.matches("sample.Init.count =").count(), 1);
    assert!(!code.source.contains("return;"));
}

#[test]
fn initializer_rejects_early_return_regions_and_invalid_signatures() {
    let class = fixture(vec![0x0012, 0x0038, 3, 0x000e, 0x000e]);
    assert!(native_java::render_method("sample.Init", &class, &class.methods[0]).is_err());
    for case in 0..7 {
        let mut class = fixture(vec![0x000e]);
        let method = &mut class.methods[0];
        match case {
            0 => method.access_flags = 0x10000,
            1 => method.return_type = "I".into(),
            2 => method.parameters.push("I".into()),
            3 => method.code = None,
            4 => method.access_flags |= 0x100,
            5 => method.access_flags |= 1,
            _ => method.declaring_type = "Lother/Class;".into(),
        }
        assert!(
            native_java::render_method("sample.Init", &class, &class.methods[0]).is_err(),
            "case {case}"
        );
    }
}

#[test]
fn empty_static_initializer_is_an_empty_java_block() {
    let class = fixture(vec![0x000e]);
    let code = native_java::render_method("sample.Init", &class, &class.methods[0]).unwrap();
    assert_eq!(code.source, "    static {\n    }\n");
}
