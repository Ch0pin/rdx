use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture(handler_returns: bool, alias: bool) -> DexClass {
    let symbols = Arc::new(DexSymbols {
        strings: vec!["<init>".into(), "x".into()],
        types: vec![
            "Ljava/lang/Object;".into(),
            "Lsample/Child;".into(),
            "I".into(),
        ],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        fields: vec![(1, 2, 1)],
        ..Default::default()
    });
    let mut words = vec![0x1070, 0, u16::from(alias)];
    let store = if alias {
        words.push(0x1007); // move-object v0, v1 (incoming this is v1)
        words.len()
    } else {
        words.len()
    };
    words.extend([if alias { 0x0259 } else { 0x0159 }, 0]);
    let jump = words.len();
    words.push(0);
    let handler = words.len();
    words.extend([0x010d, if handler_returns { 0x000e } else { 0x0127 }]);
    let ret = words.len();
    words.push(0x000e);
    words[jump] = (((ret - jump) as u16) << 8) | 0x28;
    DexClass {
        descriptor: "Lsample/Child;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![DexField {
            declaring_type: "Lsample/Child;".into(),
            name: "x".into(),
            field_type: "I".into(),
            access_flags: 0x11,
            is_static: false,
        }],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Child;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: if alias { 3 } else { 2 },
                ins: 2,
                outs: 1,
                tries: 1,
                try_regions: vec![DexTryRegion {
                    start: store as u32,
                    end: (store + 2) as u32,
                    catches: vec![(None, handler as u32)].into(),
                }],
                instructions: words,
                offset: 0,
            }),
        }],
        symbols,
    }
}

fn phi_fixture() -> DexClass {
    let mut class = fixture(false, true);
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 4;
    code.instructions = vec![
        0x1070, 0, 2, 0x0338, 4, 0x2007, 0x0228, 0x2007, 0x0359, 0, 0x0328, 0x010d, 0x0127, 0x000e,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 8,
        end: 10,
        catches: vec![(None, 11)].into(),
    }];
    class
}

#[test]
fn protected_final_store_requires_terminal_failure_path() {
    for alias in [false, true] {
        let class = fixture(false, alias);
        let source = native_java::render_method("sample.Child", &class, &class.methods[0])
            .unwrap()
            .source;
        assert!(source.contains("this.x = p0;"), "{source}");
        assert!(source.contains("throw e"), "{source}");
        let class = fixture(true, alias);
        assert!(native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
    }
    let class = phi_fixture();
    assert!(native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}

fn custom_fixture(
    words: Vec<u16>,
    registers: u16,
    store: usize,
    catches: Vec<(Option<Arc<str>>, u32)>,
) -> DexClass {
    let mut class = fixture(false, true);
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = registers;
    code.instructions = words;
    code.try_regions = vec![DexTryRegion {
        start: store as u32,
        end: (store + 2) as u32,
        catches: catches.into(),
    }];
    class
}

fn branch_final_fixture(words: Vec<u16>, try_end: usize, handler: usize) -> DexClass {
    let mut class = fixture(false, true);
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 4;
    code.instructions = words;
    code.try_regions = vec![DexTryRegion {
        start: 3,
        end: try_end as u32,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), handler as u32)].into(),
    }];
    class
}

#[test]
fn mutually_exclusive_protected_final_stores_are_accepted() {
    let class = branch_final_fixture(
        vec![
            0x1070, 0, 2, // invoke-direct {this}, Object.<init>()
            0x0338, 5, // if-eqz p0, else
            0x2359, 0,      // this.x = p0
            0x0428, // goto return
            0x1012, // const/4 v0, 1
            0x2059, 0,      // this.x = v0
            0x000e, // return-void
            0x000d, 0x0027, // catch RuntimeException e; throw e
        ],
        11,
        12,
    );
    let source = native_java::render_method("sample.Child", &class, &class.methods[0])
        .unwrap()
        .source;
    assert_eq!(source.matches("this.x =").count(), 2, "{source}");
    assert!(source.contains("if (p0 == 0)"), "{source}");
    assert!(source.contains("throw e"), "{source}");
}

#[test]
fn partial_branch_final_assignment_is_rejected() {
    let class = branch_final_fixture(
        vec![
            0x1070, 0, 2, 0x0338, 5, 0x2359, 0, 0x0128, 0x000e, 0x000d, 0x0027,
        ],
        8,
        9,
    );
    assert!(native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}

#[test]
fn partial_branch_final_read_is_rejected() {
    let class = branch_final_fixture(
        vec![
            0x1070, 0, 2, 0x0338, 4, 0x2359, 0, 0x2052, 0, 0x000e, 0x000d, 0x0027,
        ],
        9,
        10,
    );
    assert!(native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}

#[test]
fn joined_second_final_assignment_is_rejected() {
    let class = branch_final_fixture(
        vec![
            0x1070, 0, 2, 0x0338, 5, 0x2359, 0, 0x0428, 0x1012, 0x2059, 0, 0x2359, 0, 0x000e,
            0x000d, 0x0027,
        ],
        13,
        14,
    );
    assert!(native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}

#[test]
fn repeated_loop_final_assignment_is_rejected() {
    let class = branch_final_fixture(
        vec![
            0x1070, 0, 2, 0x2359, 0, 0x0339, 0xfffe, 0x000e, 0x000d, 0x0027,
        ],
        8,
        8,
    );
    assert!(native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}

#[test]
fn exceptional_store_proof_follows_shared_return_and_every_handler() {
    let indirect_return = custom_fixture(
        vec![
            0x1070, 0, 2, 0x2107, 0x1007, 0x0359, 0, 0x0328, 0x010d, 0x0128, 0x000e,
        ],
        4,
        5,
        vec![(None, 8)],
    );
    assert!(
        native_java::render_method(
            "sample.Child",
            &indirect_return,
            &indirect_return.methods[0]
        )
        .is_err()
    );

    let one_of_two_returns = custom_fixture(
        vec![
            0x1070, 0, 2, 0x2107, 0x1007, 0x0359, 0, 0x0528, 0x010d, 0x0127, 0x010d, 0x000e, 0x000e,
        ],
        4,
        5,
        vec![(Some("Ljava/lang/Exception;".into()), 8), (None, 10)],
    );
    assert!(
        native_java::render_method(
            "sample.Child",
            &one_of_two_returns,
            &one_of_two_returns.methods[0]
        )
        .is_err()
    );
}

#[test]
fn direct_alias_chain_accepts_this_and_rejects_copied_parameter() {
    let this_alias = custom_fixture(
        vec![
            0x1070, 0, 2, 0x2107, 0x1007, 0x0359, 0, 0x0328, 0x010d, 0x0127, 0x000e,
        ],
        4,
        5,
        vec![(None, 8)],
    );
    let source = native_java::render_method("sample.Child", &this_alias, &this_alias.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("this.x = p0;"), "{source}");

    let parameter_alias = custom_fixture(
        vec![
            0x1070, 0, 2, 0x3007, 0x0359, 0, 0x0328, 0x010d, 0x0127, 0x000e,
        ],
        4,
        4,
        vec![(None, 7)],
    );
    assert!(
        native_java::render_method(
            "sample.Child",
            &parameter_alias,
            &parameter_alias.methods[0]
        )
        .is_err()
    );
}

#[test]
fn exceptional_store_failure_cannot_read_uncommitted_final() {
    let reads_then_rethrows = custom_fixture(
        vec![
            0x1070, 0, 2, 0x2359, 0, 0x0528, 0x000d, 0x2152, 0, 0x0027, 0x000e,
        ],
        4,
        3,
        vec![(None, 6)],
    );
    assert!(
        native_java::render_method(
            "sample.Child",
            &reads_then_rethrows,
            &reads_then_rethrows.methods[0]
        )
        .is_err()
    );
}

#[test]
fn typed_only_handlers_cannot_return_or_read_failed_final() {
    for catch_type in ["Ljava/lang/Throwable;", "Ljava/lang/Exception;"] {
        let returns = custom_fixture(
            vec![
                0x1070, 0, 2, 0x2107, 0x1007, 0x0359, 0, 0x0328, 0x010d, 0x000e, 0x000e,
            ],
            4,
            5,
            vec![(Some(catch_type.into()), 8)],
        );
        assert!(
            native_java::render_method("sample.Child", &returns, &returns.methods[0]).is_err(),
            "typed return accepted for {catch_type}"
        );

        let reads_then_rethrows = custom_fixture(
            vec![
                0x1070, 0, 2, 0x2359, 0, 0x0528, 0x000d, 0x2152, 0, 0x0027, 0x000e,
            ],
            4,
            3,
            vec![(Some(catch_type.into()), 6)],
        );
        assert!(
            native_java::render_method(
                "sample.Child",
                &reads_then_rethrows,
                &reads_then_rethrows.methods[0]
            )
            .is_err(),
            "typed read accepted for {catch_type}"
        );

        let terminal = custom_fixture(
            vec![
                0x1070, 0, 2, 0x2107, 0x1007, 0x0359, 0, 0x0328, 0x010d, 0x0127, 0x000e,
            ],
            4,
            5,
            vec![(Some(catch_type.into()), 8)],
        );
        assert!(
            native_java::render_method("sample.Child", &terminal, &terminal.methods[0]).is_ok(),
            "terminal typed handler rejected for {catch_type}"
        );
    }

    let mixed_typed = custom_fixture(
        vec![
            0x1070, 0, 2, 0x2107, 0x1007, 0x0359, 0, 0x0528, 0x010d, 0x0127, 0x010d, 0x000e, 0x000e,
        ],
        4,
        5,
        vec![
            (Some("Ljava/lang/Exception;".into()), 8),
            (Some("Ljava/lang/Throwable;".into()), 10),
        ],
    );
    assert!(
        native_java::render_method("sample.Child", &mixed_typed, &mixed_typed.methods[0]).is_err()
    );
}

#[test]
fn typed_only_protected_final_metadata_is_bounded() {
    let mut oversized_words = fixture(false, false);
    let code = oversized_words.methods[0].code.as_mut().unwrap();
    code.try_regions[0].catches = vec![(Some("Ljava/lang/Throwable;".into()), 6); 65].into();
    assert!(
        native_java::render_method(
            "sample.Child",
            &oversized_words,
            &oversized_words.methods[0]
        )
        .is_err()
    );

    let mut oversized_code = fixture(false, false);
    oversized_code.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .resize(4097, 0);
    assert!(
        native_java::render_method("sample.Child", &oversized_code, &oversized_code.methods[0])
            .is_err()
    );

    let mut oversized_fields = fixture(false, false);
    for index in 0..256 {
        oversized_fields.fields.push(DexField {
            declaring_type: "Lsample/Child;".into(),
            name: format!("ordinary{index}").into(),
            field_type: "I".into(),
            access_flags: 0x1,
            is_static: false,
        });
    }
    assert!(
        native_java::render_method(
            "sample.Child",
            &oversized_fields,
            &oversized_fields.methods[0]
        )
        .is_err()
    );
}

fn final_reference_prefix_fixture_raw() -> DexClass {
    let symbols = Arc::new(DexSymbols {
        strings: vec![
            "<init>".into(),
            "x".into(),
            "seen".into(),
            "hashCode".into(),
        ],
        types: vec![
            "Ljava/lang/Object;".into(),
            "Lsample/Child;".into(),
            "Ljava/lang/Exception;".into(),
            "I".into(),
        ],
        protos: vec![("V".into(), vec![]), ("I".into(), vec![])],
        methods: vec![(0, 0, 0), (0, 1, 3)],
        fields: vec![(1, 0, 1), (1, 2, 2)],
        ..Default::default()
    });
    DexClass {
        descriptor: "Lsample/Child;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![
            DexField {
                declaring_type: "Lsample/Child;".into(),
                name: "x".into(),
                field_type: "Ljava/lang/Object;".into(),
                access_flags: 0x11,
                is_static: false,
            },
            DexField {
                declaring_type: "Lsample/Child;".into(),
                name: "seen".into(),
                field_type: "Ljava/lang/Exception;".into(),
                access_flags: 1,
                is_static: false,
            },
        ],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Child;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 3,
                ins: 2,
                outs: 1,
                tries: 1,
                try_regions: vec![DexTryRegion {
                    start: 3,
                    end: 8,
                    catches: vec![(Some("Ljava/lang/Exception;".into()), 9)].into(),
                }],
                instructions: vec![
                    0x1070, 0, 1, // super()
                    0x125b, 0, // this.x = p0
                    0x106e, 1, 2,      // p0.hashCode()
                    0x0528, // goto return
                    0x000d, // catch Exception e
                    0x105b, 1,      // this.seen = e
                    0x0128, // goto return
                    0x000e, // return-void
                ],
                offset: 0,
            }),
        }],
        symbols,
    }
}

fn attach_platform_hierarchy(class: DexClass) -> DexClass {
    let hierarchy = TypeHierarchy::from_classes([&class]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    class
}

fn final_reference_prefix_fixture() -> DexClass {
    attach_platform_hierarchy(final_reference_prefix_fixture_raw())
}

fn final_reference_prefix_runtime_fixture() -> DexClass {
    let mut class = final_reference_prefix_fixture_raw();
    let symbols = Arc::get_mut(&mut class.symbols).unwrap();
    symbols.types.push("Lsample/Base;".into());
    symbols.methods[0].0 = (symbols.types.len() - 1) as u16;
    symbols.strings.push("published".into());
    symbols.fields.push((1, 1, 4));
    class.superclass = Some("Lsample/Base;".into());
    class.fields.push(DexField {
        declaring_type: "Lsample/Child;".into(),
        name: "published".into(),
        field_type: "Lsample/Child;".into(),
        access_flags: 0x9,
        is_static: true,
    });
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x1070, 0, 1, // super()
        0x125b, 0, // this.x = p0
        0x106e, 1, 2,      // p0.hashCode()
        0x0628, // goto return
        0x000d, // catch Exception e
        0x105b, 1, // this.seen = e
        0x0169, 2,      // Child.published = this
        0x000e, // return-void
    ];
    code.try_regions[0] = DexTryRegion {
        start: 3,
        end: 8,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 9)].into(),
    };
    attach_platform_hierarchy(class)
}

#[test]
fn own_this_final_reference_prefix_is_outside_typed_try() {
    let class = final_reference_prefix_fixture();
    let source = native_java::render_method("sample.Child", &class, &class.methods[0])
        .unwrap()
        .source;
    assert_eq!(
        source,
        concat!(
            "    public Child(java.lang.Object p0) {\n",
            "        super();\n",
            "        this.x = p0;\n",
            "        try {\n",
            "            p0.hashCode();\n",
            "        } catch (java.lang.Exception e1) {\n",
            "            this.seen = e1;\n",
            "        }\n",
            "        return;\n",
            "    }\n",
        )
    );
}

#[test]
fn ordinary_unprotected_constructor_does_not_require_prefix_hierarchy_or_analysis() {
    let mut class = final_reference_prefix_fixture_raw();
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![0x1070, 0, 1, 0x125b, 0, 0x000e];
    let source = native_java::render_method("sample.Child", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("this.x = p0;"), "{source}");
}

fn assert_final_prefix_certificate_absent(class: &DexClass, label: &str) {
    let Ok(rendered) = native_java::render_method("sample.Child", class, &class.methods[0]) else {
        return;
    };
    let source = rendered.source;
    let try_at = source
        .find("try {")
        .unwrap_or_else(|| panic!("{label}: accepted control lost original try: {source}"));
    let assignment_at = source
        .find(".x =")
        .unwrap_or_else(|| panic!("{label}: accepted control lost x assignment: {source}"));
    let call_at = source
        .find("hashCode(")
        .unwrap_or_else(|| panic!("{label}: accepted control lost protected call: {source}"));
    assert!(
        try_at < assignment_at && assignment_at < call_at,
        "{label}: certificate shifted an unsupported control: {source}"
    );
}

#[test]
fn final_reference_prefix_requires_exact_exception_and_field_shape() {
    let mut throwable = final_reference_prefix_fixture();
    throwable.methods[0].code.as_mut().unwrap().try_regions[0].catches =
        vec![(Some("Ljava/lang/Throwable;".into()), 9)].into();
    assert_final_prefix_certificate_absent(&throwable, "Throwable catch");

    let mut volatile = final_reference_prefix_fixture();
    volatile.fields[0].access_flags |= 0x40;
    assert_final_prefix_certificate_absent(&volatile, "volatile final field");

    let mut static_field = final_reference_prefix_fixture();
    static_field.fields[0].access_flags |= 8;
    static_field.fields[0].is_static = true;
    assert_final_prefix_certificate_absent(&static_field, "static field");

    let mut ambiguous = final_reference_prefix_fixture();
    ambiguous.fields.push(DexField {
        declaring_type: "Lsample/Child;".into(),
        name: "x".into(),
        field_type: "Ljava/lang/Object;".into(),
        access_flags: 0x11,
        is_static: false,
    });
    assert_final_prefix_certificate_absent(&ambiguous, "ambiguous field");

    let mut mismatch = final_reference_prefix_fixture();
    mismatch.fields[0].field_type = "Ljava/lang/Exception;".into();
    assert_final_prefix_certificate_absent(&mismatch, "field descriptor mismatch");

    let mut primitive = final_reference_prefix_fixture();
    primitive.symbols = Arc::new(DexSymbols {
        strings: primitive.symbols.strings.clone(),
        types: primitive.symbols.types.clone(),
        protos: primitive.symbols.protos.clone(),
        methods: primitive.symbols.methods.clone(),
        fields: vec![(1, 3, 1), (1, 2, 2)],
        ..Default::default()
    });
    primitive.fields[0].field_type = "I".into();
    primitive.methods[0].parameters[0] = "I".into();
    let words = &mut primitive.methods[0].code.as_mut().unwrap().instructions;
    words[3] = 0x1259;
    words[7] = 1;
    assert_final_prefix_certificate_absent(&primitive, "primitive field store");

    let mut contradictory_visibility = final_reference_prefix_fixture();
    contradictory_visibility.fields[0].access_flags |= 3;
    assert_final_prefix_certificate_absent(&contradictory_visibility, "contradictory visibility");

    let mut unknown_flags = final_reference_prefix_fixture();
    unknown_flags.fields[0].access_flags |= 0x2000;
    assert_final_prefix_certificate_absent(&unknown_flags, "unknown field flags");

    let mut malformed = final_reference_prefix_fixture_raw();
    let mut types = malformed.symbols.types.clone();
    types.push("Lmissing-semicolon".into());
    let mut fields = malformed.symbols.fields.clone();
    fields[0].1 = (types.len() - 1) as u16;
    malformed.symbols = Arc::new(DexSymbols {
        strings: malformed.symbols.strings.clone(),
        types,
        protos: malformed.symbols.protos.clone(),
        fields,
        methods: malformed.symbols.methods.clone(),
        annotations: malformed.symbols.annotations.clone(),
        ..Default::default()
    });
    malformed.fields[0].field_type = "Lmissing-semicolon".into();
    let hierarchy = TypeHierarchy::from_classes([]).unwrap();
    malformed
        .symbols
        .hierarchy
        .set(Arc::new(hierarchy))
        .unwrap();
    assert_final_prefix_certificate_absent(&malformed, "malformed field descriptor");
}

#[test]
fn final_reference_prefix_requires_unshadowed_platform_exception_identity() {
    let class = final_reference_prefix_fixture_raw();
    let shadow = DexClass {
        descriptor: "Ljava/lang/Exception;".into(),
        superclass: Some("Ljava/lang/Throwable;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    };
    let hierarchy = TypeHierarchy::from_classes([&class, &shadow]).unwrap();
    class.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    assert_final_prefix_certificate_absent(&class, "shadowed platform Exception");
}

#[test]
fn final_reference_prefix_rejects_foreign_and_phi_receivers() {
    let mut foreign = final_reference_prefix_fixture();
    foreign.methods[0].parameters[0] = "Lsample/Child;".into();
    foreign.methods[0].code.as_mut().unwrap().instructions[3] = 0x215b;
    assert!(native_java::render_method("sample.Child", &foreign, &foreign.methods[0]).is_err());

    let mut phi = final_reference_prefix_fixture();
    let code = phi.methods[0].code.as_mut().unwrap();
    code.registers = 4;
    code.instructions = vec![
        0x1070, 0, 2, // super()
        0x0338, 4,      // if-eqz p0, second copy
        0x2007, // move-object v0, this
        0x0228, // goto store
        0x2007, // move-object v0, this
        0x035b, 0, // v0.x = p0 (phi receiver)
        0x106e, 1, 3,      // p0.hashCode()
        0x0628, // goto return
        0x010d, // catch Exception e
        0x215b, 1,      // this.seen = e
        0x0228, // goto return
        0x0000, 0x000e,
    ];
    code.try_regions[0] = DexTryRegion {
        start: 8,
        end: 13,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 14)].into(),
    };
    assert!(native_java::render_method("sample.Child", &phi, &phi.methods[0]).is_err());
}

#[test]
fn final_reference_prefix_rejects_monitor_overlap_and_alternate_entry() {
    let mut monitor = final_reference_prefix_fixture();
    let code = monitor.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x1070, 0, 1, 0x011d, 0x011e, 0x125b, 0, 0x106e, 1, 2, 0x0528, 0x000d, 0x105b, 1, 0x0128,
        0x000e,
    ];
    code.try_regions[0] = DexTryRegion {
        start: 5,
        end: 10,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 11)].into(),
    };
    assert!(native_java::render_method("sample.Child", &monitor, &monitor.methods[0]).is_err());

    let mut overlap = final_reference_prefix_fixture();
    let code = overlap.methods[0].code.as_mut().unwrap();
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 3,
        end: 8,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 9)].into(),
    });
    assert!(native_java::render_method("sample.Child", &overlap, &overlap.methods[0]).is_err());

    let mut alternate = final_reference_prefix_fixture();
    let code = alternate.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x1070, 0, 1, 0x0238, 4, 0x125b, 0, 0x106e, 1, 2, 0x0528, 0x000d, 0x105b, 1, 0x0128, 0x000e,
    ];
    code.try_regions[0] = DexTryRegion {
        start: 5,
        end: 10,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 11)].into(),
    };
    assert!(native_java::render_method("sample.Child", &alternate, &alternate.methods[0]).is_err());

    let mut branch_to_store = final_reference_prefix_fixture();
    let code = branch_to_store.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x1070, 0, 1, 0x0029, 2, 0x125b, 0, 0x106e, 1, 2, 0x0528, 0x000d, 0x105b, 1, 0x0128, 0x000e,
    ];
    code.try_regions[0] = DexTryRegion {
        start: 5,
        end: 10,
        catches: vec![(Some("Ljava/lang/Exception;".into()), 11)].into(),
    };
    assert!(
        native_java::render_method(
            "sample.Child",
            &branch_to_store,
            &branch_to_store.methods[0]
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires javac/java; exact rendered body is checked against a raw-word oracle"]
fn final_reference_prefix_exact_rendered_jvm_oracle() {
    let class = final_reference_prefix_runtime_fixture();
    let method = &class.methods[0];
    let code = method.code.as_ref().unwrap();
    let first_incoming = code.registers - code.ins;
    let this_register = first_incoming;
    let parameter_register = first_incoming + 1;
    let x_pool = class.symbols.fields[code.instructions[4] as usize];
    assert_eq!(
        class.symbols.types[x_pool.0 as usize].as_ref(),
        "Lsample/Child;"
    );
    assert_eq!(
        class.symbols.types[x_pool.1 as usize].as_ref(),
        "Ljava/lang/Object;"
    );
    assert_eq!(class.symbols.strings[x_pool.2 as usize], "x");
    let super_pool = class.symbols.methods[code.instructions[1] as usize];
    assert_eq!(
        class.symbols.types[super_pool.0 as usize].as_ref(),
        "Lsample/Base;"
    );
    assert_eq!(class.symbols.strings[super_pool.2 as usize], "<init>");
    let hash_pool = class.symbols.methods[code.instructions[6] as usize];
    assert_eq!(
        class.symbols.types[hash_pool.0 as usize].as_ref(),
        "Ljava/lang/Object;"
    );
    assert_eq!(class.symbols.strings[hash_pool.2 as usize], "hashCode");
    let seen_pool = class.symbols.fields[code.instructions[11] as usize];
    assert_eq!(
        class.symbols.types[seen_pool.0 as usize].as_ref(),
        "Lsample/Child;"
    );
    assert_eq!(
        class.symbols.types[seen_pool.1 as usize].as_ref(),
        "Ljava/lang/Exception;"
    );
    assert_eq!(class.symbols.strings[seen_pool.2 as usize], "seen");
    let published_pool = class.symbols.fields[code.instructions[13] as usize];
    assert_eq!(
        class.symbols.types[published_pool.0 as usize].as_ref(),
        "Lsample/Child;"
    );
    assert_eq!(
        class.symbols.types[published_pool.1 as usize].as_ref(),
        "Lsample/Child;"
    );
    assert_eq!(
        class.symbols.strings[published_pool.2 as usize],
        "published"
    );
    assert_eq!(code.instructions[2] & 15, this_register);
    assert_eq!(code.instructions[3] >> 12, this_register);
    assert_eq!((code.instructions[3] >> 8) & 15, parameter_register);
    assert_eq!(code.instructions[7] & 15, parameter_register);
    assert_eq!(code.instructions[10] >> 12, this_register);
    assert_eq!((code.instructions[10] >> 8) & 15, 0);
    assert_eq!(code.instructions[12] >> 8, this_register);
    let source = native_java::render_method("sample.Child", &class, method)
        .unwrap()
        .source;
    let directory = std::env::temp_dir().join(format!(
        "rdx-final-prefix-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let package = directory.join("sample");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("Child.java"),
        format!(
            "package sample; public class Child extends Base {{ public final Object x; public Exception seen; public static Child published;\n{source}}}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        package.join("State.java"),
        r#"package sample;
public final class State {
    private State() {}
    public static String events = "";
}
"#,
    )
    .unwrap();
    std::fs::write(
        package.join("Base.java"),
        r#"package sample;
public class Base {
    public Base() { State.events += "B"; }
}
"#,
    )
    .unwrap();
    std::fs::write(
        package.join("MarkerException.java"),
        r#"package sample;
public final class MarkerException extends RuntimeException {
    private static final long serialVersionUID = 1L;
}
"#,
    )
    .unwrap();
    std::fs::write(
        package.join("MarkerError.java"),
        r#"package sample;
public final class MarkerError extends Error {
    private static final long serialVersionUID = 1L;
}
"#,
    )
    .unwrap();
    std::fs::write(
        package.join("Probe.java"),
        r#"package sample;
public final class Probe {
    final RuntimeException exception;
    final Error error;
    Probe(RuntimeException exception, Error error) {
        this.exception = exception;
        this.error = error;
    }
    @Override public int hashCode() {
        State.events += "H";
        if (exception != null) throw exception;
        if (error != null) throw error;
        return 7;
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        package.join("Oracle.java"),
        r#"package sample;
public final class Oracle {
    public static void main(String[] ignored) {
        MarkerException marker = new MarkerException();
        Probe failingProbe = new Probe(marker, null);
        State.events = "S"; Child.published = null;
        Child failure = new Child(failingProbe); State.events += "R";
        System.out.println("failure|" + State.events + "|" + (failure.x == failingProbe)
            + "|" + (failure.seen == marker) + "|" + (Child.published == failure));

        Probe successProbe = new Probe(null, null);
        State.events = "S"; Child.published = null;
        Child success = new Child(successProbe); State.events += "R";
        System.out.println("success|" + State.events + "|" + (success.x == successProbe)
            + "|" + (success.seen == null) + "|" + (Child.published == null));

        State.events = "S"; Child.published = null;
        Child nullValue = new Child(null); State.events += "R";
        System.out.println("null|" + State.events + "|" + (nullValue.x == null)
            + "|" + (nullValue.seen instanceof NullPointerException)
            + "|" + (Child.published == nullValue));

        MarkerError markerError = new MarkerError();
        Probe errorProbe = new Probe(null, markerError);
        State.events = "S"; Child.published = null;
        boolean sameError = false;
        try { new Child(errorProbe); }
        catch (MarkerError error) { sameError = error == markerError; State.events += "E"; }
        System.out.println("error|" + State.events + "|" + sameError
            + "|" + (Child.published == null));

        boolean distinct = true;
        Child previous = null;
        for (int index = 0; index < 32; index++) {
            Probe probe = new Probe(null, null);
            State.events = "S"; Child.published = null;
            Child current = new Child(probe); State.events += "R";
            distinct &= current != previous && current.x == probe && current.seen == null
                && Child.published == null && State.events.equals("SBHR");
            previous = current;
        }
        System.out.println("repeat|32|" + distinct);
    }
}
"#,
    )
    .unwrap();
    let field_pool = class
        .symbols
        .fields
        .iter()
        .map(|&(owner, descriptor, name)| {
            serde_json::json!({
                "owner": class.symbols.types[owner as usize].as_ref(),
                "descriptor": class.symbols.types[descriptor as usize].as_ref(),
                "name": class.symbols.strings[name as usize].as_str(),
            })
        })
        .collect::<Vec<_>>();
    let method_pool = class
        .symbols
        .methods
        .iter()
        .map(|&(owner, prototype, name)| {
            serde_json::json!({
                "owner": class.symbols.types[owner as usize].as_ref(),
                "name": class.symbols.strings[name as usize].as_str(),
                "return": class.symbols.protos[prototype as usize].0.as_ref(),
                "parameters": class.symbols.protos[prototype as usize]
                    .1
                    .iter()
                    .map(AsRef::<str>::as_ref)
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let raw = serde_json::json!({
        "words": &code.instructions,
        "registers": code.registers,
        "ins": code.ins,
        "field_pool": field_pool,
        "method_pool": method_pool,
        "class": class.descriptor.as_ref(),
        "try_start": 3,
        "try_end": 8,
        "handler": 9,
        "catch_type": code.try_regions[0].catches[0].0.as_deref(),
    });
    std::fs::write(
        directory.join("raw.json"),
        serde_json::to_vec_pretty(&raw).unwrap(),
    )
    .unwrap();
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/final_prefix_jvm.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
    println!("evidence {}", directory.display());
}
