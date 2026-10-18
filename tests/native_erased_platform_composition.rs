use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::{path::Path, process::Command, sync::Arc};
const SDK: &str = "Landroid/widget/LinearLayout$LayoutParams;";
const COPY: &str = "Landroid/view/ViewGroup$LayoutParams;";
fn class(owner: &str, parent: &str, flags: u32) -> DexClass {
    DexClass {
        descriptor: owner.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: flags,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
        symbols: Arc::new(DexSymbols::default()),
    }
}
#[allow(clippy::too_many_arguments)] // Exact DEX fixture metadata stays explicit at each call.
fn method(
    owner: &str,
    name: &str,
    args: &[&str],
    ret: &str,
    registers: u16,
    ins: u16,
    outs: u16,
    words: &[u16],
) -> DexMethod {
    DexMethod {
        declaring_type: owner.into(),
        name: name.into(),
        parameters: args.iter().map(|a| Arc::from(*a)).collect(),
        return_type: ret.into(),
        thrown_types: vec![],
        access_flags: if name == "<clinit>" {
            0x10008
        } else if name == "<init>" {
            0x10001
        } else {
            9
        },
        code: Some(DexCode {
            registers,
            ins,
            outs,
            tries: 0,
            try_regions: vec![],
            instructions: words.to_vec(),
            offset: 0,
        }),
    }
}
fn fixture() -> Vec<DexClass> {
    let mut bridge = class("Lsample/Bridge;", SDK, 1);
    let mut child = class("Lsample/Child;", "Lsample/Bridge;", 17);
    for (name, ty) in [("flag", "Z"), ("value", "I")] {
        child.fields.push(DexField {
            declaring_type: child.descriptor.clone(),
            name: name.into(),
            field_type: ty.into(),
            access_flags: 1,
            is_static: false,
        });
    }
    for (c, event) in [(&mut bridge, "bridge"), (&mut child, "child")] {
        c.symbols = Arc::new(DexSymbols {
            types: vec!["Lsample/Hooks;".into()],
            strings: vec![event.into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        });
        c.methods.push(method(
            &c.descriptor,
            "<clinit>",
            &[],
            "V",
            0,
            0,
            0,
            &[0x0071, 0, 0, 0x000e],
        ));
    }
    let mut factory = class("Lsample/Factory;", "Ljava/lang/Object;", 1);
    factory.symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Child;".into(),
            "Lsample/Bridge;".into(),
            SDK.into(),
            "Lsample/Hooks;".into(),
            COPY.into(),
        ],
        strings: vec!["<init>".into(), "before".into(), "after".into()],
        protos: vec![
            ("V".into(), vec!["I".into(), "I".into()]),
            ("V".into(), vec![COPY.into()]),
            ("V".into(), vec![]),
            ("V".into(), vec!["Lsample/Child;".into()]),
            ("V".into(), vec!["I".into(), "I".into(), "F".into()]),
        ],
        methods: vec![
            (2, 0, 0),
            (2, 1, 0),
            (3, 2, 1),
            (3, 3, 2),
            (4, 0, 0),
            (2, 4, 0),
        ],
        ..Default::default()
    });
    factory.methods.push(method(
        &factory.descriptor,
        "create",
        &["I", "I"],
        "Lsample/Child;",
        3,
        2,
        3,
        &[
            0x0071, 2, 0, 0x0022, 0, 0x3070, 0, 0x0210, 0x1071, 3, 0, 0x0011,
        ],
    ));
    factory.methods.push(method(
        &factory.descriptor,
        "bridge",
        &["I", "I"],
        "Lsample/Bridge;",
        3,
        2,
        3,
        &[0x0022, 1, 0x3070, 0, 0x0210, 0x0011],
    ));
    factory.methods.push(method(
        &factory.descriptor,
        "copy",
        &[COPY],
        "Lsample/Child;",
        2,
        1,
        2,
        &[
            0x0071, 2, 0, 0x0022, 0, 0x2070, 1, 0x0010, 0x1071, 3, 0, 0x0011,
        ],
    ));
    factory.methods.push(method(
        &factory.descriptor,
        "bridgeCopy",
        &[COPY],
        "Lsample/Bridge;",
        2,
        1,
        2,
        &[0x0022, 1, 0x2070, 1, 0x0010, 0x0011],
    ));
    vec![bridge, child, factory]
}
fn install(c: &[DexClass]) -> Arc<TypeHierarchy> {
    let h = Arc::new(TypeHierarchy::from_classes(c.iter()).unwrap());
    for c in c {
        c.symbols.hierarchy.set(h.clone()).unwrap();
    }
    h
}
fn source(c: &DexClass) -> String {
    native_java::render(
        &c.descriptor
            .trim_start_matches('L')
            .trim_end_matches(';')
            .replace('/', "."),
        c,
    )
    .unwrap()
    .source
}
fn child_records(h: &TypeHierarchy) -> Vec<String> {
    h.recovered_constructors("Lsample/Child;")
        .iter()
        .map(|c| c.parameters.join(""))
        .collect()
}
#[test]
fn exact_observed_sdk_overloads_compose_and_preserve_existing_parent_source() {
    let mut old = fixture();
    old.remove(1);
    install(&old);
    let old_source = source(&old[0]);
    let c = fixture();
    let h = install(&c);
    assert_eq!(child_records(&h), vec!["II", COPY]);
    for r in h.recovered_constructors("Lsample/Child;") {
        assert_eq!(r.parent.as_ref(), "Lsample/Bridge;");
        assert_eq!(r.invoked_owner.as_ref(), SDK);
        assert!(
            r.strict_allocation_order
                && r.observed_allocation
                && !r.observed_super
                && r.thrown_types.is_empty()
        );
    }
    assert_eq!(source(&c[0]), old_source);
    let child = source(&c[1]);
    assert!(child.contains("Child(int p0, int p1)"));
    assert!(child.contains("super(p0, p1);"));
    assert!(child.contains("super(p0);"));
    assert!(!child.contains("this.flag =") && !child.contains("this.value ="));
    for i in [0, 2] {
        let code = native_java::render_method("sample.Factory", &c[2], &c[2].methods[i]).unwrap();
        assert!(code.source.contains("new sample.Child("), "{}", code.source);
        assert!(code.links.iter().any(|l| {
            l.label
                .starts_with("android.widget.LinearLayout$LayoutParams.<init>")
        }));
    }
}
#[test]
fn unrelated_restored_overload_cannot_authorize_missing_signature() {
    let mut c = fixture();
    c[2].methods.retain(|m| m.name.as_ref() != "bridgeCopy");
    c[2].methods.push(method(
        "Lsample/Factory;",
        "weighted",
        &["I", "I", "F"],
        "Lsample/Child;",
        4,
        3,
        4,
        &[0x0022, 0, 0x4070, 5, 0x3210, 0x0011],
    ));
    let h = install(&c);
    assert_eq!(child_records(&h), vec!["II"]);
    assert!(
        native_java::render_method(
            "sample.Factory",
            &c[2],
            c[2].methods
                .iter()
                .find(|m| m.name.as_ref() == "copy")
                .unwrap()
        )
        .is_err()
    );
    assert!(
        native_java::render_method(
            "sample.Factory",
            &c[2],
            c[2].methods
                .iter()
                .find(|m| m.name.as_ref() == "weighted")
                .unwrap()
        )
        .is_err()
    );
}
#[test]
fn restored_forwarders_preserve_after_new_effect_with_timing_lowering() {
    let mut c = fixture();
    c[2].methods.push(method(
        "Lsample/Factory;",
        "effectful",
        &["I", "I"],
        "Lsample/Child;",
        3,
        2,
        3,
        &[0x0022, 0, 0x0071, 2, 0, 0x3070, 0, 0x0210, 0x0011],
    ));
    let h = install(&c);
    assert_eq!(child_records(&h), vec!["II", COPY]);
    let method = c[2]
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "effectful")
        .unwrap();
    let code = native_java::render_method("sample.Factory", &c[2], method).unwrap();
    assert!(code.source.contains("new sample.Child(switch (0)"));
    assert!(code.source.contains("sample.Hooks.before();"));
    assert!(code.source.contains("yield p0;"));
}

#[test]
fn fresh_identity_escape_field_access_self_argument_branch_and_primitive_alias_decline() {
    for mode in 0..7 {
        for reversed in [false, true] {
            let mut c = fixture();
            let (registers, words): (u16, Vec<u16>) = match mode {
                0 => {
                    let symbols = Arc::get_mut(&mut c[2].symbols).unwrap();
                    symbols.types.push("Ljava/lang/Object;".into());
                    symbols.strings.push("use".into());
                    symbols
                        .protos
                        .push(("V".into(), vec!["Ljava/lang/Object;".into()]));
                    symbols.methods.push((3, 5, 3));
                    (
                        3,
                        vec![0x0022, 0, 0x1071, 6, 0x0000, 0x3070, 0, 0x0210, 0x0011],
                    )
                }
                1 | 2 => {
                    let symbols = Arc::get_mut(&mut c[2].symbols).unwrap();
                    symbols.types.push("I".into());
                    symbols.strings.push("value".into());
                    symbols.fields.push((0, 5, 3));
                    let access = if mode == 1 {
                        vec![0x0152, 0]
                    } else {
                        vec![0x1112, 0x0159, 0]
                    };
                    let mut words = vec![0x0022, 0];
                    words.extend(access);
                    words.extend([0x3070, 0, 0x0320, 0x0011]);
                    (4, words)
                }
                3 => (3, vec![0x0022, 0, 0x3070, 0, 0x0010, 0x0011]),
                4 => (3, vec![0x0022, 0, 0x0138, 5, 0x3070, 0, 0x0210, 0x0011]),
                5 => (4, vec![0x0022, 0, 0x0101, 0x3070, 0, 0x0320, 0x0011]),
                6 => (5, vec![0x0022, 0, 0x0104, 0x3070, 0, 0x0430, 0x0011]),
                _ => unreachable!(),
            };
            c[2].methods.push(method(
                "Lsample/Factory;",
                &format!("invalidFresh{mode}"),
                &["I", "I"],
                "Lsample/Child;",
                registers,
                2,
                3,
                &words,
            ));
            if reversed {
                c[2].methods.reverse();
            }
            let h = install(&c);
            assert!(
                child_records(&h).is_empty(),
                "mode {mode}, reversed {reversed}"
            );
            assert_eq!(h.recovered_constructors("Lsample/Bridge;").len(), 2);
        }
    }
}

#[test]
fn own_fields_retained_constructors_owner_access_and_extra_layers_decline() {
    for mode in 0..10 {
        let mut c = fixture();
        match mode {
            0 => c[1].fields[0].access_flags |= 16,
            1 => c[1].fields[0].access_flags |= 64,
            2 => c[1].fields[0].field_type = "Ljava/lang/Object;".into(),
            3 => {
                let parent = c[0].descriptor.clone();
                c[0].fields.push(DexField {
                    declaring_type: parent,
                    name: "state".into(),
                    field_type: "I".into(),
                    access_flags: 1,
                    is_static: false,
                });
            }
            4 => {
                c[1].symbols = Arc::new(clone_symbols(&c[2].symbols));
                c[1].methods.push(method(
                    "Lsample/Child;",
                    "<init>",
                    &[],
                    "V",
                    3,
                    1,
                    3,
                    &[0x0012, 0x0112, 0x3070, 0, 0x0102, 0x000e],
                ));
            }
            5 => {
                c[0].symbols = Arc::new(clone_symbols(&c[2].symbols));
                c[0].methods.push(method(
                    "Lsample/Bridge;",
                    "<init>",
                    &["I", "I"],
                    "V",
                    3,
                    3,
                    3,
                    &[0x0071, 2, 0, 0x3070, 0, 0x0210, 0x000e],
                ));
            }
            6 => c[0].access_flags = 17,
            7 => {
                c[1].superclass = Some("Loutside/Bridge;".into());
                c[0].descriptor = "Loutside/Bridge;".into();
                let parent = c[0].descriptor.clone();
                for m in &mut c[0].methods {
                    m.declaring_type = parent.clone();
                }
            }
            8 => {
                c[0].superclass = Some("Lsample/Child;".into());
            }
            9 => {
                c[1].superclass = Some("Lsample/Middle;".into());
                c.push(class("Lsample/Middle;", "Lsample/Bridge;", 1));
            }
            _ => unreachable!(),
        }
        let h = install(&c);
        assert!(child_records(&h).is_empty(), "mode {mode}");
    }
}
#[test]
fn sdk_endpoint_enclosing_ancestor_and_object_shadows_decline() {
    for shadow in [
        SDK,
        "Landroid/widget/LinearLayout;",
        "Landroid/view/ViewGroup$MarginLayoutParams;",
        COPY,
        "Ljava/lang/Object;",
    ] {
        let mut c = fixture();
        let mut s = class(shadow, "Ljava/lang/Object;", 1);
        if shadow == "Ljava/lang/Object;" {
            s.superclass = None;
        }
        c.push(s);
        let h = install(&c);
        assert!(child_records(&h).is_empty(), "{shadow}");
    }
}
#[test]
fn conflicts_and_unknown_inputs_revoke_only_the_exact_prototype_in_either_order() {
    for bad_outs in [false, true] {
        for reversed in [false, true] {
            let mut c = fixture();
            let mut observation = method(
                "Lsample/Factory;",
                "conflict",
                &["I", "I"],
                "Lsample/Child;",
                3,
                2,
                3,
                &[
                    0x0022,
                    0,
                    0x3070,
                    if bad_outs { 0 } else { 4 },
                    0x0210,
                    0x0011,
                ],
            );
            if bad_outs {
                observation.code.as_mut().unwrap().outs = 2;
            }
            c[2].methods.push(observation);
            if reversed {
                c[2].methods.reverse();
                c.reverse();
            }
            let h = install(&c);
            if bad_outs {
                assert!(child_records(&h).is_empty());
            } else {
                assert_eq!(child_records(&h), vec![COPY]);
            }
            assert!(h.recovered_constructors("Lsample/Bridge;").iter().any(|r| {
                r.parameters.iter().map(|p| p.as_ref()).collect::<Vec<_>>() == vec!["I", "I"]
            }));
        }
    }
}
#[test]
fn malformed_code_duplicate_owner_and_metadata_budgets_decline() {
    for mode in 0..7 {
        let mut c = fixture();
        match mode {
            0 => {
                let m = &mut c[2].methods[0];
                m.code.as_mut().unwrap().ins = 1;
            }
            1 => {
                let m = &mut c[2].methods[0];
                m.code.as_mut().unwrap().instructions.push(0x0014);
            }
            2 => c.push(class("Lsample/Child;", "Lsample/Bridge;", 17)),
            3 => c[1].fields[0].declaring_type = "Lsample/Other;".into(),
            4 => c[1].fields[0].is_static = true,
            5 => c[1].fields[1].name = "flag".into(),
            6 => c[1].fields.extend((0..255).map(|i| DexField {
                declaring_type: "Lsample/Child;".into(),
                name: format!("f{i}").into(),
                field_type: "I".into(),
                access_flags: 1,
                is_static: false,
            })),
            _ => unreachable!(),
        }
        let h = install(&c);
        let records = child_records(&h);
        assert!(records.is_empty(), "mode {mode}: {records:?}");
    }
}
#[test]
fn class_and_observation_order_preserve_parent_and_leaf_emission() {
    let first = fixture();
    let h = install(&first);
    let expected = source(&first[1]);
    let parent = source(&first[0]);
    let records = child_records(&h);
    let mut second = fixture();
    second[2].methods.reverse();
    second.reverse();
    let h = install(&second);
    let leaf = second
        .iter()
        .find(|c| c.descriptor.as_ref() == "Lsample/Child;")
        .unwrap();
    let bridge = second
        .iter()
        .find(|c| c.descriptor.as_ref() == "Lsample/Bridge;")
        .unwrap();
    assert_eq!(child_records(&h), records);
    assert_eq!(source(leaf), expected);
    assert_eq!(source(bridge), parent);
}
fn clone_symbols(s: &DexSymbols) -> DexSymbols {
    DexSymbols {
        types: s.types.clone(),
        strings: s.strings.clone(),
        protos: s.protos.clone(),
        fields: s.fields.clone(),
        methods: s.methods.clone(),
        annotations: s.annotations.clone(),
        ..Default::default()
    }
}
#[test]
fn cfg_and_binding_uncertainty_cannot_hide_behind_old_scanner_preconditions() {
    for mode in 0..5 {
        for reversed in [false, true] {
            let mut c = fixture();
            let mut bad = class("Lsample/Unknown;", "Ljava/lang/Object;", 1);
            bad.symbols = Arc::new(clone_symbols(&c[2].symbols));
            let mut m = method(
                "Lsample/Unknown;",
                "bad",
                &["I", "I"],
                "Lsample/Child;",
                3,
                2,
                3,
                &[0x0022, 0, 0x3070, 0, 0x0210, 0x0011],
            );
            match mode {
                0 => {
                    // The decoder itself owns malformed switch-edge rejection.
                    let mut malformed_switch = method(
                        "Lsample/Unknown;",
                        "badSwitch",
                        &["I", "I"],
                        "Lsample/Child;",
                        3,
                        2,
                        3,
                        &[0x0022, 0, 0x3070, 0, 0x0210, 0x0011],
                    );
                    malformed_switch
                        .code
                        .as_mut()
                        .unwrap()
                        .instructions
                        .extend([0x012b, 4, 0, 0x000e, 0x0100, 1, 0, 0, 0xfffd, 0xffff]);
                    let error = rdx::native_ir::DecodedMethod::decode(
                        malformed_switch.code.as_ref().unwrap(),
                    )
                    .unwrap_err();
                    assert_eq!(
                        error.to_string(),
                        "switch case target is not an instruction boundary at 6"
                    );

                    // Separate decode-valid metadata reaches the CFG validator.
                    let code = m.code.as_mut().unwrap();
                    code.tries = 1;
                    code.try_regions.push(DexTryRegion {
                        start: 1,
                        end: 2,
                        catches: Arc::from([(None, 5)]),
                    });
                    assert!(rdx::native_ir::DecodedMethod::decode(code).is_ok());
                    let error = rdx::native_cfg::ControlFlowGraph::build(code).unwrap_err();
                    assert_eq!(
                        error.to_string(),
                        "try range is not aligned to instruction boundaries"
                    );
                }
                1 => Arc::get_mut(&mut bad.symbols).unwrap().methods[0].2 = 60000, // absent name
                2 => Arc::get_mut(&mut bad.symbols).unwrap().methods[0].1 = 60000, // absent prototype
                3 => Arc::get_mut(&mut bad.symbols).unwrap().protos[0].0 = "I".into(), // nonvoid constructor
                4 => {
                    // The old scanner skips duplicate invoked owners before observe.
                    // The new audit still records conflict against the valid SDK endpoint.
                    m.code.as_mut().unwrap().instructions[3] = 4;
                    let symbols = Arc::get_mut(&mut bad.symbols).unwrap();
                    symbols.types.push("Loutside/Wrong;".into());
                    symbols.methods[4].0 = 5;
                    c.push(class("Loutside/Wrong;", "Ljava/lang/Object;", 1));
                    c.push(class("Loutside/Wrong;", "Ljava/lang/Object;", 1));
                }
                _ => unreachable!(),
            }
            bad.methods.push(m);
            c.push(bad);
            if reversed {
                c.reverse();
            }
            let h = install(&c);
            if mode == 4 {
                assert_eq!(child_records(&h), vec![COPY]);
            } else {
                assert!(child_records(&h).is_empty(), "mode {mode}");
            }
            assert_eq!(h.recovered_constructors("Lsample/Bridge;").len(), 2);
        }
    }
}
#[test]
fn conflicting_wrapper_observations_cannot_authorize_leaf_in_either_order() {
    for reversed in [false, true] {
        let mut c = fixture();
        c[2].methods.push(method(
            "Lsample/Factory;",
            "wrongBridge",
            &["I", "I"],
            "Lsample/Bridge;",
            3,
            2,
            3,
            &[0x0022, 1, 0x3070, 4, 0x0210, 0x0011],
        ));
        if reversed {
            c[2].methods.reverse();
            c.reverse();
        }
        let h = install(&c);
        assert_eq!(child_records(&h), vec![COPY]);
        assert_eq!(h.recovered_constructors("Lsample/Bridge;").len(), 2); // old source authority retained
    }
}
#[test]
fn total_budget_exhaustion_discards_new_records_in_every_order() {
    for reversed in [false, true] {
        let mut c = fixture();
        for i in 0..257 {
            c[2].methods.push(method(
                "Lsample/Factory;",
                &format!("extra{i}"),
                &["I", "I"],
                "Lsample/Child;",
                3,
                2,
                3,
                &[0x0022, 0, 0x3070, 0, 0x0210, 0x0011],
            ));
        }
        if reversed {
            c[2].methods.reverse();
            c.reverse();
        }
        let h = install(&c);
        assert!(child_records(&h).is_empty());
        assert_eq!(h.recovered_constructors("Lsample/Bridge;").len(), 2);
    }
}
#[test]
#[ignore = "requires JDK25; root runs bounded SDK-model oracle serially"]
fn unchanged_generated_chain_and_callers_preserve_overload_fault_and_effect_order() {
    let mut c = fixture();
    c[2].methods.push(method(
        "Lsample/Factory;",
        "effectful",
        &["I", "I"],
        "Lsample/Child;",
        3,
        2,
        3,
        &[0x0022, 0, 0x0071, 2, 0, 0x3070, 0, 0x0210, 0x0011],
    ));
    install(&c);
    let directory =
        std::env::temp_dir().join(format!("rdx-erased-platform-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    for (name, class) in [("Bridge", &c[0]), ("Child", &c[1]), ("Factory", &c[2])] {
        std::fs::write(directory.join(format!("{name}.java")), source(class)).unwrap();
    }
    let factory = std::fs::read_to_string(directory.join("Factory.java")).unwrap();
    assert!(
        factory.contains("import android.view.ViewGroup;"),
        "{factory}"
    );
    assert!(
        factory.contains("copy(ViewGroup.LayoutParams "),
        "{factory}"
    );
    assert!(!factory.contains("ViewGroup$LayoutParams"), "{factory}");
    for (name, index) in [("create", 0), ("copy", 2), ("effectful", 4)] {
        std::fs::write(
            directory.join(format!("{name}.fragment")),
            native_java::render_method("sample.Factory", &c[2], &c[2].methods[index])
                .unwrap()
                .source,
        )
        .unwrap();
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/erased_platform_composition_jvm.py");
    let output = Command::new("python3")
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
