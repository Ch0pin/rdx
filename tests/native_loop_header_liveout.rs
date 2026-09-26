//! A loop header's definitions are live at exit, not necessarily at entry.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(old_type: bool) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["touch".into(), "old".into()],
        types: vec!["Lsample/Header;".into()],
        protos: vec![("I".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let m = &mut class.methods[0];
    m.name = if old_type { "oldType" } else { "missing" }.into();
    m.access_flags = 9;
    m.parameters = vec!["I".into()];
    m.return_type = "I".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    code.outs = 1;
    // v1 is undefined or String before the loop. Every header evaluation
    // defines it as int, including the initial zero-body-iterations check.
    code.instructions = vec![0x0012];
    if old_type {
        code.instructions.extend([0x011a, 1]);
    }
    code.instructions.extend([
        0x1071, 0, 0, 0x010a, 0x2035, 5, 0x00d8, 0x0100, 0xf828, 0x010f,
    ]);
    class
}
fn source(old_type: bool) -> String {
    let class = fixture(old_type);
    native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source
}
#[test]
fn ordinary_loop_carries_header_definition_to_exit() {
    for old_type in [false, true] {
        let rendered = source(old_type);
        assert_eq!(
            rendered.matches("sample.Header.touch(").count(),
            1,
            "{rendered}"
        );
        assert!(
            rendered.find("while (true)").unwrap() < rendered.find("sample.Header.touch(").unwrap(),
            "{rendered}"
        );
    }
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn header_liveout_jvm_preserves_zero_iteration_values_and_call_order() {
    let dir = std::env::temp_dir().join(format!("rdx-header-liveout-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Header {{
  static int calls, throwAt;
  static final RuntimeException sentinel=new RuntimeException("sentinel");
  public static int touch(int n) {{
    if(n!=calls++) throw new AssertionError("order");
    if(n==throwAt) throw sentinel;
    return n+100;
  }}
  {}
  {}
  public static void main(String[] args) {{
    for(int limit=-1; limit<9; limit++) for(int form=0; form<2; form++)
      for(int fail=-1; fail<10; fail++) {{
        calls=0; throwAt=fail;
        int last=Math.max(0,limit);
        boolean throwsExpected=fail>=0 && fail<=last;
        try {{
          int actual=form==0 ? missing(limit) : oldType(limit);
          if(throwsExpected || actual!=last+100) throw new AssertionError("result");
        }} catch(RuntimeException e) {{
          if(!throwsExpected || e!=sentinel) throw new AssertionError("exception identity",e);
        }}
        if(calls!=(throwsExpected ? fail+1 : last+1)) throw new AssertionError("calls");
      }}
  }}
}}
"#,
        source(false),
        source(true)
    );
    fs::write(dir.join("sample/Header.java"), &java).unwrap();
    for (program, argument) in [("javac", "sample/Header.java"), ("java", "sample.Header")] {
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
    fs::remove_dir_all(dir).unwrap();
}
