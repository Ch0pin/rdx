//! G01-A production rendering: call effects, wide words and source identity.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(range: bool) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["first".into(), "second".into(), "combine".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![
            ("J".into(), vec!["I".into()]),
            ("J".into(), vec!["J".into(), "J".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = if range { "ranged" } else { "evaluate" }.into();
    method.parameters = vec!["I".into()];
    method.return_type = "J".into();
    method.access_flags = 9;
    let code = method.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 1;
    code.outs = 4;
    // first(v4) -> v0/v1; second(v4) -> v2/v3;
    // combine(v0/v1, v2/v3) overwrites the first wide argument.
    code.instructions = vec![0x1071, 0, 4, 0x000b, 0x1071, 1, 4, 0x020b];
    code.instructions.extend(if range {
        [0x0477, 2, 0]
    } else {
        [0x4071, 2, 0x3210]
    });
    code.instructions.extend([0x000b, 0x0010]);
    class
}

#[test]
fn wide_call_forms_preserve_calls_and_exact_navigation_spans() {
    for range in [false, true] {
        let class = fixture(range);
        let rendered = native_java::render_method("sample.Hello", &class, &class.methods[0])
            .expect("straight-line Java");
        let mut spans = Vec::new();
        for (name, signature) in [("first", "(I)J"), ("second", "(I)J"), ("combine", "(JJ)J")] {
            let label = format!("sample.Effects.{name}{signature}");
            let links: Vec<_> = rendered.links.iter().filter(|l| l.label == label).collect();
            assert_eq!(links.len(), 1, "{label}: {}", rendered.source);
            let link = links[0];
            spans.push((link.start, link.end));
            assert_eq!(
                rendered
                    .source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>(),
                name,
                "original signature must navigate from the emitted identifier"
            );
        }
        // Nested calls need not appear in execution order in Java source.
        // The JVM fixture below verifies effect order and exception identity;
        // here each original method must retain its own nonoverlapping span.
        spans.sort_unstable();
        assert!(spans.windows(2).all(|pair| pair[0].1 <= pair[1].0));
        assert!(!rendered.source.contains(".method"));
    }
}

#[test]
fn malformed_wide_call_binding_is_rejected_by_public_renderer() {
    for malformed in [vec![0x3071, 2, 0x0210], vec![0x4071, 2, 0x3212]] {
        let mut class = fixture(false);
        class.methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions
            .splice(8..11, malformed);
        assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
    }
}

#[test]
fn incompatible_move_result_is_rejected_by_public_renderer() {
    let mut class = fixture(false);
    class.methods[0].code.as_mut().unwrap().instructions[3] = 0x000a;
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
fn result_separated_from_its_call_is_rejected_by_public_renderer() {
    let mut class = fixture(false);
    // A nop between invoke and move-result must not reconnect a stale result.
    class.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .insert(3, 0);
    assert!(native_java::render_method("sample.Hello", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn straight_line_jvm_preserves_wide_values_effect_order_and_exception_identity() {
    let methods: Vec<_> = [false, true]
        .into_iter()
        .map(|range| {
            let class = fixture(range);
            native_java::render_method("sample.Hello", &class, &class.methods[0])
                .unwrap()
                .source
        })
        .collect();
    let java = format!(
        r#"package sample;
public class Effects {{
  static String trace; static int fail;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  static void effect(String name, int stage) {{
    trace += name;
    if (fail == stage) throw sentinel;
  }}
  public static long first(int n) {{ effect("F", 0); return ((long)n << 35) + 7; }}
  public static long second(int n) {{ effect("S", 1); return ~((long)n << 33); }}
  public static long combine(long a, long b) {{ effect("C", 2); return a ^ b; }}
  {}
  {}
  public static void main(String[] args) {{
    for (int n : new int[] {{Integer.MIN_VALUE, -1, 0, 1, Integer.MAX_VALUE}})
      for (int form = 0; form < 2; form++) for (fail = -1; fail < 3; fail++) {{
        trace = "";
        try {{
          long result = form == 0 ? evaluate(n) : ranged(n);
          if (fail != -1 || result != ((((long)n << 35) + 7) ^ ~((long)n << 33)))
            throw new AssertionError("wide result or missing exception");
        }} catch (RuntimeException ex) {{
          if (fail == -1 || ex != sentinel) throw new AssertionError("exception identity", ex);
        }}
        String expected = fail == 0 ? "F" : fail == 1 ? "FS" : "FSC";
        if (!trace.equals(expected)) throw new AssertionError("effect order: " + trace);
      }}
  }}
}}
"#,
        methods[0], methods[1]
    );
    let dir = std::env::temp_dir().join(format!("rdx-straight-line-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
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
