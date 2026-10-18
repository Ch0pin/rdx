use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(
    param: &str,
    declared: &[&str],
    nested: bool,
    replacement: bool,
) -> (DexClass, DexMethod) {
    let m = DexMethod {
        declaring_type: "Lsample/Example;".into(),
        name: "run".into(),
        return_type: "V".into(),
        parameters: vec![param.into()],
        thrown_types: declared.iter().map(|s| (*s).into()).collect(),
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 0,
            tries: if nested { 2 } else { 1 },
            try_regions: if nested {
                vec![
                    DexTryRegion {
                        start: 0,
                        end: 1,
                        catches: vec![(None, 1)].into(),
                    },
                    DexTryRegion {
                        start: 1,
                        end: 3,
                        catches: vec![(None, 3)].into(),
                    },
                ]
            } else {
                vec![DexTryRegion {
                    start: 0,
                    end: 1,
                    catches: vec![(None, 1)].into(),
                }]
            },
            instructions: if nested {
                vec![0x0027, 0x000d, 0x0027, 0x000d, 0x0027]
            } else if replacement {
                vec![0x0027, 0x000d, 0x0012, 0x0027]
            } else {
                vec![0x0027, 0x000d, 0x0027]
            },
            offset: 0,
        }),
    };
    let c = DexClass {
        symbols: Arc::new(DexSymbols::default()),
        descriptor: "Lsample/Example;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![],
    };
    (c, m)
}
// A caught-object bridge changes source legality, never the DEX checked
// contract or the caller's original declared header.
fn caught_bridge(c: &DexClass, m: &DexMethod) -> String {
    let source = native_java::render_method("sample.Example", c, m)
        .unwrap()
        .source;
    assert!(source.contains("throw _rdx_caught_throw_"), "{source}");
    let header = source.lines().find(|line| line.contains(" run(")).unwrap();
    let expected = m
        .thrown_types
        .iter()
        .map(|ty| {
            ty.trim_start_matches('L')
                .trim_end_matches(';')
                .replace('/', ".")
        })
        .collect::<Vec<_>>()
        .join(", ");
    if expected.is_empty() {
        assert!(!header.contains(" throws "), "{header}");
    } else {
        assert!(
            header.ends_with(&format!(" throws {expected} {{")),
            "{header}"
        );
    }
    source
}
fn checked_contract(
    c: &DexClass,
    owner: &str,
    name: &str,
    args: &[&str],
    ret: &str,
    static_call: bool,
) {
    let args = args
        .iter()
        .map(|ty| Arc::<str>::from(*ty))
        .collect::<Vec<_>>();
    assert_eq!(
        c.symbols
            .hierarchy
            .get()
            .unwrap()
            .call_checked_effects(owner, name, &args, ret, static_call)
            .unwrap(),
        vec![Arc::<str>::from("Ljava/io/IOException;")]
    );
}
#[test]
fn caught_checked_parameter_keeps_original_caller_header() {
    let (c, m) = fixture("Ljava/lang/Throwable;", &[], false, false);
    caught_bridge(&c, &m);
}
#[test]
fn nested_caught_parameter_keeps_original_caller_header() {
    let (c, m) = fixture("Ljava/lang/Throwable;", &[], true, false);
    caught_bridge(&c, &m);
}
#[test]
fn ordinary_unchecked_rethrow_retains_identity() {
    let (c, m) = fixture("Ljava/lang/RuntimeException;", &[], false, false);
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("throw e"), "{s}");
}
#[test]
fn declared_checked_rethrow_retains_identity() {
    let (c, m) = fixture(
        "Ljava/io/IOException;",
        &["Ljava/io/IOException;"],
        false,
        false,
    );
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("throw e"), "{s}");
}
#[test]
fn catch_replacement_null_retains_original_throw() {
    let (c, m) = fixture("Ljava/lang/Throwable;", &[], false, true);
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("throw null"), "{s}");
}

fn call_fixture(constructor: bool, declares: bool) -> (DexClass, DexMethod) {
    let (c, mut m) = fixture(
        "Ljava/lang/Throwable;",
        if declares {
            &["Ljava/io/IOException;"]
        } else {
            &[]
        },
        false,
        false,
    );
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Api;".into()],
        strings: vec![if constructor {
            "<init>".into()
        } else {
            "work".into()
        }],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let api_method = DexMethod {
        declaring_type: "Lsample/Api;".into(),
        name: if constructor {
            "<init>".into()
        } else {
            "work".into()
        },
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec!["Ljava/io/IOException;".into()],
        access_flags: if constructor { 1 } else { 9 },
        code: None,
    };
    let api = DexClass {
        symbols: symbols.clone(),
        descriptor: "Lsample/Api;".into(),
        methods: vec![api_method],
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
    };
    let hierarchy = rdx::native_hierarchy::TypeHierarchy::from_classes([&api]).unwrap();
    symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    let c = DexClass { symbols, ..c };
    m.parameters.clear();
    let code = m.code.as_mut().unwrap();
    code.ins = 0;
    code.instructions = if constructor {
        vec![0x0022, 0, 0x1070, 0, 0, 0x000e, 0x000d, 0x0027]
    } else {
        vec![0x0071, 0, 0, 0x000e, 0x000d, 0x0027]
    };
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: if constructor { 5 } else { 3 },
        catches: vec![(None, if constructor { 6 } else { 4 })].into(),
    }];
    (c, m)
}
#[test]
fn declared_checked_method_rethrow_requires_contract() {
    let (c, m) = call_fixture(false, false);
    checked_contract(&c, "Lsample/Api;", "work", &[], "V", true);
    caught_bridge(&c, &m);
}
#[test]
fn declared_checked_constructor_rethrow_requires_contract() {
    let (c, m) = call_fixture(true, false);
    checked_contract(&c, "Lsample/Api;", "<init>", &[], "V", false);
    caught_bridge(&c, &m);
}
#[test]
fn declared_checked_method_contract_allows_original_identity() {
    let (c, m) = call_fixture(false, true);
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("throw e"), "{s}");
}
#[test]
fn declared_checked_constructor_contract_allows_original_identity() {
    let (c, m) = call_fixture(true, true);
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("throw e"), "{s}");
}

#[test]
fn broad_throw_narrow_first_rethrow_requires_its_checked_contract() {
    let (c, mut m) = fixture("Ljava/lang/Throwable;", &[], false, false);
    let code = m.code.as_mut().unwrap();
    code.instructions = vec![0x0027, 0x000d, 0x0027, 0x000d, 0x000e];
    code.try_regions[0].catches = vec![(Some("Ljava/io/IOException;".into()), 1), (None, 3)].into();
    caught_bridge(&c, &m);
}

#[test]
fn declared_narrow_catch_uses_exact_typed_identity() {
    let (c, mut m) = fixture(
        "Ljava/lang/Throwable;",
        &["Ljava/io/IOException;"],
        false,
        false,
    );
    let code = m.code.as_mut().unwrap();
    code.instructions = vec![0x0027, 0x000d, 0x0027, 0x000d, 0x000e];
    code.try_regions[0].catches = vec![(Some("Ljava/io/IOException;".into()), 1), (None, 3)].into();
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("throw e"), "{s}");
    assert!(!s.contains("throw caught"), "{s}");
}
#[test]
#[ignore = "requires javac and java"]
fn jvm_exact_checked_and_unchecked_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-checked-effects-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    for mode in 0..11 {
        let (mut c, m) = match mode {
            0 => fixture("Ljava/lang/RuntimeException;", &[], false, false),
            1 => fixture(
                "Ljava/io/IOException;",
                &["Ljava/io/IOException;"],
                false,
                false,
            ),
            2 => call_fixture(false, true),
            3 => call_fixture(true, true),
            4 => fixture("Ljava/lang/Throwable;", &[], false, true),
            5 => fixture("Ljava/lang/Throwable;", &[], false, false),
            6 => fixture("Ljava/lang/Throwable;", &[], true, false),
            7 => call_fixture(false, false),
            8 => call_fixture(true, false),
            9 => {
                let (c, mut m) = immediate_refinement(false);
                m.code.as_mut().unwrap().instructions[4] = 0x0139;
                (c, m)
            }
            _ => {
                let (c, mut m) = immediate_refinement(false);
                let code = m.code.as_mut().unwrap();
                code.instructions[7] = 0x0029;
                code.instructions.push(0xffff);
                (c, m)
            }
        };
        c.methods.push(m);
        let source = native_java::render("sample.Example", &c).unwrap().source;
        let sub = dir.join(mode.to_string());
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("Example.java"), source.clone()).unwrap();
        std::fs::write(sub.join("Api.java"),"package sample; public class Api { static java.io.IOException fault; public Api() throws java.io.IOException { throw fault; } public static void work() throws java.io.IOException { throw fault; } }").unwrap();
        let (fault, call) = match mode {
            0 => ("new RuntimeException()", "Example.run(fault)"),
            1 => ("new java.io.IOException()", "Example.run(fault)"),
            2 | 3 | 7 | 8 => ("new java.io.IOException()", "Example.run()"),
            5 | 6 | 10 => ("new java.io.IOException()", "Example.run(fault)"),
            _ => ("new RuntimeException()", "Example.run(fault)"),
        };
        let expected = if mode == 4 {
            "if (!(got instanceof NullPointerException) || got == fault) throw new AssertionError();"
        } else {
            "if (got != fault) throw new AssertionError();"
        };
        let bind = if matches!(mode, 2 | 3 | 7 | 8) {
            "Api.fault = fault;"
        } else {
            ""
        };
        std::fs::write(sub.join("Harness.java"),format!("package sample; public class Harness {{ public static void main(String[] args) throws Throwable {{ var fault={fault}; {bind} Throwable got=null; try {{ {call}; }} catch(Throwable e) {{ got=e; }} {expected} }} }}")).unwrap();
        let o = std::process::Command::new("javac")
            .arg("-d")
            .arg(&sub)
            .args([
                sub.join("Example.java"),
                sub.join("Api.java"),
                sub.join("Harness.java"),
            ])
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{source}\n{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let o = std::process::Command::new("java")
            .arg("-cp")
            .arg(&sub)
            .arg("sample.Harness")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
}

fn inherited_fixture(shadow: bool) -> (DexClass, DexMethod) {
    let (mut c, mut m) = call_fixture(false, false);
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Api;".into(), "Lsample/Sub;".into()],
        strings: vec!["work".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    let mut api = fixture("Ljava/lang/Throwable;", &[], false, false).0;
    api.symbols = symbols.clone();
    api.descriptor = "Lsample/Api;".into();
    let method = DexMethod {
        declaring_type: "Lsample/Api;".into(),
        name: "work".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec!["Ljava/io/IOException;".into()],
        access_flags: 9,
        code: None,
    };
    api.methods = vec![method];
    let mut sub = fixture("Ljava/lang/Throwable;", &[], false, false).0;
    sub.symbols = symbols.clone();
    sub.descriptor = "Lsample/Sub;".into();
    sub.superclass = Some("Lsample/Api;".into());
    if shadow {
        sub.methods = vec![DexMethod {
            declaring_type: "Lsample/Sub;".into(),
            name: "work".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: None,
        }];
    }
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&api, &sub]).unwrap();
    symbols.hierarchy.set(Arc::new(h)).unwrap();
    c.symbols = symbols;
    m.thrown_types.clear();
    (c, m)
}
#[test]
fn inherited_static_checked_contract_is_retained() {
    let (c, m) = inherited_fixture(false);
    checked_contract(&c, "Lsample/Sub;", "work", &[], "V", true);
    caught_bridge(&c, &m);
}
#[test]
fn loaded_empty_shadow_stops_inherited_checked_contract() {
    let (c, m) = inherited_fixture(true);
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("Sub.work"), "{s}");
}
#[test]
fn static_instance_contract_modes_remain_distinct() {
    let (c, _) = call_fixture(false, false);
    let h = c.symbols.hierarchy.get().unwrap();
    assert!(h
        .call_checked_effects("Lsample/Api;", "work", &[], "V", false)
        .is_some_and(|types| types
            .iter()
            .all(|ty| ty.as_ref() == "Ljava/lang/Throwable;")));
    assert_eq!(
        h.call_checked_effects("Lsample/Api;", "work", &[], "V", true)
            .unwrap()
            .len(),
        1
    );
}

fn argument_fixture(staged: bool, declared: bool) -> (DexClass, DexMethod) {
    let (mut c, mut m) = call_fixture(true, declared);
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Api;".into()],
        strings: vec!["arg".into(), "<init>".into()],
        protos: vec![("I".into(), vec![]), ("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let mut api = fixture("Ljava/lang/Throwable;", &[], false, false).0;
    api.symbols = symbols.clone();
    api.descriptor = "Lsample/Api;".into();
    api.methods = vec![
        DexMethod {
            declaring_type: "Lsample/Api;".into(),
            name: "arg".into(),
            return_type: "I".into(),
            parameters: vec![],
            thrown_types: vec!["Ljava/io/IOException;".into()],
            access_flags: 9,
            code: None,
        },
        DexMethod {
            declaring_type: "Lsample/Api;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec!["I".into()],
            thrown_types: vec![],
            access_flags: 1,
            code: None,
        },
    ];
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&api]).unwrap();
    symbols.hierarchy.set(Arc::new(h)).unwrap();
    c.symbols = symbols;
    let code = m.code.as_mut().unwrap();
    code.registers = if staged { 3 } else { 2 };
    code.ins = if staged { 1 } else { 0 };
    code.instructions = if staged {
        vec![
            0x0022, 0, 0x0238, 7, 0x0071, 0, 0, 0x010a, 0x0228, 0x7112, 0x2070, 1, 0x0010, 0x000e,
            0x000d, 0x0027,
        ]
    } else {
        vec![
            0x0022, 0, 0x0071, 0, 0, 0x010a, 0x2070, 1, 0x0010, 0x000e, 0x000d, 0x0027,
        ]
    };
    if staged {
        m.parameters = vec!["Z".into()];
    } else {
        m.parameters.clear();
    }
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: if staged { 13 } else { 9 },
        catches: vec![(None, if staged { 14 } else { 10 })].into(),
    }];
    (c, m)
}
#[test]
fn checked_constructor_argument_call_requires_rethrow_contract() {
    for staged in [false, true] {
        let (c, m) = argument_fixture(staged, false);
        checked_contract(&c, "Lsample/Api;", "arg", &[], "I", true);
        caught_bridge(&c, &m);
    }
}
#[test]
fn declared_constructor_argument_effect_survives_staging() {
    for staged in [false, true] {
        let (c, m) = argument_fixture(staged, true);
        let s = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        assert!(s.contains("throw e"), "staged={staged} {s}");
    }
}

fn interface_fixture(shadow: bool) -> (DexClass, DexMethod) {
    let (mut c, mut m) = call_fixture(false, false);
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Checked;".into(), "Lsample/Sub;".into()],
        strings: vec!["work".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    let mut interface = fixture("Ljava/lang/Throwable;", &[], false, false).0;
    interface.symbols = symbols.clone();
    interface.descriptor = "Lsample/Checked;".into();
    interface.access_flags = 0x601;
    interface.methods = vec![DexMethod {
        declaring_type: "Lsample/Checked;".into(),
        name: "work".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec!["Ljava/io/IOException;".into()],
        access_flags: 0x401,
        code: None,
    }];
    let mut sub = fixture("Ljava/lang/Throwable;", &[], false, false).0;
    sub.symbols = symbols.clone();
    sub.descriptor = "Lsample/Sub;".into();
    sub.interfaces = vec!["Lsample/Checked;".into()];
    if shadow {
        sub.methods = vec![DexMethod {
            declaring_type: "Lsample/Sub;".into(),
            name: "work".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 1,
            code: None,
        }];
    }
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&interface, &sub]).unwrap();
    symbols.hierarchy.set(Arc::new(h)).unwrap();
    c.symbols = symbols;
    m.parameters = vec!["Lsample/Sub;".into()];
    let code = m.code.as_mut().unwrap();
    code.ins = 1;
    code.instructions[0] = 0x106e;
    (c, m)
}
#[test]
fn inherited_interface_checked_contract_is_preserved() {
    let (c, m) = interface_fixture(false);
    checked_contract(&c, "Lsample/Sub;", "work", &[], "V", false);
    caught_bridge(&c, &m);
}
#[test]
fn empty_loaded_interface_override_stops_checked_contract() {
    let (c, m) = interface_fixture(true);
    assert!(native_java::render_method("sample.Example", &c, &m).is_ok());
}

fn receiver_argument_fixture(
    staged: bool,
    narrowed: bool,
    declared: bool,
) -> (DexClass, DexMethod) {
    let (mut c, mut m) = argument_fixture(staged, declared);
    let owner = if narrowed { 2 } else { 1 };
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Api;".into(),
            "Lsample/Base;".into(),
            "Lsample/Sub;".into(),
        ],
        strings: vec!["arg".into(), "<init>".into()],
        protos: vec![("I".into(), vec![]), ("V".into(), vec!["I".into()])],
        methods: vec![(owner, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let method = |owner: &str, throws: Vec<std::sync::Arc<str>>| DexMethod {
        declaring_type: owner.into(),
        name: "arg".into(),
        return_type: "I".into(),
        parameters: vec![],
        thrown_types: throws,
        access_flags: 1,
        code: None,
    };
    let mut api = fixture("Ljava/lang/Throwable;", &[], false, false).0;
    api.symbols = symbols.clone();
    api.descriptor = "Lsample/Api;".into();
    api.methods = vec![DexMethod {
        declaring_type: "Lsample/Api;".into(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec!["I".into()],
        thrown_types: vec![],
        access_flags: 1,
        code: None,
    }];
    let mut base = fixture("Ljava/lang/Throwable;", &[], false, false).0;
    base.symbols = symbols.clone();
    base.descriptor = "Lsample/Base;".into();
    base.methods = vec![method(
        "Lsample/Base;",
        vec!["Ljava/io/IOException;".into()],
    )];
    let mut sub = fixture("Ljava/lang/Throwable;", &[], false, false).0;
    sub.symbols = symbols.clone();
    sub.descriptor = "Lsample/Sub;".into();
    sub.superclass = Some("Lsample/Base;".into());
    sub.methods = vec![method("Lsample/Sub;", vec![])];
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&api, &base, &sub]).unwrap();
    symbols.hierarchy.set(Arc::new(h)).unwrap();
    c.symbols = symbols;
    m.parameters = vec![if narrowed {
        "Lsample/Base;".into()
    } else {
        "Lsample/Sub;".into()
    }];
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    let pc = if staged { 4 } else { 2 };
    code.instructions[pc] = 0x106e;
    code.instructions[pc + 2] = 2;
    if narrowed {
        code.instructions.splice(0..0, [0x021f, 2]);
        code.try_regions[0].end += 2;
        code.try_regions[0].catches = vec![(None, if staged { 16 } else { 12 })].into();
    }
    (c, m)
}
#[test]
fn staged_receiver_cast_retains_broader_source_contract() {
    for staged in [false, true] {
        let (c, m) = receiver_argument_fixture(staged, false, false);
        checked_contract(&c, "Lsample/Base;", "arg", &[], "I", false);
        let source = caught_bridge(&c, &m);
        assert!(source.contains("sample.Base"), "{source}");
        let (c, m) = receiver_argument_fixture(staged, false, true);
        let s = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        assert!(s.contains("sample.Base"), "{s}");
    }
}
#[test]
fn staged_receiver_cast_retains_empty_narrower_override() {
    for staged in [false, true] {
        let (c, m) = receiver_argument_fixture(staged, true, false);
        let s = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        assert!(s.contains("sample.Sub"), "{s}");
    }
}

fn immediate_refinement(declares: bool) -> (DexClass, DexMethod) {
    let (mut c, mut m) = fixture(
        "Ljava/lang/Throwable;",
        if declares {
            &["Ljava/io/IOException;"]
        } else {
            &[]
        },
        false,
        false,
    );
    c.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/io/IOException;".into()],
        ..DexSymbols::default()
    });
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.instructions = vec![0x0227, 0x000d, 0x0120, 0, 0x0138, 3, 0x0027, 0x000e];
    (c, m)
}
#[test]
fn immediate_positive_catch_type_retains_checked_identity() {
    let (c, m) = immediate_refinement(true);
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("throw ((java.io.IOException) e"), "{s}");
}
#[test]
fn immediate_positive_catch_type_requires_checked_contract() {
    let (c, m) = immediate_refinement(false);
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn immediate_positive_catch_type_rejects_different_throw_identity() {
    let (c, mut m) = immediate_refinement(true);
    m.code.as_mut().unwrap().instructions[6] = 0x0227;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}

#[test]
fn immediate_catch_false_edge_does_not_refine() {
    let (c, mut m) = immediate_refinement(true);
    m.code.as_mut().unwrap().instructions[4] = 0x0139;
    let source = caught_bridge(&c, &m);
    assert!(!source.contains("throw ((java.io.IOException)"), "{source}");
}
#[test]
fn immediate_catch_tested_register_write_does_not_refine() {
    let (c, mut m) = immediate_refinement(true);
    m.code.as_mut().unwrap().instructions[2] = 0x0020;
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn immediate_catch_foreign_entry_does_not_refine() {
    let (c, mut m) = immediate_refinement(true);
    let code = m.code.as_mut().unwrap();
    code.instructions[7] = 0x0029;
    code.instructions.push((-1i16) as u16);
    let source = caught_bridge(&c, &m);
    assert!(!source.contains("throw ((java.io.IOException)"), "{source}");
}

#[test]
fn partial_sdk_inferred_contract_retains_unknown_explicit_throw() {
    let (mut api, _) = fixture("Ljava/lang/Throwable;", &[], false, false);
    api.descriptor = "Lsample/Api;".into();
    api.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/Class;".into()],
        strings: vec!["forName".into(), "sample.Missing".into()],
        protos: vec![(
            "Ljava/lang/Class;".into(),
            vec!["Ljava/lang/String;".into()],
        )],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    api.methods = vec![DexMethod {
        declaring_type: api.descriptor.clone(),
        name: "work".into(),
        return_type: "V".into(),
        parameters: vec!["Ljava/io/IOException;".into()],
        thrown_types: vec![],
        access_flags: 10,
        code: Some(DexCode {
            registers: 2,
            ins: 1,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x001a, 1, 0x1071, 0, 0, 0x0127],
            offset: 0,
        }),
    }];
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&api]).unwrap();
    assert_eq!(
        h.call_checked_effects(
            "Lsample/Api;",
            "work",
            &["Ljava/io/IOException;".into()],
            "V",
            true
        )
        .unwrap(),
        vec![Arc::<str>::from("Ljava/lang/Throwable;")]
    );
}
#[test]
fn same_parameter_covariant_declarations_remain_ambiguous() {
    let (mut api, mut m) = fixture("Ljava/lang/Throwable;", &[], false, false);
    api.descriptor = "Lsample/Api;".into();
    m.declaring_type = api.descriptor.clone();
    m.name = "work".into();
    m.parameters.clear();
    m.code = None;
    m.return_type = "Ljava/lang/Object;".into();
    m.thrown_types = vec!["Ljava/io/IOException;".into()];
    let second = DexMethod {
        declaring_type: api.descriptor.clone(),
        name: "work".into(),
        parameters: vec![],
        access_flags: 9,
        code: None,
        return_type: "Ljava/lang/String;".into(),
        thrown_types: vec![],
    };
    api.methods = vec![m, second];
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&api]).unwrap();
    assert_eq!(
        h.call_checked_effects("Lsample/Api;", "work", &[], "Ljava/lang/String;", true)
            .unwrap(),
        vec![Arc::<str>::from("Ljava/lang/Throwable;")]
    );
}

#[test]
fn exhausted_loaded_ancestry_remains_unknown() {
    let classes: Vec<_> = (0..65)
        .map(|index| {
            let (mut c, _) = fixture("Ljava/lang/Throwable;", &[], false, false);
            c.descriptor = format!("Lsample/Chain{index};").into();
            c.superclass = Some(if index == 64 {
                Arc::from("Ljava/lang/Object;")
            } else {
                format!("Lsample/Chain{};", index + 1).into()
            });
            c
        })
        .collect();
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes(classes.iter()).unwrap();
    assert_eq!(
        h.call_checked_effects("Lsample/Chain0;", "work", &[], "V", true)
            .unwrap(),
        vec![Arc::<str>::from("Ljava/lang/Throwable;")]
    );
}

fn overloaded_contract_owner(owner: &str, name: &str) -> DexClass {
    let (mut c, _) = fixture("Ljava/lang/Throwable;", &[], false, false);
    c.descriptor = owner.into();
    c.methods = ["Ljava/lang/String;", "Ljava/lang/Object;"]
        .into_iter()
        .enumerate()
        .map(|(index, param)| DexMethod {
            declaring_type: c.descriptor.clone(),
            name: name.into(),
            return_type: "V".into(),
            parameters: vec![param.into()],
            thrown_types: if index == 0 {
                vec!["Ljava/io/IOException;".into()]
            } else {
                vec![]
            },
            access_flags: 9,
            code: None,
        })
        .collect();
    c
}
#[test]
fn distinct_same_arity_overloads_retain_exact_checked_and_empty_contracts() {
    let c = overloaded_contract_owner("Lsample/Api;", "work");
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&c]).unwrap();
    assert_eq!(
        h.call_checked_effects(
            &c.descriptor,
            "work",
            &["Ljava/lang/String;".into()],
            "V",
            true
        )
        .unwrap(),
        vec![Arc::<str>::from("Ljava/io/IOException;")]
    );
    assert!(h
        .call_checked_effects(
            &c.descriptor,
            "work",
            &["Ljava/lang/Object;".into()],
            "V",
            true
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        h.call_checked_effects(
            &c.descriptor,
            "work",
            &["Ljava/lang/String;".into()],
            "V",
            false
        )
        .unwrap(),
        vec![Arc::<str>::from("Ljava/lang/Throwable;")]
    );
    assert_eq!(
        h.call_checked_effects(
            &c.descriptor,
            "work",
            &["Ljava/lang/String;".into()],
            "I",
            true
        )
        .unwrap(),
        vec![Arc::<str>::from("Ljava/lang/Throwable;")]
    );
}
#[test]
fn conflicting_loaded_sdk_shadow_retains_unknown_contract_in_ambiguous_family() {
    let mut c = overloaded_contract_owner("Ljava/lang/Class;", "forName");
    for method in &mut c.methods {
        method.thrown_types.clear();
        method.return_type = "Ljava/lang/Class;".into();
    }
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&c]).unwrap();
    assert_eq!(
        h.call_checked_effects(
            &c.descriptor,
            "forName",
            &["Ljava/lang/String;".into()],
            "Ljava/lang/Class;",
            true
        )
        .unwrap(),
        vec![Arc::<str>::from("Ljava/lang/Throwable;")]
    );
}

fn recovered_checked_constructor(declared: bool) -> (DexClass, DexMethod) {
    let (mut c, mut m) = call_fixture(true, declared);
    let symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Child;".into(),
            "Lsample/Base;".into(),
            "Ljava/lang/Object;".into(),
        ],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec!["I".into()]), ("V".into(), vec![])],
        methods: vec![(1, 0, 0), (2, 1, 0)],
        ..Default::default()
    });
    let (mut child, _) = fixture("Ljava/lang/Throwable;", &[], false, false);
    child.descriptor = "Lsample/Child;".into();
    child.superclass = Some("Lsample/Base;".into());
    child.symbols = symbols.clone();
    let (mut base, _) = fixture("Ljava/lang/Throwable;", &[], false, false);
    base.descriptor = "Lsample/Base;".into();
    base.access_flags = 1;
    base.symbols = symbols.clone();
    base.methods = vec![DexMethod {
        declaring_type: base.descriptor.clone(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec!["I".into()],
        thrown_types: vec!["Ljava/io/IOException;".into()],
        access_flags: 1,
        code: Some(DexCode {
            registers: 2,
            ins: 2,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x1070, 1, 0, 0x000e],
            offset: 0,
        }),
    }];
    c.symbols = symbols.clone();
    let code = m.code.as_mut().unwrap();
    code.registers = 2;
    code.outs = 2;
    code.instructions = vec![0x0022, 0, 0x1112, 0x2070, 0, 0x0010, 0x000e, 0x000d, 0x0027];
    code.try_regions[0].end = 6;
    code.try_regions[0].catches = vec![(None, 7)].into();
    c.methods = vec![DexMethod {
        declaring_type: c.descriptor.clone(),
        name: m.name.clone(),
        return_type: m.return_type.clone(),
        parameters: vec![],
        thrown_types: m.thrown_types.clone(),
        access_flags: m.access_flags,
        code: Some(DexCode {
            registers: code.registers,
            ins: code.ins,
            outs: code.outs,
            tries: code.tries,
            try_regions: code.try_regions.clone(),
            instructions: code.instructions.clone(),
            offset: code.offset,
        }),
    }];
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&c, &child, &base]).unwrap();
    assert_eq!(
        h.recovered_constructors("Lsample/Child;")[0].thrown_types,
        vec![Arc::<str>::from("Ljava/io/IOException;")]
    );
    assert_eq!(
        h.call_checked_effects("Lsample/Child;", "<init>", &["I".into()], "V", false)
            .unwrap(),
        vec![Arc::<str>::from("Ljava/io/IOException;")]
    );
    symbols.hierarchy.set(Arc::new(h)).unwrap();
    (c, m)
}
#[test]
fn recovered_checked_constructor_rethrow_requires_exact_contract() {
    let (c, m) = recovered_checked_constructor(false);
    checked_contract(&c, "Lsample/Child;", "<init>", &["I"], "V", false);
    caught_bridge(&c, &m);
}
#[test]
fn recovered_checked_constructor_declared_contract_retains_identity() {
    let (c, m) = recovered_checked_constructor(true);
    let s = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(s.contains("new sample.Child(1)"), "{s}");
    assert!(s.contains("throw e"), "{s}");
}
