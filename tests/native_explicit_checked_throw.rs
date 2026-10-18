use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;
fn fixture(words: Vec<u16>) -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Checked;".into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let make = |owner: &str, parent: &str, methods| DexClass {
        descriptor: owner.into(),
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
    let root = make(
        "Lsample/Caller;",
        "Ljava/lang/Object;",
        vec![DexMethod {
            declaring_type: "Lsample/Caller;".into(),
            name: "run".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 1,
            code: Some(DexCode {
                registers: 2,
                ins: 1,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    );
    let checked = make("Lsample/Checked;", "Ljava/lang/Exception;", vec![]);
    symbols
        .hierarchy
        .set(Arc::new(
            TypeHierarchy::from_classes([&root, &checked]).unwrap(),
        ))
        .unwrap();
    root
}
#[test]
fn fresh_checked_throw_keeps_original_header() {
    let class = fixture(vec![0x0022, 0, 0x1070, 0, 0, 0x0027]);
    let source = native_java::render("sample.Caller", &class).unwrap().source;
    assert!(source.contains("throw _rdx_checked_throw_"), "{source}");
    assert!(
        !source
            .lines()
            .find(|l| l.contains(" run("))
            .unwrap()
            .contains("throws"),
        "{source}"
    );
    assert!(source.contains("throw (E) failure"), "{source}");
}
#[test]
fn primitive_and_undefined_throws_remain_rejected_and_null_keeps_npe() {
    for words in [vec![0x1012, 0x0027], vec![0x0027]] {
        let class = fixture(words);
        assert!(native_java::render_method("sample.Caller", &class, &class.methods[0]).is_err());
    }
    let class = fixture(vec![0x0012, 0x0027]);
    let source = native_java::render("sample.Caller", &class).unwrap().source;
    assert!(source.contains("throw null;"), "{source}");
    assert!(!source.contains("_rdx_checked_throw_"), "{source}");
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_explicit_checked_bridge_preserves_constructor_order_and_identity() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-explicit-checked-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    let class = fixture(vec![0x0022, 0, 0x1070, 0, 0, 0x0027]);
    fs::write(
        base.join("sample/Caller.java"),
        native_java::render("sample.Caller", &class).unwrap().source,
    )
    .unwrap();
    fs::write(base.join("sample/Checked.java"),"package sample; public class Checked extends Exception { private static final long serialVersionUID=1L; static Checked last; static String trace=\"\"; public Checked(){trace+=\"C\";last=this;} }").unwrap();
    fs::write(base.join("sample/Check.java"),"package sample; public class Check { public static void main(String[] args) { for(int i=0;i<3;i++){Checked.trace=\"B\";try{new Caller().run();throw new AssertionError();}catch(Throwable e){if(e!=Checked.last||!Checked.trace.equals(\"BC\"))throw new AssertionError(e);}Checked.trace+=\"A\";if(!Checked.trace.equals(\"BCA\"))throw new AssertionError();}System.out.print(\"ok\");} }").unwrap();
    let output = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Caller.java",
            "sample/Checked.java",
            "sample/Check.java",
        ])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ok");
}
