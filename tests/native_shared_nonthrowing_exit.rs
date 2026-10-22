//! Shared pure exit assignments retain their value on normal and catch paths.
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn catch_shared_assignment() -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["effect".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![("I".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.parameters.clear();
    let code = m.code.as_mut().unwrap();
    code.registers = 2;
    code.ins = 0;
    code.outs = 0;
    code.tries = 1;
    code.instructions = vec![
        0x0071, 0, 0, 0x000a, 0x0039, 4, 0x0012, 0x0528, 0x1012, 0x0328, 0x010d, 0xfb28, 0x000f,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 10,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 10)].into(),
    }];
    c
}

#[test]
fn catch_can_share_nonthrowing_assignment_inside_protected_address_range() {
    let c = catch_shared_assignment();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("catch (java.lang.RuntimeException"),
        "{source}"
    );
    assert_eq!(
        source.matches("sample.Effects.effect()").count(),
        1,
        "{source}"
    );
}

#[test]
fn catch_reentering_throwing_protected_instruction_remains_rejected() {
    let mut c = catch_shared_assignment();
    // Catch jumps to the protected invocation, changing exception dispatch.
    c.methods[0].code.as_mut().unwrap().instructions[11] = 0xf528;
    let error = native_java::render_method("sample.Hello", &c, &c.methods[0]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("outside handler re-enters protected region"),
        "{error}"
    );
}

fn backward_assignment_with_protected_continuation() -> DexClass {
    let mut c = catch_shared_assignment();
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["touch".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![("I".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.parameters = vec!["I".into(), "I".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 2;
    code.outs = 1;
    code.instructions = vec![
        0x0012, 0x0328, 0x7012, 0x0928, 0x1035, 7, 0x2032, 0xfffc, 0x00d8, 0x0100, 0xfa28, 0x000f,
        0x1071, 0, 0, 0x000a, 0x0428, 0x000d, 0xf012, 0x0128, 0x000f,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 12,
        end: 16,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 17)].into(),
    }];
    c
}

#[test]
fn backward_shared_assignment_can_reach_a_complete_protected_continuation() {
    let c = backward_assignment_with_protected_continuation();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("while (true)"), "{source}");
    assert!(
        source.contains("catch (java.lang.RuntimeException"),
        "{source}"
    );
    assert!(source.contains("sample.Effects.touch"), "{source}");
}

fn forward_protected_continuation() -> DexClass {
    let mut c = backward_assignment_with_protected_continuation();
    let code = c.methods[0].code.as_mut().unwrap();
    // Exhaustion leaves the loop for the protected tail; a successful search
    // returns directly from the latch fallthrough without calling touch.
    code.instructions = vec![
        0x0012, 0x1035, 7, 0x00d8, 0x0100, 0x2033, 0xfffc, 0x000f, 0x1071, 0, 0, 0x000a, 0x0428,
        0x000d, 0xf012, 0x0128, 0x000f,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 8,
        end: 12,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 13)].into(),
    }];
    c
}

#[test]
fn forward_loop_exit_preserves_its_complete_protected_continuation() {
    let c = forward_protected_continuation();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("while (true)"), "{source}");
    assert!(
        source.contains("catch (java.lang.RuntimeException"),
        "{source}"
    );
}

#[test]
fn forward_exit_cannot_reenter_loop_through_handler_or_protected_interior() {
    let mut c = forward_protected_continuation();
    c.methods[0].code.as_mut().unwrap().instructions[15] = 0xf228; // handler goto pc1
    assert!(native_java::render_method("sample.Hello", &c, &c.methods[0]).is_err());
    let mut c = forward_protected_continuation();
    // The exit would consume a pending call result in the middle of a try.
    c.methods[0].code.as_mut().unwrap().instructions[2] = 10; // pc1 -> pc11
    assert!(native_java::render_method("sample.Hello", &c, &c.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac and java"]
fn forward_exit_jvm_keeps_success_exhaustion_and_exception_paths_separate() {
    use std::{fs, process::Command};
    let c = forward_protected_continuation();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    let dir =
        std::env::temp_dir().join(format!("rdx-forward-protected-exit-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Hello.java"), format!(r#"package sample;
public class Hello {{
{}
public static void main(String[] args) {{
 if(run(3,1)!=1 || Effects.calls!=0)throw new AssertionError("successful search");
 for(int mode=0;mode<3;mode++) {{Effects.mode=mode;Effects.calls=0;
  try {{int result=run(3,9);if(mode==2 || result!=(mode==0?103:-1))throw new AssertionError("exhaustion value");}}
  catch(Error actual) {{if(mode!=2 || actual!=Effects.unmatched)throw actual;}}
  if(Effects.calls!=1)throw new AssertionError("effect count");
 }}
}}
}}
class Effects {{
static int mode,calls;
static final Error unmatched=new AssertionError("unmatched");
static int touch(int value) {{calls++;if(mode==1)throw new RuntimeException();if(mode==2)throw unmatched;return value+100;}}
}}
"#,source)).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Hello.java"]),
        ("java", vec!["-cp", ".", "sample.Hello"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "requires javac and java"]
fn backward_assignment_jvm_preserves_loop_exit_catch_and_effect_count() {
    use std::{fs, process::Command};
    let c = backward_assignment_with_protected_continuation();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-backward-shared-exit-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Hello.java"), format!(r#"package sample;
public class Hello {{
{}
public static void main(String[] args) {{
 if(run(0,1)!=0 || run(3,9)!=3 || Effects.calls!=0)throw new AssertionError("normal loop exit");
 for(int mode=0;mode<3;mode++) {{Effects.mode=mode;Effects.calls=0;
  try {{int result=run(3,1);if(mode==2 || result!=(mode==0?107:-1))throw new AssertionError("value");}}
  catch(Error actual) {{if(mode!=2 || actual!=Effects.unmatched)throw actual;}}
  if(Effects.calls!=1)throw new AssertionError("effect count");
 }}
}}
}}
class Effects {{
static int mode,calls;
static final Error unmatched=new AssertionError("unmatched");
static int touch(int value) {{calls++;if(mode==1)throw new RuntimeException();if(mode==2)throw unmatched;return value+100;}}
}}
"#,source)).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Hello.java"]),
        ("java", vec!["-cp", ".", "sample.Hello"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn shared_catch_assignment_jvm_preserves_normal_catch_and_unmatched_exception() {
    use std::{fs, process::Command};
    let c = catch_shared_assignment();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-shared-exit-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Hello.java"), format!(r#"package sample;
public class Hello {{
{}
public static void main(String[] args) {{
 for(int mode=0;mode<4;mode++) {{ Effects.mode=mode;Effects.calls=0;
  try {{ int result=run(); if(mode==3 || result!=(mode==1?1:0))throw new AssertionError("value"); }}
  catch(Error actual) {{ if(mode!=3 || actual!=Effects.unmatched)throw actual; }}
  if(Effects.calls!=1)throw new AssertionError("effect count");
 }}
}}
}}
class Effects {{
static int mode,calls;
static final Error unmatched=new AssertionError("unmatched");
static int effect() {{ calls++;if(mode==2)throw new RuntimeException();if(mode==3)throw unmatched;return mode; }}
}}
"#, source)).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Hello.java"]),
        ("java", vec!["-cp", ".", "sample.Hello"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
