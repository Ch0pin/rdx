use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::sync::Arc;

fn class(instructions: Vec<u16>, strings: Vec<&str>, methods: Vec<(u16, u16, u32)>) -> DexClass {
    DexClass {
        symbols: Arc::new(DexSymbols {
            strings: strings.into_iter().map(String::from).collect(),
            types: vec![
                "Lsample/A;".into(),
                "Lsample/Source;".into(),
                "I".into(),
                "Ljava/lang/String;".into(),
                "Lsample/PhoneLaunchActivity;".into(),
            ],
            protos: vec![
                ("I".into(), vec![]),
                ("V".into(), vec!["I".into()]),
                ("V".into(), vec!["Ljava/lang/String;".into()]),
                (
                    "V".into(),
                    vec!["Ljava/lang/String;".into(), "Ljava/lang/String;".into()],
                ),
                (
                    "V".into(),
                    vec![
                        "Ljava/lang/Class;".into(),
                        "Ljava/lang/String;".into(),
                        "Ljava/lang/String;".into(),
                        "I".into(),
                    ],
                ),
            ],
            fields: vec![(1, 2, 0)],
            methods,
            ..Default::default()
        }),
        descriptor: "Lsample/Test;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Test;".into(),
            name: "make".into(),
            return_type: "Lsample/A;".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 0,
                outs: 2,
                tries: 0,
                try_regions: vec![],
                instructions,
                offset: 0,
            }),
        }],
    }
}

fn throwable_class(
    instructions: Vec<u16>,
    strings: Vec<&str>,
    methods: Vec<(u16, u16, u32)>,
) -> DexClass {
    let mut class = class(instructions, strings, methods);
    Arc::get_mut(&mut class.symbols).unwrap().types[0] =
        "Ljava/lang/UnsupportedOperationException;".into();
    class.methods[0].return_type = "Ljava/lang/UnsupportedOperationException;".into();
    class
}

#[test]
fn allocation_precedes_static_field_effect_and_keeps_exact_links() {
    // new v0; sget v1; invoke-direct {v0,v1}; return-object v0
    let class = class(
        vec![0x0022, 0, 0x0160, 0, 0x2070, 0, 0x0010, 0x0011],
        vec!["number", "<init>"],
        vec![(0, 1, 1)],
    );
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source
            .contains("int v0;\n        sample.A v1 = new sample.A((v0 = sample.Source.number));"),
        "{}",
        code.source
    );
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "sample.Source.number:I")
    );
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "sample.A.<init>(I)V")
    );
}

#[test]
fn allocation_precedes_call_and_move_alias_reuses_one_object_local() {
    // new v0; move-object v2,v0; invoke-static {}; move-result v1;
    // invoke-direct {v2,v1}; return-object v0
    let class = class(
        vec![
            0x0022, 0, 0x0207, 0x0071, 0, 0, 0x010a, 0x2070, 1, 0x0012, 0x0011,
        ],
        vec!["f", "<init>"],
        vec![(1, 0, 0), (0, 1, 1)],
    );
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source
            .contains("int v0;\n        sample.A v1 = new sample.A((v0 = sample.Source.f()));"),
        "{}",
        code.source
    );
    assert_eq!(code.source.matches("new sample.A").count(), 1);
    assert!(code.source.contains("return v1;"), "{}", code.source);
}

#[test]
fn effect_that_constructor_does_not_consume_declines_without_partial_output() {
    // The call result is unused. The transactional decoder declines; the legacy
    // path then rejects the intervening call instead of leaking declarations.
    let class = class(
        vec![0x0022, 0, 0x0071, 0, 0, 0x010a, 0x1070, 1, 0x0000, 0x0011],
        vec!["f", "<init>"],
        vec![(1, 0, 0), (0, 1, 1)],
    );
    assert!(native_java::render_method("sample.Test", &class, &class.methods[0]).is_err());
}

#[test]
fn malformed_register_and_empty_constructor_inputs_fail_without_panicking() {
    let malformed_move = class(
        vec![0x0022, 0, 0x0307, 0x1070, 0, 0, 0x0011],
        vec!["number", "<init>"],
        vec![(0, 1, 1)],
    );
    let result = std::panic::catch_unwind(|| {
        native_java::render_method("sample.Test", &malformed_move, &malformed_move.methods[0])
    });
    assert!(result.is_ok_and(|rendered| rendered.is_err()));

    let empty_constructor = class(
        vec![0x0022, 0, 0x0070, 0, 0, 0x0011],
        vec!["number", "<init>"],
        vec![(0, 1, 1)],
    );
    let result = std::panic::catch_unwind(|| {
        native_java::render_method(
            "sample.Test",
            &empty_constructor,
            &empty_constructor.methods[0],
        )
    });
    assert!(result.is_ok_and(|rendered| rendered.is_err()));
}

#[test]
fn consecutive_allocations_keep_prior_effects_unique_names_and_shifted_links() {
    let class = class(
        vec![
            0x0022, 0, 0x0160, 0, 0x2070, 0, 0x0010, 0x0022, 0, 0x0160, 0, 0x2070, 0, 0x0010,
            0x0011,
        ],
        vec!["number", "<init>"],
        vec![(0, 1, 1)],
    );
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert_eq!(code.source.matches("new sample.A").count(), 2);
    assert!(code.source.contains("return v3;"), "{}", code.source);
    let constructors: Vec<_> = code
        .links
        .iter()
        .filter(|link| link.label == "sample.A.<init>(I)V")
        .collect();
    assert_eq!(constructors.len(), 2);
    assert!(constructors[0].end < constructors[1].start);
    for link in constructors {
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "sample.A"
        );
    }
}

#[test]
fn string_resolution_stays_after_allocation_and_alias_is_evaluated_once() {
    // new v0; const-string v1; move-object v2,v1;
    // invoke-direct {v0,v1,v2}, A.<init>(String,String); throw v0
    let class = throwable_class(
        vec![0x0022, 0, 0x011a, 0, 0x1207, 0x3070, 0, 0x0210, 0x0027],
        vec!["Super calls \"unsupported\"\n", "<init>"],
        vec![(0, 3, 1)],
    );
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(
        code.source.contains(
            "java.lang.String v0;\n        java.lang.UnsupportedOperationException v1 = new java.lang.UnsupportedOperationException((v0 = \"Super calls \\\"unsupported\\\"\\n\"), v0);"
        ),
        "{}",
        code.source
    );
    assert_eq!(code.source.matches("Super calls").count(), 1);
    assert!(code.source.contains("throw v1;"), "{}", code.source);
}

#[test]
fn jumbo_string_is_supported_and_bad_string_index_fails_closed() {
    let jumbo = throwable_class(
        vec![0x0022, 0, 0x011b, 0, 0, 0x2070, 0, 0x0010, 0x0027],
        vec!["jumbo", "<init>"],
        vec![(0, 2, 1)],
    );
    let code = native_java::render_method("sample.Test", &jumbo, &jumbo.methods[0]).unwrap();
    assert!(code.source.contains("(v0 = \"jumbo\")"), "{}", code.source);

    let malformed = class(
        vec![0x0022, 0, 0x011a, 99, 0x2070, 0, 0x0010, 0x0027],
        vec!["only", "<init>"],
        vec![(0, 2, 1)],
    );
    let result = std::panic::catch_unwind(|| {
        native_java::render_method("sample.Test", &malformed, &malformed.methods[0])
    });
    assert!(result.is_ok_and(|rendered| rendered.is_err()));
}

#[test]
fn class_literal_resolution_is_ordered_once_and_reused_by_next_allocation() {
    let mut class = class(
        vec![
            0x0022, 0, 0x011c, 4, 0x021a, 0, 0x031a, 1, 0x0412, 0x5470, 0, 0x3210, 0x0522, 0,
            0x021a, 0, 0x031a, 1, 0x0412, 0x5470, 0, 0x3215, 0x0511,
        ],
        vec!["rootLayout", "getRootLayout()Ljava/lang/Object;", "<init>"],
        vec![(0, 4, 2)],
    );
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 6;
    code.outs = 5;
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert_eq!(
        code.source
            .matches("sample.PhoneLaunchActivity.class")
            .count(),
        1
    );
    assert_eq!(code.source.matches("new sample.A").count(), 2);
    assert!(
        code.source.contains(
            "java.lang.Class v0;\n        java.lang.String v1;\n        java.lang.String v2;"
        ),
        "{}",
        code.source
    );
    let class_links: Vec<_> = code
        .links
        .iter()
        .filter(|link| link.label == "sample.PhoneLaunchActivity")
        .collect();
    assert_eq!(class_links.len(), 1);
    assert!(code.source.contains("return v6;"), "{}", code.source);
}

#[test]
fn captured_constructor_arguments_convert_boolean_and_null_literals() {
    // const v2,0; new v0; invoke-static {} number; move-result v1;
    // invoke-direct {v0,v1,v2,v2} <init>(IZObject); return v0.
    let mut c = class(
        vec![
            0x0212, 0x0022, 0, 0x0071, 0, 0, 0x010a, 0x4070, 1, 0x2210, 0x0011,
        ],
        vec!["number", "<init>"],
        vec![(1, 0, 0), (0, 1, 1)],
    );
    Arc::get_mut(&mut c.symbols).unwrap().protos[1] = (
        "V".into(),
        vec!["I".into(), "Z".into(), "Ljava/lang/Object;".into()],
    );
    let result = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(
        result.source.contains("false, ((java.lang.Object) null)"),
        "{}",
        result.source
    );
    assert_eq!(result.source.matches("sample.Source.number()").count(), 1);
}

fn optimized_noarg_allocation(body: Vec<u16>) -> DexClass {
    let mut c = class(
        vec![0x0022, 0, 0x1070, 0, 0, 0x0011],
        vec!["<init>"],
        vec![(1, 0, 0)],
    );
    {
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.types[1] = "Ljava/lang/Object;".into();
        symbols.protos[0] = ("V".into(), vec![]);
    }
    let mut allocated = class(vec![], vec![], vec![]);
    allocated.symbols = Arc::clone(&c.symbols);
    allocated.descriptor = "Lsample/A;".into();
    allocated.methods = vec![DexMethod {
        declaring_type: allocated.descriptor.clone(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x10002,
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: body,
            offset: 0,
        }),
    }];
    c.symbols
        .hierarchy
        .set(Arc::new(
            rdx::native_hierarchy::TypeHierarchy::from_classes([&c, &allocated]).unwrap(),
        ))
        .unwrap();
    c
}

#[test]
fn optimized_empty_constructor_retargets_display_but_keeps_raw_link() {
    let c = optimized_noarg_allocation(vec![0x1070, 0, 0, 0x000e]);
    let result = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(
        result.source.contains("new sample.A()"),
        "{}",
        result.source
    );
    assert!(
        result
            .links
            .iter()
            .any(|link| link.label == "java.lang.Object.<init>()V")
    );
}

#[test]
fn optimized_constructor_with_extra_instructions_is_not_retargeted() {
    let c = optimized_noarg_allocation(vec![0x1070, 0, 0, 0x0000, 0x000e]);
    assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_err());
}
