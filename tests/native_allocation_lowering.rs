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

fn captured_reference_argument(expected: &str, with_hierarchy: bool) -> DexClass {
    // new v0; invoke-static {} -> Boolean; move-result v1; init(v0,v1); return v0.
    let mut c = class(
        vec![0x0022, 0, 0x0071, 0, 0, 0x010c, 0x2070, 1, 0x0010, 0x0011],
        vec!["boxed", "<init>"],
        vec![(1, 0, 0), (0, 1, 1)],
    );
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.protos[0] = ("Ljava/lang/Boolean;".into(), vec![]);
    symbols.protos[1] = ("V".into(), vec![expected.into()]);
    if with_hierarchy {
        let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&c]).unwrap();
        c.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    }
    c
}

#[test]
fn captured_reference_widening_keeps_declared_overload_and_single_evaluation() {
    let c = captured_reference_argument("Ljava/lang/Object;", true);
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(
        code.source
            .contains("((java.lang.Object) (v0 = sample.Source.boxed()))"),
        "{}",
        code.source
    );
    assert_eq!(code.source.matches("sample.Source.boxed()").count(), 1);
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "sample.A.<init>(Ljava/lang/Object;)V")
    );
}

#[test]
fn captured_reference_conversion_rejects_unproven_or_missing_hierarchy() {
    for (target, hierarchy) in [
        ("Lunknown/Interface;", true),
        ("Lunknown/Interface;", false),
        ("Ljava/lang/String;", true),
    ] {
        let c = captured_reference_argument(target, hierarchy);
        assert!(
            native_java::render_method("sample.Test", &c, &c.methods[0]).is_err(),
            "{target}"
        );
    }
}

#[test]
fn check_cast_argument_stays_after_allocation_and_preserves_link() {
    let mut c = class(
        vec![0x0022, 0, 0x021f, 1, 0x2070, 0, 0x0020, 0x0011],
        vec!["<init>"],
        vec![(0, 1, 0)],
    );
    Arc::get_mut(&mut c.symbols).unwrap().protos[1] =
        ("V".into(), vec!["Ljava/lang/Object;".into()]);
    c.methods[0].parameters = vec!["Ljava/lang/Object;".into()];
    c.methods[0].code.as_mut().unwrap().ins = 1;
    c.symbols
        .hierarchy
        .set(Arc::new(
            rdx::native_hierarchy::TypeHierarchy::from_classes([&c]).unwrap(),
        ))
        .unwrap();
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(
        code.source
            .contains("new sample.A(((java.lang.Object) (v0 = ((sample.Source) p0))))"),
        "{}",
        code.source
    );
    assert!(code.links.iter().any(|link| link.label == "sample.Source"));
}

#[test]
fn unused_effectful_check_cast_is_retained_under_readable_staging_policy() {
    let mut c = class(
        vec![0x0022, 0, 0x021f, 1, 0x1070, 0, 0, 0x0011],
        vec!["<init>"],
        vec![(0, 1, 0)],
    );
    Arc::get_mut(&mut c.symbols).unwrap().protos[1] = ("V".into(), vec![]);
    c.methods[0].parameters = vec!["Ljava/lang/Object;".into()];
    c.methods[0].code.as_mut().unwrap().ins = 1;
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    let cast = code.source.find("((sample.Source) p0)").unwrap();
    let construct = code.source.find("new sample.A()").unwrap();
    assert!(cast < construct, "{}", code.source);
    assert_eq!(code.source.matches("((sample.Source) p0)").count(), 1);
}

#[test]
fn unrelated_reference_check_cast_uses_nonthrowing_object_bridge() {
    let mut c = class(
        vec![0x0022, 0, 0x021f, 1, 0x2070, 0, 0x0020, 0x0011],
        vec!["<init>"],
        vec![(0, 1, 0)],
    );
    Arc::get_mut(&mut c.symbols).unwrap().protos[1] = ("V".into(), vec!["Lsample/Source;".into()]);
    c.methods[0].parameters = vec!["Ljava/lang/String;".into()];
    c.methods[0].code.as_mut().unwrap().ins = 1;
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(
        code.source
            .contains("new sample.A((v0 = ((sample.Source) ((java.lang.Object) p0))))"),
        "{}",
        code.source
    );
}

#[test]
fn reversed_constructor_cast_arguments_use_ordered_temporary_statements() {
    let mut c = class(
        vec![0x0022, 0, 0x021f, 1, 0x011f, 3, 0x3070, 0, 0x0210, 0x0011],
        vec!["<init>"],
        vec![(0, 1, 0)],
    );
    Arc::get_mut(&mut c.symbols).unwrap().protos[1] = (
        "V".into(),
        vec!["Ljava/lang/String;".into(), "Lsample/Source;".into()],
    );
    c.methods[0].parameters = vec!["Ljava/lang/Object;".into(), "Ljava/lang/Object;".into()];
    c.methods[0].code.as_mut().unwrap().ins = 2;
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(
        code.source
            .contains("sample.Source v0 = ((sample.Source) p1);"),
        "{}",
        code.source
    );
    assert!(
        code.source
            .contains("java.lang.String v1 = ((java.lang.String) p0);"),
        "{}",
        code.source
    );
    assert!(
        code.source.contains("new sample.A(v1, v0)"),
        "{}",
        code.source
    );
    assert!(
        code.source.find("((sample.Source)").unwrap()
            < code.source.find("((java.lang.String)").unwrap()
    );
    assert!(
        code.source.find("((java.lang.String)").unwrap()
            < code.source.find("new sample.A").unwrap()
    );
    for label in ["sample.Source", "java.lang.String"] {
        let link = code.links.iter().find(|link| link.label == label).unwrap();
        let token: String = code
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect();
        assert_eq!(token, label);
    }
}

#[test]
fn object_typed_provider_capture_keeps_explicit_interface_cast() {
    let mut class = class(
        vec![
            0x0022, 0, 0x0071, 0, 0, 0x010c, 0x1072, 1, 1, 0x010a, 0x2070, 2, 0x0010, 0x0011,
        ],
        vec!["provider", "value", "<init>"],
        vec![(1, 5, 0), (1, 0, 1), (0, 1, 2)],
    );
    Arc::get_mut(&mut class.symbols)
        .unwrap()
        .protos
        .push(("Ljava/lang/Object;".into(), vec![]));
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert!(code.source.contains("sample.Source)"), "{}", code.source);
    assert_eq!(code.source.matches(".provider()").count(), 1);
    assert_eq!(code.source.matches(".value()").count(), 1);
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "sample.Source.value()I")
    );
}

#[test]
fn long_allocation_window_remains_bounded() {
    for (length, supported) in [(70, true), (130, true), (260, false)] {
        let mut words = vec![0x0022, 0];
        words.extend(std::iter::repeat_n(0x0112, length));
        words.extend([0x0071, 0, 0, 0x010a, 0x2070, 1, 0x0010, 0x0011]);
        let class = class(words, vec!["value", "<init>"], vec![(1, 0, 0), (0, 1, 1)]);
        let result = native_java::render_method("sample.Test", &class, &class.methods[0]);
        assert_eq!(result.is_ok(), supported, "{result:?}");
    }
}

#[test]
fn object_widening_does_not_require_platform_classes_in_apk() {
    let class = captured_reference_argument("Ljava/lang/Object;", false);
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert_eq!(code.source.matches("sample.Source.boxed()").count(), 1);
    assert!(
        code.links
            .iter()
            .any(|link| link.label == "sample.A.<init>(Ljava/lang/Object;)V")
    );
}

#[test]
fn long_linear_allocation_window_is_bounded_and_preserves_call() {
    fn fixture(padding: usize) -> DexClass {
        let mut words = vec![0x0022, 0, 0x0071, 0, 0, 0x010a];
        words.extend(std::iter::repeat_n(0x0212, padding));
        words.extend([0x2070, 1, 0x0010, 0x0011]);
        class(words, vec!["next", "<init>"], vec![(1, 0, 0), (0, 1, 1)])
    }
    let class = fixture(130);
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    assert_eq!(code.source.matches("sample.Source.next()").count(), 1);
    assert!(code.source.contains("new sample.A("));
    let over_budget = fixture(260);
    assert!(
        native_java::render_method("sample.Test", &over_budget, &over_budget.methods[0]).is_err()
    );
}

#[test]
fn overwritten_cast_in_constructor_arguments_is_still_executed() {
    let mut class = class(
        vec![
            0x0022, 0, 0x0071, 0, 0, 0x010c, 0x011f, 3, 0x1112, 0x2070, 1, 0x0010, 0x0011,
        ],
        vec!["next", "<init>"],
        vec![(1, 0, 0), (0, 1, 1)],
    );
    Arc::get_mut(&mut class.symbols).unwrap().protos[0].0 = "Ljava/lang/Object;".into();
    let code = native_java::render_method("sample.Test", &class, &class.methods[0]).unwrap();
    let call = code.source.find("sample.Source.next()").unwrap();
    let check = code.source.find("((java.lang.String)").unwrap();
    let construct = code.source.find("new sample.A(").unwrap();
    assert!(call < check && check < construct, "{}", code.source);
    assert_eq!(code.source.matches("sample.Source.next()").count(), 1);
}

#[test]
fn constructor_argument_guarded_integer_copy_and_add() {
    // static make(int input): default=1; new A; if input!=0 copy input;
    // subtract one and call A(int). The allocation cannot escape into the guard.
    let words = vec![
        0x1112, 0x0022, 0, 0x0239, 3, 0x0228, 0x2101, 0xf212, 0x21b0, 0x2070, 0, 0x0010, 0x0011,
    ];
    let mut c = class(words, vec!["<init>"], vec![(0, 1, 0)]);
    c.methods[0].parameters = vec!["I".into()];
    c.methods[0].code.as_mut().unwrap().ins = 1;
    let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(code.source.contains("p0 != 0 ? p0 : 1"), "{}", code.source);
    assert!(code.source.contains("+ -1"), "{}", code.source);
    assert_eq!(code.source.matches("new sample.A").count(), 1);
    assert!(code.links.iter().any(|l| l.label == "sample.A.<init>(I)V"));

    // Both branch polarities preserve which path performs the copy.
    c.methods[0].code.as_mut().unwrap().instructions[3] = 0x0238;
    let inverse = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
    assert!(inverse.source.contains("p0 == 0 ? p0 : 1"));

    // Reference copy / use of the uninitialized allocation must stay rejected.
    c.methods[0].code.as_mut().unwrap().instructions[6] = 0x0107;
    assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_err());
}

#[test]
fn filled_array_arguments_preserve_order_in_both_dex_encodings() {
    for array in [[0x2024, 5, 0x0021], [0x0225, 5, 1]] {
        let mut words = vec![0x0022, 0, 0x011a, 1, 0x021a, 2];
        words.extend(array);
        words.extend([0x010c, 0x2070, 0, 0x0010, 0x0011]);
        let mut c = class(words, vec!["<init>", "first", "second"], vec![(0, 0, 0)]);
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.types.push("[Ljava/lang/String;".into());
        symbols.protos[0] = ("V".into(), vec!["[Ljava/lang/String;".into()]);
        let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        assert!(
            code.source.contains("new java.lang.String[]"),
            "{}",
            code.source
        );
        assert!(code.source.find("\"first\"").unwrap() < code.source.find("\"second\"").unwrap());
        assert_eq!(code.source.matches("\"first\"").count(), 1);
        assert_eq!(code.source.matches("\"second\"").count(), 1);
        assert_eq!(code.source.matches("new sample.A").count(), 1);
        Arc::get_mut(&mut c.symbols).unwrap().types[5] = "[J".into();
        assert!(native_java::render_method("sample.Test", &c, &c.methods[0]).is_err());
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.types[5] = "[I".into();
        symbols.protos[0] = ("V".into(), vec!["[I".into()]);
        let mut words = vec![0x0022, 0, 0x1112, 0x2212];
        words.extend(array);
        words.extend([0x010c, 0x2070, 0, 0x0010, 0x0011]);
        c.methods[0].code.as_mut().unwrap().instructions = words;
        let code = native_java::render_method("sample.Test", &c, &c.methods[0]).unwrap();
        assert!(code.source.contains("new int[]"), "{}", code.source);
        assert!(
            code.links
                .iter()
                .all(|link| link.label != "[I" && link.label != "int")
        );
    }
}
