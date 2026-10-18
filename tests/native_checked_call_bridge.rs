use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn fixture(caught: bool) -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/Class;".into()],
        strings: vec!["forName".into()],
        protos: vec![(
            "Ljava/lang/Class;".into(),
            vec!["Ljava/lang/String;".into()],
        )],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = |owner: &str, words| DexMethod {
        declaring_type: owner.into(),
        name: "lookup".into(),
        return_type: "Ljava/lang/Class;".into(),
        parameters: vec!["Ljava/lang/String;".into()],
        thrown_types: vec![],
        access_flags: 1,
        code: Some(DexCode {
            registers: 2,
            ins: 2,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: words,
            offset: 0,
        }),
    };
    let class = |descriptor: &str, parent: &str, methods| DexClass {
        descriptor: descriptor.into(),
        superclass: Some(parent.into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
        symbols: symbols.clone(),
    };
    let parent = class(
        "Lsample/Parent;",
        "Ljava/lang/Object;",
        vec![method("Lsample/Parent;", vec![0x0012, 0x0011])],
    );
    let mut root = class(
        "Lsample/Lookup;",
        "Lsample/Parent;",
        vec![method(
            "Lsample/Lookup;",
            vec![0x1071, 0, 1, 0x000c, 0x0011],
        )],
    );
    if caught {
        let code = root.methods[0].code.as_mut().unwrap();
        code.tries = 1;
        code.try_regions = vec![rdx::native_dex::DexTryRegion {
            start: 0,
            end: 4,
            catches: vec![(None, 5)].into(),
        }];
        code.instructions.extend([0x000d, 0x0012, 0x0011]);
    }
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&root, &parent]).unwrap(),
        ))
        .unwrap();
    root
}
#[test]
fn checked_platform_call_uses_exact_private_forwarder_without_override_throws() {
    let class = fixture(false);
    let source = native_java::render("sample.Lookup", &class).unwrap().source;
    assert!(
        source.contains("private static <RdxCallThrowable"),
        "{source}"
    );
    assert!(source.contains("Class.forName(p0)"), "{source}");
    let declaration = source
        .lines()
        .find(|line| line.contains(" lookup("))
        .unwrap();
    assert!(!declaration.contains(" throws "), "{declaration}");
    assert!(source.contains("_rdx_checked_"), "{source}");
}
#[test]
fn locally_caught_checked_call_keeps_original_call_and_handler() {
    let class = fixture(true);
    let source = native_java::render("sample.Lookup", &class).unwrap().source;
    assert!(!source.contains("_rdx_checked_"), "{source}");
    assert!(source.contains("catch ("), "{source}");
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_checked_forwarder_preserves_class_initialization_failure_identity_and_order() {
    use std::{fs, process::Command};
    let class = fixture(false);
    let base = std::env::temp_dir().join(format!("rdx-checked-call-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    fs::write(
        base.join("sample/Lookup.java"),
        native_java::render("sample.Lookup", &class).unwrap().source,
    )
    .unwrap();
    fs::write(base.join("sample/Parent.java"),"package sample; public class Parent { public Class<?> lookup(String name) { return null; } }").unwrap();
    fs::write(base.join("sample/Check.java"),r#"package sample;
public class Check {
static final Error MARKER=new Error("marker"); static String events="";
public static class Active {static{events+="A";}}
public static class Broken {static{events+="B";if(true)throw MARKER;}}
public static void main(String[] args) throws Throwable {
Lookup lookup=new Lookup();
events="start";if(lookup.lookup("sample.Check$Active")!=Active.class || !events.equals("startA"))throw new AssertionError(events);
try{lookup.lookup("sample.Check$Broken");throw new AssertionError();}catch(Error error){if(error!=MARKER || !events.equals("startAB"))throw new AssertionError(error);}
try{lookup.lookup("missing.rdx.Type");throw new AssertionError();}catch(Throwable error){if(!(error instanceof ClassNotFoundException))throw new AssertionError(error);}
try{lookup.lookup(null);throw new AssertionError();}catch(NullPointerException expected){}
System.out.print("ok");}}
"#).unwrap();
    let compiled = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Xlint:-rawtypes",
            "-Werror",
            "sample/Parent.java",
            "sample/Lookup.java",
            "sample/Check.java",
        ])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert_eq!(executed.stdout, b"ok");
    fs::remove_dir_all(base).unwrap();
}
