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

fn prefix_snapshot_fixture() -> rdx::native_dex::DexClass {
    let mut class = fixture(4);
    let m = &mut class.methods[0];
    m.parameters = vec!["Ljava/lang/String;".into()];
    let c = m.code.as_mut().unwrap();
    c.registers = 3;
    c.ins = 2;
    // v0 is first defined by a nonthrowing copy, before Class.forName can fail.
    c.instructions = vec![0x2007, 0x1071, 0, 2, 0x000c, 0x0011, 0x010d, 0x0011];
    c.try_regions[0].end = 5;
    c.try_regions[0].catches = Arc::from([(Some("Ljava/lang/ClassNotFoundException;".into()), 6)]);
    class
}

#[test]
fn first_definition_before_throw_is_available_to_the_handler() {
    let c = prefix_snapshot_fixture();
    let source = native_java::render_method("sample.SnapshotEnd", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("Class.forName") && source.contains("ClassNotFoundException"),
        "{source}"
    );
}

#[test]
#[ignore = "requires javac/java; serial validation"]
fn jvm_nonthrowing_prefix_preserves_handler_value() {
    let c = prefix_snapshot_fixture();
    let source = native_java::render_method("sample.SnapshotEnd", &c, &c.methods[0])
        .unwrap()
        .source;
    let root = std::env::temp_dir().join(format!("rdx-prefix-snapshot-{}", std::process::id()));
    fs::create_dir_all(root.join("sample")).unwrap();
    fs::write(root.join("sample/SnapshotEnd.java"),format!(r#"package sample;@SuppressWarnings("rawtypes") public class SnapshotEnd{{
{source}
public static void main(String[] args){{SnapshotEnd s=new SnapshotEnd();String missing=new String("rdx.MissingClass");
if(s.resolve("java.lang.String")!=String.class || s.resolve(missing)!=missing)throw new AssertionError();
try{{s.resolve(null);throw new AssertionError();}}catch(NullPointerException expected){{}}
System.out.print("ok");}}
}}"#)).unwrap();
    let out = Command::new("javac")
        .args(["-Xlint:all", "-Werror", "sample/SnapshotEnd.java"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("java")
        .args(["-cp", ".", "sample.SnapshotEnd"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"ok");
}

fn sequential_handlers(count: usize) -> rdx::native_dex::DexClass {
    let mut class = fixture(4);
    class.descriptor = "Lsample/Sequential;".into();
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.access_flags = 9;
    method.name = "run".into();
    method.parameters = vec!["I".into()];
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 1;
    code.instructions = vec![0x1001]; // counter = argument
    code.try_regions.clear();
    for _ in 0..count {
        let start = code.instructions.len() as u32;
        code.instructions.extend([0x1071, 0, 0, 0x00d8, 0x0100]);
        let end = code.instructions.len() as u32;
        code.try_regions.push(DexTryRegion {
            start,
            end,
            catches: Arc::from([(Some("Ljava/lang/RuntimeException;".into()), end)]),
        });
    }
    code.instructions.push(0x000f);
    code.tries = count as u16;
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Hooks;".into()],
        strings: vec!["touch".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    class
}

#[test]
fn forty_sequential_handlers_fit_a_two_mib_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let c = sequential_handlers(40);
            let source = native_java::render_method("sample.Sequential", &c, &c.methods[0])
                .unwrap()
                .source;
            assert_eq!(source.matches("Hooks.touch(").count(), 40, "{source}");
            assert_eq!(source.matches("catch (").count(), 40, "{source}");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[ignore = "requires javac/java; serial validation"]
fn jvm_sequential_handlers_preserve_pre_write_state() {
    let c = sequential_handlers(40);
    let source = native_java::render_method("sample.Sequential", &c, &c.methods[0])
        .unwrap()
        .source;
    let root = std::env::temp_dir().join(format!("rdx-sequential-handlers-{}", std::process::id()));
    fs::create_dir_all(root.join("sample")).unwrap();
    fs::write(
        root.join("sample/Sequential.java"),
        format!("package sample; public class Sequential {{ {source} }}"),
    )
    .unwrap();
    fs::write(root.join("sample/Hooks.java"), r#"package sample; final class Hooks {
static int calls, failEvery; static boolean fatal; static final Error ERROR=new AssertionError();
static void touch(int value){calls++;if(fatal)throw ERROR;if(failEvery>0&&calls%failEvery==0)throw new IllegalStateException();}
}"#).unwrap();
    fs::write(root.join("sample/Check.java"), r#"package sample; public class Check {
public static void main(String[] args){for(int f:new int[]{0,1,2,7}){Hooks.calls=0;Hooks.failEvery=f;
int expected=3+40-(f==0?0:40/f);if(Sequential.run(3)!=expected||Hooks.calls!=40)throw new AssertionError(f);}
Hooks.fatal=true;try{Sequential.run(3);throw new AssertionError();}catch(Error e){if(e!=Hooks.ERROR)throw e;}}
}"#).unwrap();
    let out = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Sequential.java",
            "sample/Hooks.java",
            "sample/Check.java",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn nested_handler_suffix_fixture() -> rdx::native_dex::DexClass {
    let mut class = sequential_handlers(1);
    let c = class.methods[0].code.as_mut().unwrap();
    c.instructions = vec![
        0x0138, 9, 0x1071, 0, 1, 0x0029, 2, 0x7012, 0x000f, 0x0071, 1, 0, 0x0a28, 0x000d, 0x0071,
        1, 0, 0x1012, 0x000f, 0x000d, 0x2012, 0x000f, 0x3012, 0x000f,
    ];
    c.tries = 2;
    c.try_regions = vec![
        DexTryRegion {
            start: 2,
            end: 5,
            catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 13)].into(),
        },
        DexTryRegion {
            start: 9,
            end: 19,
            catches: vec![(Some("Ljava/lang/IllegalStateException;".into()), 19)].into(),
        },
    ];
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/NestedHooks;".into()],
        strings: vec!["touch".into(), "cleanup".into()],
        protos: vec![("V".into(), vec!["I".into()]), ("V".into(), vec![])],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    class
}

#[test]
fn detached_handler_retains_its_own_protected_suffix() {
    let class = nested_handler_suffix_fixture();
    let source = native_java::render_method("sample.Sequential", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("NestedHooks.touch") && source.contains("NestedHooks.cleanup"),
        "{source}"
    );
    assert!(source.contains("IllegalStateException"), "{source}");
    let mut bad = class;
    let c = bad.methods[0].code.as_mut().unwrap();
    c.instructions[20] = 0xee28; // nested handler re-enters the protected call at 2
    assert!(native_java::render_method("sample.Sequential", &bad, &bad.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac/java; serial validation"]
fn jvm_detached_handler_keeps_nested_catch_ownership() {
    let class = nested_handler_suffix_fixture();
    let source = native_java::render_method("sample.Sequential", &class, &class.methods[0])
        .unwrap()
        .source;
    let root =
        std::env::temp_dir().join(format!("rdx-nested-handler-suffix-{}", std::process::id()));
    fs::create_dir_all(root.join("sample")).unwrap();
    fs::write(
        root.join("sample/Sequential.java"),
        format!("package sample;public class Sequential{{{source}}}"),
    )
    .unwrap();
    fs::write(root.join("sample/NestedHooks.java"), r#"package sample;final class NestedHooks{
static boolean primary,secondary,fatal;static String trace="";static final Error ERROR=new AssertionError();
static void touch(int value){trace+="T";if(primary)throw new RuntimeException();}
static void cleanup(){trace+="C";if(fatal)throw ERROR;if(secondary)throw new IllegalStateException();}
}"#).unwrap();
    fs::write(root.join("sample/Check.java"), r#"package sample;public class Check{public static void main(String[] args){
for(boolean p:new boolean[]{false,true})for(boolean s:new boolean[]{false,true})for(int x:new int[]{0,1}){
NestedHooks.primary=p;NestedHooks.secondary=s;NestedHooks.trace="";
int expected=x==0?(s?2:3):!p?7:s?2:1;
if(Sequential.run(x)!=expected||!NestedHooks.trace.equals(x==0?"C":p?"TC":"T"))throw new AssertionError();}
NestedHooks.primary=true;NestedHooks.fatal=true;try{Sequential.run(1);throw new AssertionError();}catch(Error e){if(e!=NestedHooks.ERROR)throw e;}
}}"#).unwrap();
    let out = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Sequential.java",
            "sample/NestedHooks.java",
            "sample/Check.java",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
