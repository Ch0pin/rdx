use rdx::{
    native_dex::{self, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(end: u32) -> rdx::native_dex::DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.descriptor = "Lsample/SnapshotEnd;".into();
    class.fields.clear();
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let m = &mut class.methods[0];
    m.declaring_type = class.descriptor.clone();
    m.name = "resolve".into();
    m.access_flags = 1;
    m.parameters = vec!["Ljava/lang/String;".into()];
    m.return_type = "Ljava/lang/Object;".into();
    let c = m.code.as_mut().unwrap();
    c.registers = 2;
    c.ins = 2;
    c.outs = 1;
    c.tries = 1;
    c.try_regions = vec![DexTryRegion {
        start: 0,
        end,
        catches: Arc::from([(Some("Ljava/lang/ClassNotFoundException;".into()), 8)]),
    }];
    c.instructions = vec![0x1071, 0, 1, 0x000c, 0x0071, 1, 0, 0x0011, 0x010d, 0x0011];
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/Class;".into(), class.descriptor.clone()],
        strings: vec!["forName".into(), "after".into()],
        protos: vec![
            (
                "Ljava/lang/Class;".into(),
                vec!["Ljava/lang/String;".into()],
            ),
            ("V".into(), vec![]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1)],
        ..Default::default()
    });
    class
}
#[test]
fn result_reuse_after_last_protected_call_preserves_entry_snapshot() {
    let c = fixture(4);
    let out = native_java::render_method("sample.SnapshotEnd", &c, &c.methods[0]).unwrap();
    assert!(
        out.source.contains("Class.forName") && out.source.contains("after()"),
        "{}",
        out.source
    );
}
#[test]
fn another_protected_call_retains_snapshot_type_check() {
    let c = fixture(7);
    assert!(native_java::render_method("sample.SnapshotEnd", &c, &c.methods[0]).is_err());
}
#[test]
#[ignore = "requires javac/java; serial validation"]
fn jvm_catch_observes_pre_call_receiver_and_later_failure_escapes() {
    let c = fixture(4);
    let out = native_java::render_method("sample.SnapshotEnd", &c, &c.methods[0]).unwrap();
    let root = std::env::temp_dir().join(format!("rdx-snapshot-end-{}", std::process::id()));
    fs::create_dir_all(root.join("sample")).unwrap();
    fs::write(root.join("sample/SnapshotEnd.java"),format!(r#"package sample; public class SnapshotEnd {{
{}
static int effects; static boolean fail; static final RuntimeException marker=new RuntimeException();
static void after() {{effects++; if(fail) throw marker;}}
public static void main(String[] args) {{ SnapshotEnd s=new SnapshotEnd();
if(s.resolve("java.lang.String")!=String.class||effects!=1) throw new AssertionError();
if(s.resolve("rdx.MissingClass")!=s||effects!=1) throw new AssertionError();
fail=true; try{{s.resolve("java.lang.Integer");throw new AssertionError();}}catch(RuntimeException e){{if(e!=marker||effects!=2)throw new AssertionError(e);}}
}}
}}"#,out.source)).unwrap();
    let c = Command::new("javac")
        .arg(root.join("sample/SnapshotEnd.java"))
        .output()
        .unwrap();
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    let r = Command::new("java")
        .args(["-cp", root.to_str().unwrap(), "sample.SnapshotEnd"])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
}
