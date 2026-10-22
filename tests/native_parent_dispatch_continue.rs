//! A nested collector may continue a parent whose only exit follows dispatch.
pub use rdx::{native_cfg, native_dominators, native_ir, native_ssa};
#[allow(dead_code)]
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod forward_iteration_entry;
#[path = "../src/native_java/parent_continue.rs"]
mod plan;
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
    class.descriptor = "Lsample/Dispatch;".into();
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: vec!["parent".into(), "child".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (0, 0, 1)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.declaring_type = class.descriptor.clone();
    method.name = "run".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into(), "I".into()];
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 2;
    code.outs = 1;
    // Parent2 dispatches its first iteration back through handler7. Handler13
    // enters collector16, whose guard continues parent2. The only actual
    // parent exit is guard26->31; the first guard5->10 is internal dispatch.
    code.instructions = vec![
        0x0012, 0x0212, 0x1071, 0, 0, 0x0039, 5, 0x00d8, 0x0100, 0xf928, 0x1312, 0x3033, 15,
        0x00d8, 0x0100, 0x0112, 0x5135, 0xfff2, 0x1071, 1, 1, 0x0290, 0x0102, 0x01d8, 0x0101,
        0xf728, 0x4035, 5, 0x00d8, 0x0100, 0xe428, 0x020f,
    ];
    class
}

fn source(class: &DexClass) -> String {
    native_java::render_method("sample.Dispatch", class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn dispatch_exit_is_selected_from_natural_membership() {
    let class = fixture();
    let code = class.methods[0].code.as_ref().unwrap();
    let plans = plan::select_all(code).expect("owned dispatch parent");
    assert_eq!(plans.len(), 1);
    assert!(plan::validate_all(code, &plans));
    let selected = &plans[0];
    assert_eq!(
        (selected.parent, selected.parent_guard, selected.exit),
        (2, 26, 31)
    );
    assert_eq!(
        (selected.child, selected.child_guard, selected.child_latch),
        (16, 16, 25)
    );
    assert!(selected.parent_nodes.contains(&5));
    assert!(selected.parent_nodes.contains(&30));
    assert!(plan::validate(code, selected));
    let rendered = source(&class);
    assert_eq!(rendered.matches("while (true)").count(), 2, "{rendered}");
    assert_eq!(
        rendered.matches("sample.Effects.parent(").count(),
        1,
        "{rendered}"
    );
    assert_eq!(
        rendered.matches("sample.Effects.child(").count(),
        1,
        "{rendered}"
    );
}

#[test]
fn a_second_parent_exit_edge_keeps_ownership_ambiguous() {
    let mut class = fixture();
    // Dispatch11 can now leave directly through31 as well as guard26. A shared
    // destination does not prove a unique owning exit instruction.
    class.methods[0].code.as_mut().unwrap().instructions[12] = 20;
    let code = class.methods[0].code.as_ref().unwrap();
    assert!(plan::select_all(code).is_none());
    assert!(native_java::render_method("sample.Dispatch", &class, &class.methods[0]).is_err());
}

#[test]
fn an_external_child_body_entry_keeps_the_single_entry_guard() {
    let mut class = fixture();
    let code = class.methods[0].code.as_mut().unwrap();
    // This entry skips the parent header and collector guard. Internal relative
    // branches retain their original destinations after the two-word shift.
    let mut words = vec![0x0438, 20];
    words.extend_from_slice(&code.instructions);
    code.instructions = words;
    assert!(plan::select_all(code).is_none());
    assert!(native_java::render_method("sample.Dispatch", &class, &class.methods[0]).is_err());
}

#[test]
fn stale_dispatch_plan_is_rejected_after_an_edge_mutation() {
    let mut class = fixture();
    let original = plan::select_all(class.methods[0].code.as_ref().unwrap())
        .unwrap()
        .remove(0);
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[12] = 20;
    assert!(!plan::validate(code, &original));
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_dispatch_parent_continues_preserve_order_values_and_exception_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-parent-dispatch-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Effects {{
  static String trace;
  static int failAt, calls;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  static void record(String tag, int value) {{
    trace += tag + value + ";";
    if (calls++ == failAt) throw sentinel;
  }}
  public static void parent(int n) {{ record("p",n); }}
  public static void child(int n) {{ record("c",n); }}
  {}
  public static void main(String[] args) {{
    for (int outer=-2; outer<8; outer++) for (int inner=-2; inner<8; inner++)
      for (int fail=-1; fail<20; fail++) {{
        String expectedTrace="";
        int expectedCalls=0, sum=0;
        boolean expectedThrow=false;
        // Independent execution of the original dispatch/collector state machine.
        int counter=0;
        oracle: for (;;) {{
          expectedTrace += "p" + counter + ";";
          if (expectedCalls++ == fail) {{ expectedThrow=true; break; }}
          if (counter==0) {{ counter++; continue; }}
          if (counter==1) {{
            counter++;
            for (int child=0; child<inner; child++) {{
              expectedTrace += "c" + child + ";";
              if (expectedCalls++ == fail) {{ expectedThrow=true; break oracle; }}
              sum += child;
            }}
            continue;
          }}
          if (counter>=outer) break;
          counter++;
        }}
        trace=""; calls=0; failAt=fail;
        try {{
          int actual=run(outer,inner);
          if (expectedThrow || actual!=sum) throw new AssertionError("value "+actual+" expected "+sum);
        }} catch (RuntimeException e) {{
          if (!expectedThrow || e!=sentinel) throw new AssertionError("exception identity",e);
        }}
        if (!trace.equals(expectedTrace) || calls!=expectedCalls)
          throw new AssertionError("trace "+trace+" expected "+expectedTrace);
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
