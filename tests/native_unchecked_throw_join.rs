use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let descriptor: Arc<str> = "Lsample/ThrowJoin;".into();
    let method = DexMethod {
        declaring_type: descriptor.clone(),
        name: "run".into(),
        return_type: "V".into(),
        parameters: vec!["I".into()],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 3,
            ins: 1,
            outs: 2,
            tries: 0,
            try_regions: vec![],
            offset: 0,
            instructions: vec![
                0x0238, 10, // if-eqz p0, second allocation
                0x0022, 0, // new IllegalArgumentException
                0x011a, 1, // "argument"
                0x2070, 0, 0x0010, // constructor
                0x0828, // goto shared throw
                0x0022, 1, // new IllegalStateException
                0x011a, 2, // "state"
                0x2070, 1, 0x0010, // constructor
                0x0027, // throw v0
            ],
        }),
    };
    let mut class = DexClass {
        descriptor,
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![method],
        symbols: Arc::new(DexSymbols {
            strings: vec!["<init>".into(), "argument".into(), "state".into()],
            types: vec![
                "Ljava/lang/IllegalArgumentException;".into(),
                "Ljava/lang/IllegalStateException;".into(),
            ],
            protos: vec![("V".into(), vec!["Ljava/lang/String;".into()])],
            methods: vec![(0, 0, 0), (1, 0, 0)],
            ..Default::default()
        }),
    };
    let hierarchy = TypeHierarchy::from_classes([&class]).unwrap();
    Arc::get_mut(&mut class.symbols)
        .unwrap()
        .hierarchy
        .set(Arc::new(hierarchy))
        .unwrap();
    class
}

#[test]
fn proven_unchecked_siblings_join_at_shared_throw() {
    let class = fixture();
    let java = native_java::render("sample.ThrowJoin", &class).unwrap();
    assert!(
        java.source.contains("RuntimeException runtimeException"),
        "{}",
        java.source
    );
    assert!(java.source.contains("throw "), "{}", java.source);
    assert!(
        !java.source.contains("throws java.lang.Exception"),
        "{}",
        java.source
    );
}

#[test]
#[ignore = "requires javac and java"]
fn jvm_preserves_both_exception_classes_and_messages() {
    use std::{fs, process::Command};
    let class = fixture();
    let java = native_java::render("sample.ThrowJoin", &class).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-unchecked-join-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/ThrowJoin.java"), java.source).unwrap();
    fs::write(
        dir.join("sample/Check.java"),
        "package sample; public class Check { public static void main(String[] args) { try { ThrowJoin.run(1); throw new AssertionError(); } catch (IllegalArgumentException e) { if (!\"argument\".equals(e.getMessage())) throw new AssertionError(e); } try { ThrowJoin.run(0); throw new AssertionError(); } catch (IllegalStateException e) { if (!\"state\".equals(e.getMessage())) throw new AssertionError(e); } } }",
    ).unwrap();
    let compile = Command::new("javac")
        .current_dir(&dir)
        .args(["sample/ThrowJoin.java", "sample/Check.java"])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .current_dir(&dir)
        .args(["-cp", ".", "sample.Check"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}
