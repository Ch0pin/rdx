fn nested_fixture() -> (DexClass, DexMethod) {
    let (c, mut m) = fixture(false, false);
    let code = m.code.as_mut().unwrap();
    code.instructions = vec![
        0x0071, 0, 0, 0x000e, 0x000d, 0x0071, 0, 0, 0x000e, 0x010d, 0x033d, 5, 0x03d8, 0xff03,
        0xfc28, 0x0027,
    ];
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 5,
        end: 8,
        catches: vec![(None, 9)].into(),
    });
    (c, m)
}
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(alias: bool, overwrite: bool) -> (DexClass, DexMethod) {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Helper;".into()],
        strings: vec!["fail".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let mut words = vec![0x0071, 0, 0, 0x000e, 0x000d];
    if alias {
        words.push(0x0107);
    }
    let start = words.len();
    words.extend([0x033d, 5, 0x03d8, 0xff03, 0xfc28]);
    if overwrite {
        words.push(0x2007);
    }
    words.push(if alias { 0x0127 } else { 0x0027 });
    // v3 is count, v2 is an arbitrary Throwable available for overwrite.
    let method = DexMethod {
        declaring_type: "Lsample/Example;".into(),
        name: "run".into(),
        return_type: "V".into(),
        parameters: vec!["Ljava/lang/Throwable;".into(), "I".into()],
        thrown_types: vec![],
        access_flags: 9,
        code: Some(DexCode {
            registers: 4,
            ins: 2,
            outs: 0,
            tries: 1,
            try_regions: vec![DexTryRegion {
                start: 0,
                end: 3,
                catches: vec![(None, 4)].into(),
            }],
            instructions: words,
            offset: 0,
        }),
    };
    assert!(start >= 5);
    (
        DexClass {
            symbols,
            descriptor: "Lsample/Example;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 0x11,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
        },
        method,
    )
}
fn rendered_sources() -> Vec<String> {
    let mut sources = Vec::new();
    for alias in [false, true] {
        let (c, m) = fixture(alias, false);
        let s = native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source;
        assert!(!s.contains("throws"), "{s}");
        sources.push(s.replacen("run(", if alias { "alias(" } else { "run(" }, 1));
    }
    let (c, m) = nested_fixture();
    sources.push(
        native_java::render_method("sample.Example", &c, &m)
            .unwrap()
            .source
            .replacen("run(", "nested(", 1),
    );
    let (c, mut m) = fixture(false, false);
    m.thrown_types = vec!["Ljava/io/IOException;".into()];
    let mut c = c;
    Arc::get_mut(&mut c.symbols).unwrap().strings[0] = "checked".into();
    let checked = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source
        .replacen("run(", "checked(", 1);
    assert!(checked.contains("throws java.io.IOException"), "{checked}");
    sources.push(checked);
    sources
}
#[test]
fn caught_loop_sources_preserve_precise_rethrow_contracts() {
    let sources = rendered_sources();
    for source in sources {
        assert!(source.contains("throw e"), "{source}");
        assert!(!source.contains("java.lang.Throwable v"), "{source}");
    }
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn caught_exception_identity_on_jvm_with_zero_multiple_iterations_aliases_nested_and_checked_contract()
 {
    let sources = rendered_sources();
    let dir = std::env::temp_dir().join(format!(
        "rdx-catch-loop-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    struct RemoveDir(std::path::PathBuf);
    impl Drop for RemoveDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = RemoveDir(dir.clone());
    let source = format!(
        "package sample; class Helper {{static java.io.IOException io;static void checked() throws java.io.IOException {{throw io;}}static RuntimeException original,secondary;static int calls;static void fail(){{throw calls++==0?original:secondary;}}}} class Example {{{}}} public class Runner {{public static void main(String[] args){{for(int n:new int[]{{0,1,5}}){{for(int alias=0;alias<3;alias++){{RuntimeException e=new RuntimeException();Helper.original=e;Helper.secondary=new RuntimeException();Helper.calls=0;try{{if(alias==0)Example.run(null,n);else if(alias==1)Example.alias(null,n);else Example.nested(null,n);throw new AssertionError();}}catch(RuntimeException caught){{if(caught!=e)throw new AssertionError(\"identity\");}}}}java.io.IOException io=new java.io.IOException();Helper.io=io;try{{Example.checked(null,n);throw new AssertionError();}}catch(java.io.IOException caught){{if(caught!=io)throw new AssertionError(\"checked identity\");}}}}}}}}",
        sources.join("\n")
    );
    std::fs::write(dir.join("Runner.java"), source).unwrap();
    let o = std::process::Command::new("javac")
        .args([
            "-d",
            dir.to_str().unwrap(),
            dir.join("Runner.java").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = std::process::Command::new("java")
        .args(["-cp", dir.to_str().unwrap(), "sample.Runner"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
#[test]
fn overwritten_catch_identity_is_rejected() {
    let (c, m) = fixture(false, true);
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn conditional_loop_overwrite_does_not_reuse_original_catch() {
    let (c, mut m) = fixture(false, false);
    m.code.as_mut().unwrap().instructions = vec![
        0x0071, 0, 0, 0x000e, 0x000d, 0x033d, 8, 0x0338, 3, 0x2007, 0x03d8, 0xff03, 0xf928, 0x0027,
    ];
    assert!(native_java::render_method("sample.Example", &c, &m).is_err());
}
#[test]
fn nested_catch_retains_distinct_outer_identity() {
    let (c, m) = nested_fixture();
    let source = native_java::render_method("sample.Example", &c, &m)
        .unwrap()
        .source;
    assert!(source.matches("catch").count() == 2, "{source}");
    assert!(source.contains("throw e"), "{source}");
    assert!(!source.contains("throws"), "{source}");
}
