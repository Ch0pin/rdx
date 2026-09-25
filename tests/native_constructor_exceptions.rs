use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(start: u32) -> DexClass {
    DexClass {
        descriptor: "Lsample/Child;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["<init>".into(), "touch".into()],
            types: vec!["Ljava/lang/Object;".into(), "Lsample/Effects;".into()],
            protos: vec![("V".into(), vec![])],
            methods: vec![(0, 0, 0), (1, 0, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Child;".into(),
            name: "<init>".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 0x10001,
            code: Some(DexCode {
                registers: 2,
                ins: 1,
                outs: 1,
                tries: 1,
                offset: 0,
                instructions: vec![0x1070, 0, 1, 0x0071, 1, 0, 0x000e, 0x000d, 0x000e],
                try_regions: vec![DexTryRegion {
                    start,
                    end: 6,
                    catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 7)].into(),
                }],
            }),
        }],
    }
}
#[test]
fn initialized_constructor_can_catch_body_exception() {
    let class = fixture(3);
    let result = native_java::render_method("sample.Child", &class, &class.methods[0]).unwrap();
    assert!(
        result.source.find("super();").unwrap() < result.source.find("try {").unwrap(),
        "{}",
        result.source
    );
    assert!(
        result.source.contains("sample.Effects.touch()"),
        "{}",
        result.source
    );
    assert!(
        result.source.contains("catch (java.lang.RuntimeException"),
        "{}",
        result.source
    );
}
#[test]
fn constructor_handler_covering_super_is_rejected() {
    let class = fixture(0);
    assert!(native_java::render_method("sample.Child", &class, &class.methods[0]).is_err());
}

fn throwing_stub() -> DexClass {
    let mut class = fixture(3);
    let code = class.methods[0].code.as_mut().unwrap();
    code.try_regions.clear();
    code.tries = 0;
    code.instructions = vec![0x0012, 0x0027];
    class
}
#[test]
fn always_throwing_constructor_uses_explicit_unreachable_delegation() {
    let c = throwing_stub();
    let out = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
    assert!(out.source.contains("if (true)"), "{}", out.source);
    assert!(out.source.find("throw null;").unwrap() < out.source.find("super();").unwrap());
    let mut c = throwing_stub();
    c.superclass = Some("Lunknown/Base;".into());
    assert!(native_java::render_method("sample.Child", &c, &c.methods[0]).is_err());
}
#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn java25_throwing_constructor_does_not_initialize_super() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let c = throwing_stub();
    let method = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-throw25-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = format!(
        "public class Child {{final int x; {} public static void main(String[] a){{try{{new Child();throw new AssertionError();}}catch(NullPointerException expected){{}}}}}}",
        method.source
    );
    fs::write(dir.join("Child.java"), source).unwrap();
    let result = Command::new(format!("{home}/bin/javac"))
        .arg("Child.java")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(format!("{home}/bin/java"))
        .args(["-cp", ".", "Child"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = Command::new(format!("{home}/bin/javap"))
        .args(["-c", "-p", "Child.class"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let disasm = String::from_utf8(result.stdout).unwrap();
    let constructor = disasm
        .split("public Child();")
        .nth(1)
        .unwrap()
        .split("public static")
        .next()
        .unwrap();
    assert!(constructor.contains("athrow"), "{disasm}");
    assert!(!constructor.contains("invokespecial"), "{disasm}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn always_throwing_constructor_retains_prologue_effect_before_throw() {
    let mut c = throwing_stub();
    c.methods[0].code.as_mut().unwrap().instructions = vec![0x0071, 1, 0, 0x0012, 0x0027];
    let out = native_java::render_method("sample.Child", &c, &c.methods[0]).unwrap();
    assert_eq!(
        out.source.matches("sample.Effects.touch()").count(),
        1,
        "{}",
        out.source
    );
    assert!(
        out.source.find("sample.Effects.touch()").unwrap()
            < out.source.find("throw null;").unwrap()
    );
}

#[test]
fn throwing_constructor_requires_exact_accessible_loaded_parent() {
    let mut child = throwing_stub();
    child.superclass = Some("Lsample/Parent;".into());
    let mut parent = fixture(3);
    parent.descriptor = "Lsample/Parent;".into();
    parent.methods[0].declaring_type = parent.descriptor.clone();
    let parent_code = parent.methods[0].code.as_mut().unwrap();
    parent_code.tries = 0;
    parent_code.try_regions.clear();
    parent_code.instructions = vec![0x1070, 0, 1, 0x000e];
    let h =
        Arc::new(rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &parent]).unwrap());
    assert!(h.has_accessible_noarg_super(&child.descriptor));
    child.symbols.hierarchy.set(h).unwrap();
    assert!(
        native_java::render_method("sample.Child", &child, &child.methods[0])
            .unwrap()
            .source
            .contains("if (true)")
    );
    parent.methods[0].access_flags = 2;
    let h = rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &parent]).unwrap();
    assert!(!h.has_accessible_noarg_super(&child.descriptor));
    parent.methods[0].access_flags = 1;
    parent.methods[0].thrown_types = vec!["Ljava/lang/Exception;".into()];
    assert!(
        !rdx::native_hierarchy::TypeHierarchy::from_classes([&child, &parent])
            .unwrap()
            .has_accessible_noarg_super(&child.descriptor)
    );
}
