use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

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

fn trivial(owner: &str, superclass: &str, invoked: &str) -> DexClass {
    let mut result = class(owner, superclass, 0x401);
    result.symbols = Arc::new(DexSymbols {
        types: vec![invoked.into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    result.methods.push(DexMethod {
        declaring_type: owner.into(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x10001,
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x1070, 0, 0, 0x000e],
        }),
    });
    result
}

fn leaf(owner: &str, parent: &str) -> DexClass {
    let mut result = class(owner, parent, 1);
    result.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/Object;".into(), owner.into()],
        strings: vec!["<init>".into(), "value".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        fields: vec![(1, 0, 1)],
        ..Default::default()
    });
    result.fields.push(DexField {
        declaring_type: owner.into(),
        name: "value".into(),
        field_type: "Ljava/lang/Object;".into(),
        access_flags: 1,
        is_static: false,
    });
    // A retained overload suppresses Java's implicit default constructor.
    result.methods.push(DexMethod {
        declaring_type: owner.into(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec!["Ljava/lang/Object;".into()],
        thrown_types: vec![],
        access_flags: 0x10001,
        code: Some(DexCode {
            registers: 2,
            ins: 2,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![0x1070, 0, 0, 0x015b, 0, 0x000e],
        }),
    });
    result
}

fn caller(observations: &[(&str, &str)]) -> DexClass {
    let mut result = class("Lsample/Caller;", "Ljava/lang/Object;", 1);
    let mut types: Vec<Arc<str>> = Vec::new();
    let mut methods = Vec::new();
    let mut words = Vec::new();
    for &(allocated, invoked) in observations {
        let allocated_index = u16::try_from(types.len()).expect("fixture type pool fits u16");
        types.push(allocated.into());
        let invoked_index = u16::try_from(types.len()).expect("fixture type pool fits u16");
        types.push(invoked.into());
        let method_index = u16::try_from(methods.len()).expect("fixture method pool fits u16");
        methods.push((invoked_index, 0, 0));
        words.extend([0x0022, allocated_index]);
        words.extend([0x1070, method_index, 0]);
    }
    words.push(0x000e);
    result.symbols = Arc::new(DexSymbols {
        types,
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods,
        ..Default::default()
    });
    result.methods.push(DexMethod {
        declaring_type: result.descriptor.clone(),
        name: "make".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: words,
        }),
    });
    result
}

fn rename_caller(class: &mut DexClass, owner: &str) {
    class.descriptor = owner.into();
    class.methods[0].declaring_type = class.descriptor.clone();
}

fn retained_then_erased_caller() -> DexClass {
    let mut result = class("Lsample/Caller;", "Ljava/lang/Object;", 1);
    result.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Leaf;".into(), "Ljava/lang/Object;".into()],
        strings: vec!["<init>".into()],
        protos: vec![
            ("V".into(), vec!["Ljava/lang/Object;".into()]),
            ("V".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 0)],
        ..Default::default()
    });
    result.methods.push(DexMethod {
        declaring_type: result.descriptor.clone(),
        name: "make".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 2,
            ins: 0,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![
                0x0022, 0, 0x0112, 0x2070, 0, 0x0010, 0x0022, 0, 0x1070, 1, 0, 0x000e,
            ],
        }),
    });
    result
}

fn fixture() -> (DexClass, DexClass, DexClass, DexClass) {
    let leaf = leaf("Lsample/Leaf;", "Lsample/Immediate;");
    let immediate = trivial(
        "Lsample/Immediate;",
        "Lsample/Implicit;",
        "Ljava/lang/Object;",
    );
    let implicit = class("Lsample/Implicit;", "Ljava/lang/Object;", 0x401);
    let caller = caller(&[("Lsample/Leaf;", "Ljava/lang/Object;")]);
    (leaf, immediate, implicit, caller)
}

fn accepted(classes: &[&DexClass], leaf: &str) -> bool {
    TypeHierarchy::from_classes(classes.iter().copied())
        .unwrap()
        .equivalent_constructor(leaf, "Ljava/lang/Object;", &[])
}

#[test]
fn exact_one_layer_certificate_is_class_order_independent() {
    for order in 0..4 {
        let (leaf, immediate, implicit, caller) = fixture();
        let classes = match order {
            0 => vec![&leaf, &immediate, &implicit, &caller],
            1 => vec![&caller, &implicit, &immediate, &leaf],
            2 => vec![&implicit, &leaf, &caller, &immediate],
            _ => vec![&immediate, &caller, &leaf, &implicit],
        };
        let hierarchy = TypeHierarchy::from_classes(classes).unwrap();
        assert!(hierarchy.equivalent_constructor("Lsample/Leaf;", "Ljava/lang/Object;", &[]));
        let recovered = hierarchy.recovered_constructors("Lsample/Leaf;");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].parent.as_ref(), "Lsample/Immediate;");
        assert_eq!(recovered[0].invoked_owner.as_ref(), "Ljava/lang/Object;");
        assert!(recovered[0].strict_allocation_order);
        assert!(recovered[0].observed_allocation);
    }
}

#[test]
fn retained_overload_completion_does_not_poison_exact_erased_noarg_observation() {
    let (leaf, immediate, implicit, _) = fixture();
    let allocation = retained_then_erased_caller();
    assert!(accepted(
        &[&leaf, &immediate, &implicit, &allocation],
        "Lsample/Leaf;"
    ));
}

#[test]
fn nonpublic_cross_owner_retained_completion_declines() {
    for visibility in [0, 2, 4] {
        for reverse_classes in [false, true] {
            let (mut leaf, immediate, implicit, _) = fixture();
            leaf.methods[0].access_flags = 0x10000 | visibility;
            let allocation = retained_then_erased_caller();
            let mut classes = vec![&leaf, &immediate, &implicit, &allocation];
            if reverse_classes {
                classes.reverse();
            }
            assert!(
                !accepted(&classes, "Lsample/Leaf;"),
                "visibility {visibility}"
            );
        }
    }
}

#[test]
fn fresh_receiver_reused_as_retained_argument_declines() {
    for reverse_classes in [false, true] {
        let (leaf, immediate, implicit, _) = fixture();
        let mut allocation = retained_then_erased_caller();
        // invoke-direct {v0, v0}, Leaf.<init>(Object): the fresh receiver also
        // escapes through the constructor argument before the later exact site.
        allocation.methods[0].code.as_mut().unwrap().instructions[5] = 0x0000;
        let mut classes = vec![&leaf, &immediate, &implicit, &allocation];
        if reverse_classes {
            classes.reverse();
        }
        assert!(!accepted(&classes, "Lsample/Leaf;"));
    }
}

#[test]
fn fresh_identity_in_wide_high_word_declines() {
    let (leaf, immediate, implicit, mut allocation) = fixture();
    allocation.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Leaf;".into(), "Ljava/lang/Object;".into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    let code = allocation.methods[0].code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 0;
    code.outs = 1;
    code.instructions = vec![0x0012, 0x0122, 0, 0x0204, 0x1070, 0, 1, 0x000e];
    assert!(!accepted(
        &[&leaf, &immediate, &implicit, &allocation],
        "Lsample/Leaf;"
    ));
}

#[test]
fn body_access_shadow_duplicate_and_observation_conflicts_decline() {
    for variant in 0..19 {
        let (mut leaf, mut immediate, mut implicit, mut allocation) = fixture();
        let mut shadow = class("Ljava/lang/Object;", "Ljava/lang/Object;", 1);
        match variant {
            0 => immediate.methods[0]
                .code
                .as_mut()
                .unwrap()
                .instructions
                .insert(0, 0),
            1 => immediate.methods[0].thrown_types = vec!["Ljava/lang/Exception;".into()],
            2 => immediate.methods[0].access_flags = 2,
            3 => implicit.methods.push(DexMethod {
                declaring_type: implicit.descriptor.clone(),
                name: "<init>".into(),
                return_type: "V".into(),
                parameters: vec!["I".into()],
                thrown_types: vec![],
                access_flags: 1,
                code: None,
            }),
            4 => {
                allocation = caller(&[
                    ("Lsample/Leaf;", "Ljava/lang/Object;"),
                    ("Lsample/Leaf;", "Lsample/Other;"),
                ])
            }
            5 => shadow.superclass = None,
            6 => {
                drop(
                    allocation.methods[0]
                        .code
                        .as_mut()
                        .unwrap()
                        .instructions
                        .splice(0..0, [0x0022, 0, 0x0012]),
                );
            }
            7 => immediate.methods[0].access_flags &= !0x10000,
            8 => immediate.access_flags |= 0x10,
            9 => {
                immediate.descriptor = "Lsample/Immediate$Inner;".into();
                immediate.methods[0].declaring_type = immediate.descriptor.clone();
                leaf.superclass = Some(immediate.descriptor.clone());
            }
            10 => leaf.methods[0].access_flags &= !0x10000,
            11 => {
                immediate.access_flags &= !1;
                immediate.descriptor = "Lother/Immediate;".into();
                immediate.methods[0].declaring_type = immediate.descriptor.clone();
                leaf.superclass = Some(immediate.descriptor.clone());
            }
            12 => immediate.fields.push(DexField {
                declaring_type: immediate.descriptor.clone(),
                name: "hidden".into(),
                field_type: "I".into(),
                access_flags: 0x12,
                is_static: false,
            }),
            13 => allocation.methods[0].code.as_mut().unwrap().outs = 0,
            14 => {
                let code = allocation.methods[0].code.as_mut().unwrap();
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 1,
                    end: 2,
                    catches: vec![(None, 5)].into(),
                });
            }
            15 => {
                let code = allocation.methods[0].code.as_mut().unwrap();
                code.registers = 2;
                code.outs = 2;
                code.instructions[2] = 0x2070;
                code.instructions[4] = 0x0010;
            }
            16 => {
                let symbols = Arc::get_mut(&mut allocation.symbols).unwrap();
                symbols.types.push("Lsample/Hooks;".into());
                symbols.strings.push("escape".into());
                symbols
                    .protos
                    .push(("V".into(), vec!["Ljava/lang/Object;".into()]));
                symbols.methods.push((2, 1, 1));
                drop(
                    allocation.methods[0]
                        .code
                        .as_mut()
                        .unwrap()
                        .instructions
                        .splice(2..2, [0x1071, 1, 0]),
                );
            }
            17 => allocation.methods[0].code.as_mut().unwrap().ins = 1,
            18 => immediate.methods[0].code.as_mut().unwrap().outs = 0,
            _ => unreachable!(),
        }
        let mut classes = vec![&leaf, &immediate, &implicit, &allocation];
        if variant == 5 {
            classes.push(&shadow);
        }
        assert!(!accepted(&classes, "Lsample/Leaf;"), "variant {variant}");
    }
    let (leaf, immediate, implicit, allocation) = fixture();
    assert!(!accepted(
        &[&leaf, &immediate, &implicit, &implicit, &allocation],
        "Lsample/Leaf;"
    ));
}

#[test]
fn malformed_active_method_poisoning_is_class_and_call_order_independent() {
    for malformed in 0..4 {
        for reverse_classes in [false, true] {
            let (leaf, immediate, implicit, mut bad) = fixture();
            let mut good = caller(&[("Lsample/Leaf;", "Ljava/lang/Object;")]);
            rename_caller(&mut good, "Lsample/Good;");
            rename_caller(&mut bad, "Lsample/Bad;");
            let code = bad.methods[0].code.as_mut().unwrap();
            match malformed {
                0 => code.outs = 0,
                1 => {
                    code.tries = 1;
                    code.try_regions.push(DexTryRegion {
                        start: 1,
                        end: 2,
                        catches: vec![(None, 5)].into(),
                    });
                }
                2 => {
                    code.registers = 2;
                    code.outs = 2;
                    code.instructions[2] = 0x2070;
                    code.instructions[4] = 0x0010;
                }
                3 => code.ins = 1,
                _ => unreachable!(),
            }
            let mut callers = vec![&good, &bad];
            if reverse_classes {
                callers.reverse();
            }
            let mut classes = vec![&leaf, &immediate, &implicit];
            classes.extend(callers);
            assert!(
                !accepted(&classes, "Lsample/Leaf;"),
                "malformed {malformed}"
            );
        }
    }
}

#[test]
fn duplicate_wrong_owner_conflict_is_order_independent() {
    for reverse_calls in [false, true] {
        for reverse_classes in [false, true] {
            let (leaf, immediate, implicit, _) = fixture();
            let observations = if reverse_calls {
                vec![
                    ("Lsample/Leaf;", "Lsample/Other;"),
                    ("Lsample/Leaf;", "Ljava/lang/Object;"),
                ]
            } else {
                vec![
                    ("Lsample/Leaf;", "Ljava/lang/Object;"),
                    ("Lsample/Leaf;", "Lsample/Other;"),
                ]
            };
            let allocation = caller(&observations);
            let other_a = class("Lsample/Other;", "Ljava/lang/Object;", 1);
            let other_b = class("Lsample/Other;", "Ljava/lang/Object;", 1);
            let mut classes = vec![
                &leaf,
                &immediate,
                &implicit,
                &allocation,
                &other_a,
                &other_b,
            ];
            if reverse_classes {
                classes.reverse();
            }
            assert!(!accepted(&classes, "Lsample/Leaf;"));
        }
    }
}

#[test]
fn a_pending_record_cannot_certify_the_next_pending_record() {
    let top = leaf("Lsample/Top;", "Lsample/TopImmediate;");
    let top_immediate = trivial(
        "Lsample/TopImmediate;",
        "Lsample/Pending;",
        "Ljava/lang/Object;",
    );
    let pending = leaf("Lsample/Pending;", "Lsample/PendingImmediate;");
    let pending_immediate = trivial(
        "Lsample/PendingImmediate;",
        "Lsample/Implicit;",
        "Ljava/lang/Object;",
    );
    let implicit = class("Lsample/Implicit;", "Ljava/lang/Object;", 0x401);
    let allocation = caller(&[
        ("Lsample/Pending;", "Ljava/lang/Object;"),
        ("Lsample/Top;", "Ljava/lang/Object;"),
    ]);
    let hierarchy = TypeHierarchy::from_classes([
        &top,
        &top_immediate,
        &pending,
        &pending_immediate,
        &implicit,
        &allocation,
    ])
    .unwrap();
    assert!(hierarchy.equivalent_constructor("Lsample/Pending;", "Ljava/lang/Object;", &[]));
    assert!(!hierarchy.equivalent_constructor("Lsample/Top;", "Ljava/lang/Object;", &[]));
}

#[test]
#[ignore = "runs javac/java against engine-generated sources"]
fn generated_sources_compile_and_preserve_default_and_retained_identity() {
    use std::{fs, process::Command, time::SystemTime};

    let (leaf, immediate, implicit, caller) = fixture();
    let hierarchy =
        Arc::new(TypeHierarchy::from_classes([&leaf, &immediate, &implicit, &caller]).unwrap());
    for class in [&leaf, &immediate, &implicit, &caller] {
        class.symbols.hierarchy.set(hierarchy.clone()).unwrap();
    }
    let suffix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("rdx-layered-noarg-{}-{suffix}", std::process::id()));
    let package = root.join("sample");
    fs::create_dir_all(&package).unwrap();
    for (name, class) in [
        ("Implicit", &implicit),
        ("Immediate", &immediate),
        ("Leaf", &leaf),
    ] {
        let rendered = native_java::render(&format!("sample.{name}"), class).unwrap();
        fs::write(package.join(format!("{name}.java")), rendered.source).unwrap();
    }
    fs::write(
        root.join("Oracle.java"),
        r#"
import sample.Leaf;
public final class Oracle {
  private static int checks;
  private static void check(boolean value) {
    checks++;
    if (!value) throw new AssertionError("check " + checks);
  }
  public static void main(String[] args) throws Exception {
    Leaf fresh = new Leaf();
    check(fresh.value == null);
    Object marker = new Object();
    Leaf retained = new Leaf(marker);
    check(retained.value == marker);
    check(Leaf.class.getConstructor() != null);
    check(Leaf.class.getConstructor(Object.class) != null);
    check(Leaf.class.getDeclaredConstructors().length == 2);
    System.out.println("PASS checks=" + checks);
  }
}
"#,
    )
    .unwrap();
    let classes = root.join("classes");
    fs::create_dir(&classes).unwrap();
    let compiled = Command::new("javac")
        .args(["-Xlint:all", "-Werror", "-d"])
        .arg(&classes)
        .arg(package.join("Implicit.java"))
        .arg(package.join("Immediate.java"))
        .arg(package.join("Leaf.java"))
        .arg(root.join("Oracle.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let output = Command::new("java")
        .args(["-cp"])
        .arg(&classes)
        .arg("Oracle")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "PASS checks=5\n");
    fs::remove_dir_all(root).unwrap();
}
