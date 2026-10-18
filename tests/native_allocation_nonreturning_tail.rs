use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn method(
    owner: &str,
    name: &str,
    ret: &str,
    args: &[&str],
    regs: u16,
    ins: u16,
    words: Vec<u16>,
) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        return_type: ret.into(),
        parameters: args.iter().map(|s| (*s).into()).collect(),
        thrown_types: vec![],
        access_flags: if name == "<init>" { 1 } else { 9 },
        code: Some(DexCode {
            registers: regs,
            ins,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }),
    }
}

fn fixture(throwing: bool, loaded: bool, alternate_tail_entry: bool) -> Vec<DexClass> {
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Target;".into(),
            "Lsample/Failures;".into(),
            "Ljava/lang/Object;".into(),
        ],
        strings: vec!["<init>".into(), "fail".into()],
        protos: vec![("V".into(), vec!["I".into()]), ("V".into(), vec![])],
        methods: vec![(0, 0, 0), (1, 1, 1), (2, 1, 0)],
        ..Default::default()
    });
    let class = |owner: &str, methods| DexClass {
        descriptor: owner.into(),
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
    // new-instance precedes both the guarded failure and the constructor.
    let words = vec![
        0x0022,
        0, // @0 new-instance v0, Target
        0x0238,
        if alternate_tail_entry { 5 } else { 7 }, // @2 if-eqz p0 -> @9 / @7
        0x0071,
        1,
        0, // @4 Failures.fail()
        0x0112,
        0x0111, // @7 const v1, 0; return-object v1
        0x1112, // @9 const v1, 1
        0x2070,
        0,
        0x0010, // @10 Target.<init>(v0,v1)
        0x0011, // @13 return-object v0
    ];
    let mut classes = vec![
        class(
            "Lsample/Caller;",
            vec![method(
                "Lsample/Caller;",
                "make",
                "Lsample/Target;",
                &["I"],
                3,
                1,
                words,
            )],
        ),
        class(
            "Lsample/Target;",
            vec![method(
                "Lsample/Target;",
                "<init>",
                "V",
                &["I"],
                2,
                2,
                vec![0x1070, 2, 0, 0x000e],
            )],
        ),
    ];
    if loaded {
        classes.push(class(
            "Lsample/Failures;",
            vec![method(
                "Lsample/Failures;",
                "fail",
                "V",
                &[],
                1,
                0,
                if throwing {
                    vec![0x0012, 0x0027]
                } else {
                    vec![0x000e]
                },
            )],
        ));
    }
    let hierarchy = Arc::new(TypeHierarchy::from_classes(classes.iter()).unwrap());
    symbols.hierarchy.set(hierarchy).unwrap();
    classes
}

#[test]
fn throwing_static_helper_retains_call_inside_original_allocation() {
    let classes = fixture(true, true, false);
    let output = native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0])
        .unwrap()
        .source;
    assert!(output.contains("switch (0)"), "{output}");
    assert!(output.contains("throw null;"), "{output}");
    assert!(
        output.find("new sample.Target").unwrap() < output.find("sample.Failures.fail()").unwrap(),
        "{output}"
    );
    assert_eq!(
        output.matches("sample.Failures.fail()").count(),
        1,
        "{output}"
    );
}

#[test]
fn returning_unknown_and_alternate_entry_helpers_keep_original_return_boundary() {
    for (throwing, loaded, alternate) in [
        (false, true, false),
        (true, false, false),
        (true, true, true),
    ] {
        let classes = fixture(throwing, loaded, alternate);
        let output =
            native_java::render_method("sample.Caller", &classes[0], &classes[0].methods[0]);
        if let Ok(output) = output {
            assert!(!output.source.contains("throw null;"), "{}", output.source);
        }
    }
}
