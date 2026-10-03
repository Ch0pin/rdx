//! Removed no-arg constructors whose implicit Java constructor calls the DEX owner.
use rdx::{
    engine::{DecompilerEngine, NativeEngine},
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn empty_class(descriptor: &str, parent: Option<&str>, flags: u32) -> DexClass {
    DexClass {
        descriptor: descriptor.into(),
        superclass: parent.map(Into::into),
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
fn parent(descriptor: &str, constructor_flags: u32) -> DexClass {
    let mut class = empty_class(descriptor, Some("Ljava/lang/Object;"), 1);
    class.methods.push(DexMethod {
        declaring_type: descriptor.into(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: constructor_flags | 0x10000,
        code: Some(DexCode {
            registers: 1,
            ins: 1,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x1070, 0, 0, 0x000e],
            offset: 0,
        }),
    });
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/Object;".into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    class
}
fn caller(child: &DexClass, parent: &DexClass) -> DexClass {
    let mut class = empty_class("Lsample/Caller;", Some("Ljava/lang/Object;"), 1);
    class.symbols = Arc::new(DexSymbols {
        types: vec![child.descriptor.clone(), parent.descriptor.clone()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(1, 0, 0)],
        ..Default::default()
    });
    class.methods.push(DexMethod {
        declaring_type: class.descriptor.clone(),
        name: "make".into(),
        return_type: child.descriptor.clone(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x0022, 0, 0x1070, 0, 0, 0x0011],
            offset: 0,
        }),
    });
    class
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&class, child, parent]).unwrap(),
        ))
        .unwrap();
    class
}

#[test]
fn absent_constructor_uses_implicit_direct_super_call_and_raw_navigation() {
    for access in [0, 1, 4] {
        let child = empty_class("Lsample/Child;", Some("Lsample/Base;"), 1);
        let parent = parent("Lsample/Base;", access);
        let class = caller(&child, &parent);
        let code = native_java::render_method("sample.Caller", &class, &class.methods[0]).unwrap();
        assert!(
            code.source.contains("new sample.Child()"),
            "{}",
            code.source
        );
        assert!(!code.source.contains("new sample.Base()"));
        assert!(
            code.links
                .iter()
                .any(|link| link.label == "sample.Base.<init>()V")
        );
    }
}

#[test]
fn implicit_constructor_proof_rejects_inaccessible_or_missing_super_constructor() {
    for access in [0, 2] {
        let child = empty_class("Lother/Child;", Some("Lsample/Base;"), 1);
        let parent = parent("Lsample/Base;", access);
        let class = caller(&child, &parent);
        assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
    }
    let child = empty_class("Lsample/Child;", Some("Lsample/Base;"), 1);
    let parent = empty_class("Lsample/Base;", Some("Ljava/lang/Object;"), 1);
    let class = caller(&child, &parent);
    // Both loaded classes have no fields, initializers, or constructors. Java's
    // implicit Child() -> Base() -> Object() chain is exactly the observed call.
    let source = native_java::render_method("sample.Caller", &class, &class.methods[0]).unwrap();
    assert!(
        source.source.contains("new sample.Child()"),
        "{}",
        source.source
    );
    assert!(
        native_java::render("sample.Base", &parent)
            .unwrap()
            .source
            .contains("class Base")
    );
    assert!(
        native_java::render("sample.Child", &child)
            .unwrap()
            .source
            .contains("extends Base")
    );
    let missing = empty_class("Lsample/Base;", Some("Lmissing/Ancestor;"), 1);
    let class = caller(&child, &missing);
    assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
}

#[test]
fn implicit_constructor_proof_rejects_nonconcrete_nested_and_existing_constructors() {
    let parent = parent("Lsample/Base;", 1);
    for (descriptor, flags) in [
        ("Lsample/Child;", 0x401),
        ("Lsample/Child;", 0x201),
        ("Lsample/Outer$Child;", 1),
        ("Lsample/Child;", 0x4001),
    ] {
        let child = empty_class(descriptor, Some("Lsample/Base;"), flags);
        assert!(
            !TypeHierarchy::from_classes([&child, &parent])
                .unwrap()
                .equivalent_noarg_constructor(descriptor, "Lsample/Base;")
        );
    }
    let mut child = empty_class("Lsample/Child;", Some("Lsample/Base;"), 1);
    child.methods.push(DexMethod {
        declaring_type: child.descriptor.clone(),
        name: "<init>".into(),
        return_type: "V".into(),
        parameters: vec!["I".into()],
        thrown_types: vec![],
        access_flags: 1,
        code: None,
    });
    assert!(
        !TypeHierarchy::from_classes([&child, &parent])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Lsample/Base;")
    );
}

#[test]
fn implicit_constructor_proof_rejects_duplicate_parent_and_declared_throws() {
    let child = empty_class("Lsample/Child;", Some("Lsample/Base;"), 1);
    let mut parent = parent("Lsample/Base;", 1);
    assert!(
        !TypeHierarchy::from_classes([&child, &parent, &parent])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Lsample/Base;")
    );
    parent.methods[0]
        .thrown_types
        .push("Ljava/lang/Exception;".into());
    assert!(
        !TypeHierarchy::from_classes([&child, &parent])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Lsample/Base;")
    );
    assert!(
        !TypeHierarchy::from_classes([&child, &parent])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Ljava/lang/Object;")
    );
}

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Play Store APK and run --ignored"]
fn play_store_registration_initializer_reconstructs_implicit_constructor() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let code = engine.decompile_with_metadata("anfw").unwrap();
    assert!(
        !code.source.contains(".method anfw.<clinit>"),
        "{}",
        code.source
    );
    assert_eq!(
        code.source.matches("new anfw()").count(),
        1,
        "{}",
        code.source
    );
    assert!(code.source.find("anaq.e()").unwrap() < code.source.find("new anfw()").unwrap());
    assert!(code.source.find("new anfw()").unwrap() < code.source.find(".b(").unwrap());
    assert!(code.source.contains("static {"), "{}", code.source);
    let link = code
        .links
        .iter()
        .find(|link| link.label == "anam.<init>()V")
        .expect("exact superclass constructor identity");
    assert_eq!(
        engine
            .resolve_usage_target("anfw", link.start, &code.source_hash)
            .unwrap()
            .id,
        "anam.<init>()V"
    );
}

#[test]
fn implicit_constructor_can_call_external_object_only_as_direct_parent() {
    let child = empty_class("Lsample/Child;", Some("Ljava/lang/Object;"), 1);
    assert!(
        TypeHierarchy::from_classes([&child])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Child;", "Ljava/lang/Object;")
    );
    let unrelated = empty_class("Lsample/Other;", Some("Lmissing/Base;"), 1);
    assert!(
        !TypeHierarchy::from_classes([&unrelated])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Other;", "Ljava/lang/Object;")
    );
    let abstract_child = empty_class("Lsample/Abstract;", Some("Ljava/lang/Object;"), 0x401);
    assert!(
        !TypeHierarchy::from_classes([&abstract_child])
            .unwrap()
            .equivalent_noarg_constructor("Lsample/Abstract;", "Ljava/lang/Object;")
    );
}

#[test]
fn observed_parameter_constructor_can_cross_proven_transparent_forwarder() {
    let mut base = parent("Lsample/Base;", 1);
    base.methods[0].parameters = vec!["I".into()];
    base.methods[0].code.as_mut().unwrap().registers = 2;
    base.methods[0].code.as_mut().unwrap().ins = 2;
    let mut middle = parent("Lsample/Middle;", 1);
    middle.superclass = Some(base.descriptor.clone());
    middle.methods[0].parameters = vec!["I".into()];
    middle.symbols = Arc::new(DexSymbols {
        types: vec![base.descriptor.clone()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let code = middle.methods[0].code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 2;
    code.outs = 2;
    code.instructions = vec![0x2070, 0, 0x0010, 0x000e];
    let leaf = empty_class("Lsample/Leaf;", Some("Lsample/Middle;"), 1);
    let mut caller = caller(&leaf, &base);
    Arc::get_mut(&mut caller.symbols).unwrap().protos[0].1 = vec!["I".into()];
    let code = caller.methods[0].code.as_mut().unwrap();
    code.registers = 2;
    code.outs = 2;
    code.instructions = vec![0x0022, 0, 0x7112, 0x2070, 0, 0x0010, 0x0011];
    let hierarchy = TypeHierarchy::from_classes([&leaf, &middle, &base, &caller]).unwrap();
    assert!(hierarchy.equivalent_constructor(&leaf.descriptor, &base.descriptor, &["I".into()]));
    let ctor = &hierarchy.recovered_constructors(&leaf.descriptor)[0];
    assert_eq!(ctor.parent, middle.descriptor);
    assert_eq!(ctor.invoked_owner, base.descriptor);
    // A constant replacing the forwarded argument changes behavior and must fail.
    middle.methods[0].code.as_mut().unwrap().instructions = vec![0x0112, 0x2070, 0, 0x0010, 0x000e];
    let hierarchy = TypeHierarchy::from_classes([&leaf, &middle, &base, &caller]).unwrap();
    assert!(!hierarchy.equivalent_constructor(&leaf.descriptor, &base.descriptor, &["I".into()]));
}

fn delegated_child(parent: &DexClass, invoked: &str) -> DexClass {
    let mut child = empty_class("Lsample/Delegated;", Some(&parent.descriptor), 1);
    child.symbols = Arc::new(DexSymbols {
        types: vec![invoked.into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    child.methods.push(DexMethod {
        declaring_type: child.descriptor.clone(),
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
            instructions: vec![0x1070, 0, 0, 0x000e],
            offset: 0,
        }),
    });
    child
}

#[test]
fn abstract_implicit_parent_is_a_super_target_but_not_an_allocation_target() {
    let middle = empty_class("Lsample/AbstractBase;", Some("Ljava/lang/Object;"), 0x401);
    let child = delegated_child(&middle, "Ljava/lang/Object;");
    let hierarchy = TypeHierarchy::from_classes([&child, &middle]).unwrap();
    assert!(hierarchy.equivalent_noarg_super_constructor(&child.descriptor, "Ljava/lang/Object;"));
    assert!(!hierarchy.equivalent_noarg_constructor(&middle.descriptor, "Ljava/lang/Object;"));
    child.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    let rendered =
        native_java::render_method("sample.Delegated", &child, &child.methods[0]).unwrap();
    assert!(rendered.source.contains("super();"), "{}", rendered.source);
    assert!(
        rendered
            .links
            .iter()
            .any(|link| link.label == "java.lang.Object.<init>()V")
    );
}

#[test]
fn ancestor_super_rejects_effects_inaccessibility_throws_and_duplicate_owners() {
    for shape in 0..5 {
        let mut middle = parent("Lsample/Base;", 1);
        if shape == 0 {
            middle.methods[0]
                .code
                .as_mut()
                .unwrap()
                .instructions
                .insert(0, 0x0012);
        }
        if shape == 1 {
            middle.methods[0].access_flags = 0x10002;
        }
        if shape == 2 {
            middle.methods[0]
                .thrown_types
                .push("Ljava/lang/Exception;".into());
        }
        if shape == 3 {
            middle.superclass = Some("Lmissing/Ancestor;".into());
        }
        let child = delegated_child(&middle, "Ljava/lang/Object;");
        let classes = if shape == 4 {
            vec![&child, &middle, &middle]
        } else {
            vec![&child, &middle]
        };
        let hierarchy = TypeHierarchy::from_classes(classes).unwrap();
        assert!(
            !hierarchy.equivalent_noarg_super_constructor(&child.descriptor, "Ljava/lang/Object;"),
            "shape {shape}"
        );
        child.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
        assert!(
            native_java::render_method("sample.Delegated", &child, &child.methods[0]).is_err(),
            "shape {shape}"
        );
    }
}

#[test]
fn ancestor_super_supports_exact_explicit_noarg_forwarder() {
    let middle = parent("Lsample/Base;", 4);
    let child = delegated_child(&middle, "Ljava/lang/Object;");
    let hierarchy = TypeHierarchy::from_classes([&child, &middle]).unwrap();
    assert!(hierarchy.equivalent_noarg_super_constructor(&child.descriptor, "Ljava/lang/Object;"));
    child.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    assert!(
        native_java::render_method("sample.Delegated", &child, &child.methods[0])
            .unwrap()
            .source
            .contains("super();")
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn ancestor_super_emission_executes_unchanged_with_abstract_parent() {
    use std::{fs, process::Command};
    let middle = empty_class("Lsample/AbstractBase;", Some("Ljava/lang/Object;"), 0x401);
    let child = delegated_child(&middle, "Ljava/lang/Object;");
    child
        .symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&child, &middle]).unwrap(),
        ))
        .unwrap();
    let rendered =
        native_java::render_method("sample.Delegated", &child, &child.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-super-forwarding-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Delegated.java"), format!("package sample; abstract class AbstractBase {{}} public class Delegated extends AbstractBase {{ {} public static void main(String[] args) {{ Object first = new Delegated(); Object second = new Delegated(); if (first == second || first.getClass() != Delegated.class) throw new AssertionError(); }} }}", rendered.source)).unwrap();
    for (program, argument) in [
        ("javac", "sample/Delegated.java"),
        ("java", "sample.Delegated"),
    ] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

fn platform_forwarder_fixture(effectful: bool) -> (DexClass, DexClass) {
    let leaf = empty_class(
        "Lsample/MessageException;",
        Some("Ljava/lang/IllegalArgumentException;"),
        1,
    );
    let mut c = empty_class("Lsample/Caller;", Some("Ljava/lang/Object;"), 1);
    c.symbols = Arc::new(DexSymbols {
        types: vec![
            leaf.descriptor.clone(),
            "Ljava/lang/IllegalArgumentException;".into(),
            "Lsample/Effects;".into(),
        ],
        strings: vec!["<init>".into(), "message".into(), "literal".into()],
        protos: vec![
            ("V".into(), vec!["Ljava/lang/String;".into()]),
            ("Ljava/lang/String;".into(), vec![]),
        ],
        methods: vec![(1, 0, 0), (2, 1, 1)],
        ..Default::default()
    });
    let words = if effectful {
        vec![0x0022, 0, 0x0071, 1, 0, 0x010c, 0x2070, 0, 0x0010, 0x0011]
    } else {
        vec![0x0022, 0, 0x011a, 2, 0x2070, 0, 0x0010, 0x0011]
    };
    c.methods.push(DexMethod {
        declaring_type: c.descriptor.clone(),
        name: "make".into(),
        return_type: leaf.descriptor.clone(),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 2,
            ins: 0,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    });
    let h = Arc::new(TypeHierarchy::from_classes([&c, &leaf]).unwrap());
    c.symbols.hierarchy.set(h.clone()).unwrap();
    leaf.symbols.hierarchy.set(h).unwrap();
    (c, leaf)
}

#[test]
fn platform_forwarder_recovers_observed_exact_overload_and_preserves_subclass_identity() {
    for effectful in [false, true] {
        let (c, leaf) = platform_forwarder_fixture(effectful);
        let h = c.symbols.hierarchy.get().unwrap();
        assert!(h.equivalent_constructor(
            &leaf.descriptor,
            "Ljava/lang/IllegalArgumentException;",
            &["Ljava/lang/String;".into()]
        ));
        let code = native_java::render_method("sample.Caller", &c, &c.methods[0]).unwrap();
        assert!(
            code.source.contains("new sample.MessageException("),
            "{}",
            code.source
        );
        assert!(
            !code
                .source
                .contains("new java.lang.IllegalArgumentException(")
        );
        assert!(
            code.links.iter().any(|link| link.label
                == "java.lang.IllegalArgumentException.<init>(Ljava/lang/String;)V")
        );
        let leaf_code = native_java::render("sample.MessageException", &leaf).unwrap();
        assert!(
            leaf_code
                .source
                .contains("public MessageException(String p0)"),
            "{}",
            leaf_code.source
        );
        assert!(leaf_code.source.contains("super(p0);"));
    }
}

#[test]
fn platform_forwarder_declines_unknown_signatures_loaded_shadows_and_final_fields() {
    for shape in 0..4 {
        let (mut c, mut leaf) = platform_forwarder_fixture(false);
        c.symbols = Arc::new(DexSymbols {
            types: c.symbols.types.clone(),
            strings: c.symbols.strings.clone(),
            protos: c.symbols.protos.clone(),
            methods: c.symbols.methods.clone(),
            ..Default::default()
        });
        leaf.symbols = Arc::new(DexSymbols::default());
        if shape == 0 {
            leaf.superclass = Some("Lmissing/Parent;".into());
            Arc::get_mut(&mut c.symbols).unwrap().types[1] = "Lmissing/Parent;".into();
        }
        if shape == 1 {
            Arc::get_mut(&mut c.symbols).unwrap().protos[0].1 = vec!["I".into()];
        }
        if shape == 2 {
            leaf.fields.push(rdx::native_dex::DexField {
                declaring_type: leaf.descriptor.clone(),
                name: "mustAssign".into(),
                field_type: "I".into(),
                access_flags: 0x11,
                is_static: false,
            });
        }
        let shadow = empty_class(
            "Ljava/lang/IllegalArgumentException;",
            Some("Ljava/lang/RuntimeException;"),
            1,
        );
        let h = if shape == 3 {
            TypeHierarchy::from_classes([&c, &leaf, &shadow])
        } else {
            TypeHierarchy::from_classes([&c, &leaf])
        }
        .unwrap();
        assert!(
            !h.equivalent_constructor(
                &leaf.descriptor,
                &c.symbols.types[1],
                &c.symbols.protos[0].1
            ),
            "shape {shape}"
        );
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn platform_forwarder_jvm_preserves_class_initialization_before_argument_and_exception_identity() {
    use std::{fs, process::Command};
    let (c, leaf) = platform_forwarder_fixture(true);
    let code = native_java::render_method("sample.Caller", &c, &c.methods[0]).unwrap();
    let leaf_code = native_java::render("sample.MessageException", &leaf).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-platform-forwarding-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    // A dependency initialization probe observes the language allocation point;
    // both emitted method and recovered constructor remain unchanged.
    let leaf_source = leaf_code
        .source
        .replacen("{", "{ static { Effects.trace += \"A\"; }", 1);
    fs::write(dir.join("sample/MessageException.java"), leaf_source).unwrap();
    fs::write(dir.join("sample/Caller.java"), format!(r#"package sample;
class Effects {{ static String trace=""; static boolean fail; static final RuntimeException marker=new RuntimeException(); static String message() {{ trace+="B"; if (fail) throw marker; return "message"; }} }}
public class Caller {{ {} public static void main(String[] args) {{ MessageException first=make(); if (first.getClass()!=MessageException.class || !first.getMessage().equals("message") || !Effects.trace.equals("AB")) throw new AssertionError(Effects.trace); Effects.trace=""; Effects.fail=true; try {{ make(); throw new AssertionError(); }} catch (RuntimeException actual) {{ if (actual!=Effects.marker || !Effects.trace.equals("B")) throw new AssertionError(Effects.trace); }} }} }}"#, code.source)).unwrap();
    for (program, arguments) in [
        (
            "javac",
            vec!["sample/Caller.java", "sample/MessageException.java"],
        ),
        ("java", vec!["sample.Caller"]),
    ] {
        let result = Command::new(program)
            .args(arguments)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

fn discarded_void_forwarder_fixture() -> (DexClass, DexClass) {
    let (mut c, _) = platform_forwarder_fixture(true);
    let mut symbols = DexSymbols {
        types: c.symbols.types.clone(),
        strings: c.symbols.strings.clone(),
        protos: c.symbols.protos.clone(),
        methods: c.symbols.methods.clone(),
        ..Default::default()
    };
    symbols.strings.push("touch".into());
    symbols.protos.push(("V".into(), vec![]));
    symbols.methods.push((2, 2, 3));
    let leaf = empty_class(
        "Lsample/MessageException;",
        Some("Ljava/lang/IllegalArgumentException;"),
        1,
    );
    c.symbols = Arc::new(symbols);
    c.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0022, 0, 0x0071, 2, 0, 0x0071, 1, 0, 0x010c, 0x2070, 0, 0x0010, 0x0011,
    ];
    c.symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&c, &leaf]).unwrap()))
        .unwrap();
    leaf.symbols
        .hierarchy
        .set(c.symbols.hierarchy.get().unwrap().clone())
        .unwrap();
    (c, leaf)
}

#[test]
fn recovered_platform_forwarder_keeps_discarded_void_effect_after_allocation() {
    let (mut c, _) = discarded_void_forwarder_fixture();
    let rendered = native_java::render_method("sample.Caller", &c, &c.methods[0]).unwrap();
    let allocation = rendered.source.find("new sample.MessageException").unwrap();
    let effect = rendered.source.find("sample.Effects.touch()").unwrap();
    let argument = rendered.source.find("sample.Effects.message()").unwrap();
    assert!(
        allocation < effect && effect < argument,
        "{}",
        rendered.source
    );
    assert_eq!(rendered.source.matches("sample.Effects.touch()").count(), 1);
    assert!(
        rendered
            .links
            .iter()
            .any(|l| l.label == "sample.Effects.touch()V")
    );
    // A void effect cannot expose the not-yet-initialized allocation receiver.
    Arc::get_mut(&mut c.symbols).unwrap().protos[2].1 = vec!["Lsample/MessageException;".into()];
    c.methods[0].code.as_mut().unwrap().instructions[2] = 0x1071;
    assert!(native_java::render_method("sample.Caller", &c, &c.methods[0]).is_err());
}
#[test]
#[ignore = "requires explicit JDK25"]
fn discarded_forwarder_void_effect_keeps_initialization_and_fault_identity_in_java() {
    use std::{fs, process::Command};
    let (c, leaf) = discarded_void_forwarder_fixture();
    let method = native_java::render_method("sample.Caller", &c, &c.methods[0]).unwrap();
    let leaf = native_java::render("sample.MessageException", &leaf).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-forwarder-void-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    // The same dependency initializer probe as the existing forwarder JVM test;
    // emitted caller and recovered constructor statements remain unchanged.
    let leaf = leaf.source.replacen(
        "{",
        "{ static { Effects.trace += \"A\"; if ((Effects.mask&4)!=0) throw Effects.init; }",
        1,
    );
    fs::write(dir.join("sample/MessageException.java"), leaf).unwrap();
    fs::write(dir.join("sample/Caller.java"),format!(r#"package sample;
class Effects {{static String trace="";static int mask;static final AssertionError init=new AssertionError();static final RuntimeException touch=new RuntimeException(),message=new RuntimeException();static void touch(){{trace+="T";if((mask&1)!=0)throw touch;}}static String message(){{trace+="M";if((mask&2)!=0)throw message;return "value";}}}}
public class Caller {{{} public static void main(String[] args){{Effects.mask=Integer.parseInt(args[0]);try{{MessageException value=make();if(!value.getMessage().equals("value")||value.getClass()!=MessageException.class)throw new AssertionError();System.out.print(Effects.trace+":value");}}catch(Throwable fault){{String identity=fault==Effects.init?"init":fault==Effects.touch?"touch":fault==Effects.message?"message":null;if(identity==null)throw new AssertionError("fault identity",fault);System.out.print(Effects.trace+":"+identity);}}}}}}
"#,method.source)).unwrap();
    let home = std::env::var("RDX_JAVA25_HOME").expect("explicit JDK25 home");
    let result = Command::new(format!("{home}/bin/javac"))
        .arg(dir.join("sample/Caller.java"))
        .arg(dir.join("sample/MessageException.java"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for mask in 0..8 {
        // Original raw new-instance and invoke pool identities fix the event order.
        let words = &c.methods[0].code.as_ref().unwrap().instructions;
        assert_eq!(&words[..2], &[0x0022, 0]);
        assert_eq!(&words[2..5], &[0x0071, 2, 0]);
        assert_eq!(&words[5..8], &[0x0071, 1, 0]);
        let expected = if mask & 4 != 0 {
            "A:init"
        } else if mask & 1 != 0 {
            "AT:touch"
        } else if mask & 2 != 0 {
            "ATM:message"
        } else {
            "ATM:value"
        };
        let result = Command::new(format!("{home}/bin/java"))
            .arg("-Xverify:all")
            .arg("-cp")
            .arg(&dir)
            .arg("sample.Caller")
            .arg(mask.to_string())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&result.stdout), expected);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn observed_missing_noarg_overload_does_not_suppress_existing_parameter_constructor() {
    let base = parent("Lsample/Base;", 1);
    let mut leaf = parent("Lsample/Leaf;", 1);
    leaf.superclass = Some(base.descriptor.clone());
    Arc::get_mut(&mut leaf.symbols).unwrap().types[0] = base.descriptor.clone();
    leaf.methods[0].parameters = vec!["I".into()];
    leaf.methods[0].code.as_mut().unwrap().registers = 2;
    leaf.methods[0].code.as_mut().unwrap().ins = 2;
    let class = caller(&leaf, &base);
    let h = class.symbols.hierarchy.get().unwrap();
    assert!(h.equivalent_constructor(&leaf.descriptor, &base.descriptor, &[]));
    assert_eq!(h.recovered_constructors(&leaf.descriptor).len(), 1);
    assert!(
        h.recovered_constructors(&leaf.descriptor)[0]
            .parameters
            .is_empty()
    );
    leaf.symbols.hierarchy.set(h.clone()).unwrap();
    let source = native_java::render("sample.Leaf", &leaf).unwrap().source;
    assert!(source.contains("public Leaf()"), "{source}");
    assert!(source.contains("public Leaf(int "), "{source}");
}
