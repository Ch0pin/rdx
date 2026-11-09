//! A try inside a loop may `continue` directly from its protected body, and
//! its catch may be a `move-exception; goto` stub whose tail lies after the
//! loop latch and continues the loop (the R8 layout of Jackson field loops).
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.descriptor = "Lsample/Scan;".into();
    class.symbols = Arc::new(DexSymbols {
        strings: vec![
            "more".into(),
            "step".into(),
            "touch".into(),
            "caught".into(),
        ],
        types: vec![
            "Lsample/Scan;".into(),
            "Ljava/lang/RuntimeException;".into(),
        ],
        protos: vec![
            ("Z".into(), vec!["I".into()]),
            ("I".into(), vec!["I".into()]),
            ("V".into(), vec!["I".into()]),
            ("V".into(), vec!["Ljava/lang/RuntimeException;".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 3, 3)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "run".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into()];
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 1;
    code.outs = 1;
    code.tries = 1;
    // 00 move v0, v3
    // 01 invoke-static {v0} more(I)Z        <- loop header
    // 04 move-result v1
    // 05 if-eqz v1, @20                     <- loop exit
    // 07 invoke-static {v0} step(I)I
    // 0a move-result v0
    // 0b rem-int/lit8 v1, v0, #3            <- try start
    // 0d if-eqz v1, @01                     <- direct continue inside try
    // 0f const/4 v2, #1
    // 10 if-ne v1, v2, @18
    // 12 invoke-static {v0} touch(I)V
    // 15 goto @01                           <- continue after an effect
    // 16 move-exception v2                  <- handler stub inside the range
    // 17 goto @1c
    // 18 invoke-static {v0} touch(I)V
    // 1b goto @01                           <- latch, try end
    // 1c invoke-static {v2} caught(RuntimeException)V  <- tail after latch
    // 1f goto @01
    // 20 return v0
    code.instructions = vec![
        0x3001, 0x1071, 0x0000, 0x0000, 0x010a, 0x0138, 0x001b, 0x1071, 0x0001, 0x0000, 0x000a,
        0x01dc, 0x0300, 0x0138, 0xfff4, 0x1212, 0x2133, 0x0008, 0x1071, 0x0002, 0x0000, 0xec28,
        0x020d, 0x0528, 0x1071, 0x0002, 0x0000, 0xe628, 0x1071, 0x0003, 0x0002, 0xe228, 0x000f,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 0xb,
        end: 0x1b,
        catches: vec![(Some(Arc::from("Ljava/lang/RuntimeException;")), 0x16)].into(),
    }];
    class
}

fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Scan", class, &class.methods[0])?.source)
}

#[test]
fn protected_body_continues_and_stub_catch_tail_continues_loop() {
    let java = source(&fixture()).unwrap();
    assert_eq!(java.matches("while (true)").count(), 1, "{java}");
    assert_eq!(java.matches("try {").count(), 1, "{java}");
    assert_eq!(java.matches("touch(").count(), 2, "{java}");
    assert_eq!(java.matches("caught(").count(), 1, "{java}");
    assert!(java.contains("catch (java.lang.RuntimeException"), "{java}");
    assert!(java.matches("continue;").count() >= 3, "{java}");
    // The continue statements of the protected body sit inside the try.
    let try_at = java.find("try {").unwrap();
    let catch_at = java.find("} catch (").unwrap();
    assert!(
        java[try_at..catch_at].matches("continue;").count() >= 2,
        "{java}"
    );
    // The catch tail runs the handler call, then continues the loop.
    let caught_at = java.find("caught(").unwrap();
    assert!(caught_at > catch_at, "{java}");
    assert!(
        java[caught_at..].trim_start().contains("continue;"),
        "{java}"
    );
    assert!(java.contains("return "), "{java}");
    assert!(!java.contains(".method "), "{java}");
}

#[test]
fn catch_tail_re_entering_the_protected_body_is_rejected() {
    let mut class = fixture();
    // goto @0b from the handler tail jumps back into the try instead of the header.
    class.methods[0].code.as_mut().unwrap().instructions[0x1f] = 0xec28;
    assert!(source(&class).is_err());
}

#[test]
fn direct_continue_from_try_without_handler_stub_renders() {
    // Replace the stub with a plain throw-free handler that returns, keeping
    // the direct protected continue edges as the only loop re-entries.
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.truncate(0x1c);
    code.instructions.push(0x000f); // 1c: return v0, the loop exit
    code.instructions[0x06] = 0x0017; // if-eqz v1, @1c
    code.instructions[0x17] = 0x000f; // the catch returns v0
    let java = source(&class).unwrap();
    assert_eq!(java.matches("while (true)").count(), 1, "{java}");
    assert!(java.contains("catch (java.lang.RuntimeException"), "{java}");
    assert!(java.matches("continue;").count() >= 2, "{java}");
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn loop_continue_try_jvm_preserves_effects_and_exception_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-loop-continue-try-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Scan {{
 static int calls,catches,steps,failAt;
 static final RuntimeException sentinel=new RuntimeException();
 static boolean more(int n) {{ return n < 12; }}
 static int step(int n) {{ steps++; return n + 1; }}
 static void touch(int n) {{ calls++; if (n == failAt) throw sentinel; }}
 static void caught(RuntimeException e) {{ if (e != sentinel) throw new AssertionError("identity", e); catches++; }}
 {}
 public static void main(String[] args) {{
   for (failAt = -1; failAt <= 12; failAt++) {{
     calls = 0; catches = 0; steps = 0;
     int result = run(0);
     int expectedCatches = failAt > 0 && failAt % 3 != 0 ? 1 : 0;
     if (result != 12 || steps != 12 || calls != 8 || catches != expectedCatches)
       throw new AssertionError(failAt + ": " + result + " " + steps + " " + calls + " " + catches);
   }}
 }}
}}"#,
        source(&fixture()).unwrap()
    );
    fs::write(dir.join("sample/Scan.java"), &java).unwrap();
    for (program, argument) in [("javac", "sample/Scan.java"), ("java", "sample.Scan")] {
        let result = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{program}: {}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(&dir).unwrap();
}
