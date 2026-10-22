//! Midbody loop exits preserve detached abrupt tails and their ordered effects.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["touch".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![("I".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "scan".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into(), "I".into()];
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 2;
    code.outs = 1;
    // Entry1 reaches sibling14 for a negative limit. Header3's first branch
    // stays inside the loop. Later branches leave through normal continuation16
    // or effectful abrupt tail20. Latch13 has no fallthrough: sibling14 must
    // execute only through its explicit entry. Continuation16 branches, so only
    // tail20 has the linear terminal-tail proof.
    code.instructions = vec![
        0x0012, 0x023a, 13, 0x3033, 3, 0x0f28, 0x2035, 10, 0x1071, 0, 0, 0x00d8, 0x0100, 0xf628,
        0xf112, 0x010f, 0x033b, 3, 0x007b, 0x000f, 0x1071, 0, 0, 0x010a, 0x010f,
    ];
    class
}

fn source(class: &DexClass) -> String {
    native_java::render_method("sample.Hello", class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn midbody_exit_keeps_both_effect_sites_and_separate_sibling_entry() {
    let rendered = source(&fixture());
    assert_eq!(rendered.matches("while (true)").count(), 1, "{rendered}");
    assert_eq!(
        rendered.matches("sample.Effects.touch(").count(),
        2,
        "{rendered}"
    );
    assert!(!rendered.contains(".method "), "{rendered}");
}

#[test]
fn two_branching_continuations_retain_the_structured_exit_guard() {
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.truncate(20);
    // A second branching tail has two observable outcomes. Neither forward
    // destination has the linear abrupt-tail proof required for normalization.
    code.instructions
        .extend([0x0038, 4, 0x5112, 0x010f, 0x6112, 0x010f]);
    let error = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap_err();
    assert!(
        error.to_string().contains("loop has no structured exit"),
        "{error:#}"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_midbody_exits_preserve_values_order_and_exception_identity() {
    let dir =
        std::env::temp_dir().join(format!("rdx-midbody-terminal-loop-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Effects {{
  static String trace;
  static int failAt;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  public static int touch(int n) {{
    trace += n + ",";
    if (n == failAt) throw sentinel;
    return n + 100;
  }}
  {}
  public static void main(String[] args) {{
    for (int limit=-2; limit<8; limit++) for (int stop=-2; stop<9; stop++)
      for (int fail=-1; fail<9; fail++) {{
        int expected=0;
        String expectedTrace="";
        boolean expectedThrow=false;
        // Independent execution of the original DEX's ordered branches.
        int counter=0;
        if (limit<0) expected=-1;
        else for (;;) {{
          if (counter==stop) {{
            expectedTrace += counter + ",";
            expectedThrow=counter==fail;
            expected=counter+100;
            break;
          }}
          if (counter>=limit) {{ expected=stop>=0 ? counter : -counter; break; }}
          expectedTrace += counter + ",";
          if (counter==fail) {{ expectedThrow=true; break; }}
          counter++;
        }}
        trace=""; failAt=fail;
        try {{
          int actual=scan(limit,stop);
          if (expectedThrow || actual!=expected)
            throw new AssertionError("value limit="+limit+" stop="+stop+" fail="+fail+" actual="+actual);
        }} catch (RuntimeException e) {{
          if (!expectedThrow || e!=sentinel) throw new AssertionError("exception identity",e);
        }}
        if (!trace.equals(expectedTrace)) throw new AssertionError("trace "+trace+" expected "+expectedTrace);
      }}
  }}
}}
"#,
        source(&fixture())
    );
    let path = dir.join("sample/Effects.java");
    fs::write(&path, java).unwrap();
    let compiled = Command::new("javac").arg(&path).output().unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let executed = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Effects")
        .output()
        .unwrap();
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
}
