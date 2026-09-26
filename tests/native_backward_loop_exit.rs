//! Loop exits follow control-flow membership, not instruction address order.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(conditional: bool) -> DexClass {
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
    let m = &mut class.methods[0];
    m.name = if conditional { "conditional" } else { "jump" }.into();
    m.access_flags = 9;
    m.parameters = vec!["I".into(), "I".into()];
    m.return_type = "I".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 2;
    code.outs = 1;
    // The effectful exit is at pc2, before the loop header at pc7. It is
    // entered only when the counter matches the second argument. The normal
    // exit returns the exhausted counter without executing touch.
    code.instructions = if conditional {
        vec![
            0x0012, 0x0628, 0x1071, 0, 0, 0x000a, 0x000f, 0x1035, 7, 0x2032, 0xfff9, 0x00d8,
            0x0100, 0xfa28, 0x000f,
        ]
    } else {
        vec![
            0x0012, 0x0628, 0x1071, 0, 0, 0x000a, 0x000f, 0x1035, 8, 0x2033, 3, 0xf728, 0x00d8,
            0x0100, 0xf928, 0x000f,
        ]
    };
    class
}

fn downstream_loop_fixture() -> DexClass {
    let mut class = fixture(true);
    let method = &mut class.methods[0];
    method.name = "chained".into();
    let words = &mut method.code.as_mut().unwrap().instructions;
    words[6] = 0x0928; // after touch, jump across first loop to the second
    words.extend([
        0x0212, 0x1235, 7, 0x02d8, 0x0102, 0x00d8, 0x0200, 0xfa28, 0x000f,
    ]);
    class
}

#[test]
fn escape_can_enter_a_later_independent_loop_but_cannot_reenter_original_loop() {
    let mut class = downstream_loop_fixture();
    let rendered = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    assert_eq!(rendered.matches("while (true)").count(), 2, "{rendered}");
    // A secondary downstream exit returning to the original loop cannot be
    // hidden by treating the downstream loop as a header -> ordinary-exit edge.
    class.methods[0].code.as_mut().unwrap().instructions[17] = 0xfff7; // pc16 -> pc7
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
fn backward_escape_into_protected_tail_interior_is_rejected() {
    let mut class = fixture(true);
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x0012, 0x0728, 0, 0x1071, 0, 0, 0x000a, 0x000f, 0x1035, 9, 0x2032, 0xfff9, 0x0038, 0xfff6,
        0x00d8, 0x0100, 0xf828, 0x000f, 0x000d, 0xf012, 0x000f,
    ];
    code.tries = 1;
    code.try_regions = vec![native_dex::DexTryRegion {
        start: 2,
        end: 7,
        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 18)].into(),
    }];
    let error = native_java::render_method("sample.Hello", &class, &class.methods[0]).unwrap_err();
    assert!(
        error.to_string().contains("unproven backward loop escape"),
        "{error:#}"
    );
}

fn source(conditional: bool) -> String {
    let class = fixture(conditional);
    native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source
}

#[test]
fn backward_conditional_and_jump_exits_keep_the_effectful_tail() {
    for conditional in [true, false] {
        let source = source(conditional);
        assert_eq!(source.matches("while (true)").count(), 1, "{source}");
        assert_eq!(
            source.matches("sample.Effects.touch(").count(),
            1,
            "{source}"
        );
        assert!(
            source.find("while (true)").unwrap() < source.find("sample.Effects.touch(").unwrap(),
            "{source}"
        );
    }
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn backward_exits_jvm_preserve_values_effect_count_and_exception_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-backward-loop-exit-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Effects {{
  static int calls;
  static boolean fail;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  public static int touch(int n) {{ calls++; if(fail) throw sentinel; return n + 100; }}
  {}
  {}
  {}
  public static void main(String[] args) {{
    for(int limit=-1; limit<9; limit++) for(int stop=-1; stop<10; stop++)
      for(int form=0; form<3; form++) for(int mode=0; mode<2; mode++) {{
        calls=0; fail=mode==1;
        boolean hit=stop>=0 && stop<limit;
        try {{
          int actual=form==0 ? conditional(limit,stop) : form==1 ? jump(limit,stop) : chained(limit,stop);
          if(fail && hit) throw new AssertionError("missing exception");
          int expected=hit ? stop+100+(form==2 ? 2*limit:0) : Math.max(0,limit);
          if(actual!=expected) throw new AssertionError("value "+actual+" expected "+expected);
        }} catch(RuntimeException e) {{
          if(!hit || !fail || e!=sentinel) throw new AssertionError("exception identity",e);
        }}
        if(calls!=(hit ? 1:0)) throw new AssertionError("effect count "+calls);
      }}
  }}
}}
"#,
        source(true),
        source(false),
        {
            let class = downstream_loop_fixture();
            native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source
        }
    );
    fs::write(dir.join("sample/Effects.java"), &java).unwrap();
    for (program, argument) in [("javac", "sample/Effects.java"), ("java", "sample.Effects")] {
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
