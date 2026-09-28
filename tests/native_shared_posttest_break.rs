//! G01-C-posttest-loop-break: one canonical body break before a conditional latch.
use rdx::{
    native_dex::{self, DexSymbols},
    native_java,
};
use std::{collections::BTreeMap, fs, process::Command, sync::Arc};

#[derive(Default)]
struct Words {
    code: Vec<u16>,
    labels: BTreeMap<&'static str, usize>,
    patches: Vec<(usize, &'static str)>,
}
impl Words {
    fn emit(&mut self, words: &[u16]) {
        self.code.extend_from_slice(words);
    }
    fn label(&mut self, label: &'static str) {
        assert!(self.labels.insert(label, self.code.len()).is_none());
    }
    fn branch(&mut self, opcode: u16, label: &'static str) {
        let pc = self.code.len();
        self.emit(&[opcode, 0]);
        self.patches.push((pc, label));
    }
    fn finish(mut self) -> Vec<u16> {
        for (pc, label) in self.patches {
            self.code[pc + 1] = (self.labels[label] as isize - pc as isize) as i16 as u16;
        }
        self.code
    }
}
fn render_with_hooks(
    name: &str,
    words: Vec<u16>,
    locals: u16,
    params: u16,
    hooks: bool,
) -> anyhow::Result<String> {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))?
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = name.into();
    method.access_flags = 9;
    method.parameters = (0..params).map(|_| "I".into()).collect();
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = locals + params;
    code.ins = params;
    code.outs = if hooks { 1 } else { 0 };
    code.instructions = words;
    if hooks {
        class.symbols = Arc::new(DexSymbols {
            strings: vec!["before".into(), "after".into()],
            types: vec!["Lsample/Hook;".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0), (0, 0, 1)],
            ..Default::default()
        });
    }
    Ok(native_java::render_method("sample.PosttestBreak", &class, &class.methods[0])?.source)
}
fn render(name: &str, words: Vec<u16>, locals: u16, params: u16) -> anyhow::Result<String> {
    render_with_hooks(name, words, locals, params, false)
}
fn render_typed(
    name: &str,
    words: Vec<u16>,
    locals: u16,
    parameters: &[&str],
    result: &str,
) -> anyhow::Result<String> {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))?
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = name.into();
    method.access_flags = 9;
    method.parameters = parameters.iter().map(|ty| (*ty).into()).collect();
    method.return_type = result.into();
    let code = method.code.as_mut().unwrap();
    code.ins = parameters
        .iter()
        .map(|ty| if matches!(*ty, "J" | "D") { 2 } else { 1 })
        .sum();
    code.registers = locals + code.ins;
    code.outs = 0;
    code.instructions = words;
    Ok(native_java::render_method("sample.PosttestBreak", &class, &class.methods[0])?.source)
}
fn before_effects() -> Vec<u16> {
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("body");
    w.emit(&[0x01d8, 0x0201]); // mandatory sum += 2
    w.branch(0x0338, "exit"); // break if selector == 0
    w.emit(&[0x01d8, 0x0301, 0x00d8, 0x0100]);
    w.branch(0x2034, "body");
    w.label("exit");
    w.emit(&[0x010f]);
    w.finish()
}
fn after_diamond() -> Vec<u16> {
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("body");
    w.branch(0x0338, "zero");
    w.emit(&[0x01d8, 0x0301]);
    w.branch(0x0029, "join");
    w.label("zero");
    w.emit(&[0x01d8, 0x0501]);
    w.label("join");
    w.branch(0x4135, "exit"); // break if sum >= stop
    w.emit(&[0x00d8, 0x0100]);
    w.branch(0x2034, "body");
    w.label("exit");
    w.emit(&[0x010f]);
    w.finish()
}
fn observable_effects() -> Vec<u16> {
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("body");
    w.emit(&[0x1071, 0, 0]); // Hook.before(v0)
    w.branch(0x0338, "exit");
    w.emit(&[0x1071, 1, 0]); // Hook.after(v0)
    w.emit(&[0x01d8, 0x0201, 0x00d8, 0x0100]);
    w.branch(0x2034, "body");
    w.label("exit");
    w.emit(&[0x010f]);
    w.finish()
}
fn multiple_break_effects() -> Vec<u16> {
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]); // index=0, sum=0
    w.label("body");
    w.emit(&[0x1071, 0, 0]); // Hook.before(index)
    w.emit(&[0x01d8, 0x0101]); // sum += 1
    w.branch(0x3032, "exit"); // if index == first
    w.emit(&[0x1071, 1, 0]); // Hook.after(index)
    w.emit(&[0x01d8, 0x0201]); // sum += 2
    w.branch(0x4032, "exit"); // if index == second
    w.emit(&[0x01d8, 0x0301, 0x00d8, 0x0100]); // sum += 3; index++
    w.branch(0x2034, "body"); // if index < limit
    w.label("exit");
    w.emit(&[0x010f]);
    w.finish()
}
fn multiple_break_references() -> Vec<u16> {
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("body");
    w.emit(&[0x5107]); // selected reference = first object
    w.branch(0x3032, "exit"); // index == first selector
    w.emit(&[0x6107]); // selected reference = second object
    w.branch(0x4032, "exit"); // index == second selector
    w.emit(&[0x5107, 0x00d8, 0x0100]);
    w.branch(0x2034, "body");
    w.label("exit");
    w.emit(&[0x0111]);
    w.finish()
}
fn multiple_break_wide() -> Vec<u16> {
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("body");
    w.emit(&[0x6104]); // selected wide = first long
    w.branch(0x4032, "exit");
    w.emit(&[0x8104]); // selected wide = second long
    w.branch(0x5032, "exit");
    w.emit(&[0x6104, 0x00d8, 0x0100]);
    w.branch(0x3034, "body");
    w.label("exit");
    w.emit(&[0x0110]);
    w.finish()
}
#[test]
fn renders_mandatory_body_and_distinct_early_exit() {
    for (name, words, params) in [
        ("beforeEffects", before_effects(), 2),
        ("afterDiamond", after_diamond(), 3),
    ] {
        let source =
            render(name, words, 2, params).unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert_eq!(source.matches("while (").count(), 1, "{source}");
        assert!(source.contains("break;"), "{source}");
    }
    let source = render_with_hooks("observableEffects", observable_effects(), 2, 2, true).unwrap();
    assert!(source.contains("sample.Hook.before"), "{source}");
    assert!(source.contains("sample.Hook.after"), "{source}");
}
#[test]
fn rejects_undefined_early_exit() {
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("body");
    w.branch(0x0238, "exit");
    w.emit(&[0x1112, 0x00d8, 0x0100]);
    w.branch(0x1034, "body");
    w.label("exit");
    w.emit(&[0x010f]);
    assert!(render("undefinedExit", w.finish(), 2, 2).is_err());
}
#[test]
fn multiple_breaks_keep_distinct_exit_values_and_effects() {
    let source = render_with_hooks("multipleBreaks", multiple_break_effects(), 2, 3, true).unwrap();
    assert_eq!(source.matches("while (").count(), 1, "{source}");
    assert_eq!(source.matches("break;").count(), 3, "{source}");
    assert_eq!(source.matches("Hook.before(").count(), 1, "{source}");
    assert_eq!(source.matches("Hook.after(").count(), 1, "{source}");
}
#[test]
fn multiple_breaks_keep_reference_and_wide_liveouts() {
    let references = render_typed(
        "referenceBreaks",
        multiple_break_references(),
        2,
        &["I", "I", "I", "Ljava/lang/Object;", "Ljava/lang/Object;"],
        "Ljava/lang/Object;",
    )
    .unwrap();
    let wide = render_typed(
        "wideBreaks",
        multiple_break_wide(),
        3,
        &["I", "I", "I", "J", "J"],
        "J",
    )
    .unwrap();
    assert_eq!(references.matches("break;").count(), 3, "{references}");
    assert_eq!(wide.matches("break;").count(), 3, "{wide}");
}
#[test]
fn multiple_breaks_reject_undefined_first_exit() {
    let mut words = multiple_break_effects();
    words[0] = 0x0000; // index remains undefined before the first body call
    assert!(render_with_hooks("undefinedFirst", words, 2, 3, true).is_err());
}
#[test]
fn multiple_breaks_reject_incompatible_exit_types() {
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]); // int exit value
    w.label("body");
    w.branch(0x3032, "exit"); // first exit carries int
    w.emit(&[0x5107]); // v1 now holds an Object
    w.branch(0x4032, "exit"); // second exit carries Object
    w.emit(&[0x00d8, 0x0100]);
    w.branch(0x2034, "body");
    w.label("exit");
    w.emit(&[0x010f]); // an int return cannot accept both types
    assert!(
        render_typed(
            "incompatibleExits",
            w.finish(),
            2,
            &["I", "I", "I", "Ljava/lang/Object;"],
            "I"
        )
        .is_err()
    );
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_multiple_breaks_preserve_effect_order_and_exception_identity() {
    let method = render_with_hooks("multipleBreaks", multiple_break_effects(), 2, 3, true).unwrap();
    let references = render_typed(
        "referenceBreaks",
        multiple_break_references(),
        2,
        &["I", "I", "I", "Ljava/lang/Object;", "Ljava/lang/Object;"],
        "Ljava/lang/Object;",
    )
    .unwrap();
    let wide = render_typed(
        "wideBreaks",
        multiple_break_wide(),
        3,
        &["I", "I", "I", "J", "J"],
        "J",
    )
    .unwrap();
    let source = format!(
        r#"package sample;
class Hook {{
  static String trace = "";
  static char failStage;
  static int failAt;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  static void mark(char stage, int i) {{
    trace += stage + Integer.toString(i) + ";";
    if (stage == failStage && i == failAt) throw sentinel;
  }}
  static void before(int i) {{ mark('B', i); }}
  static void after(int i) {{ mark('A', i); }}
}}
public class PosttestBreak {{
{method}
{references}
{wide}
  static int checks;
  static void check(boolean okay) {{ checks++; if (!okay) throw new AssertionError(checks); }}
  public static void main(String[] args) {{
    for (int limit : new int[] {{-1, 0, 1, 2, 5}})
      for (int first : new int[] {{-1, 0, 1, 4}})
        for (int second : new int[] {{-1, 0, 1, 4}})
          for (char fail : new char[] {{0, 'B', 'A'}})
            for (int failAt : new int[] {{-1, 0, 1, 4}}) {{
              int expected = 0;
              StringBuilder trace = new StringBuilder();
              boolean throwsNow = false;
              for (int i = 0;; i++) {{
                trace.append('B').append(i).append(';');
                if (fail == 'B' && failAt == i) {{ throwsNow = true; break; }}
                expected += 1;
                if (i == first) break;
                trace.append('A').append(i).append(';');
                if (fail == 'A' && failAt == i) {{ throwsNow = true; break; }}
                expected += 2;
                if (i == second) break;
                expected += 3;
                if (i + 1 >= limit) break;
              }}
              Hook.trace = ""; Hook.failStage = fail; Hook.failAt = failAt;
              try {{ int actual = multipleBreaks(limit, first, second);
                check(!throwsNow && actual == expected); }}
              catch (RuntimeException error) {{ check(throwsNow && error == Hook.sentinel); }}
              check(Hook.trace.equals(trace.toString()));
            }}
    Object left = new Object(), right = new Object();
    for (int limit : new int[] {{-1, 0, 1, 3}})
      for (int first : new int[] {{-1, 0, 1, 2}})
        for (int second : new int[] {{-1, 0, 1, 2}}) {{
          int count = Math.max(1, limit);
          boolean secondBreak = second >= 0 && second < count
              && (first < 0 || first >= count || second < first);
          Object expectedRef = secondBreak ? right : left;
          long expectedWide = secondBreak ? Long.MAX_VALUE : Long.MIN_VALUE;
          check(referenceBreaks(limit, first, second, left, right) == expectedRef);
          check(wideBreaks(limit, first, second, Long.MIN_VALUE, Long.MAX_VALUE) == expectedWide);
        }}
  }}
}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-posttest-multiple-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/PosttestBreak.java"), source).unwrap();
    for (program, argument) in [
        ("javac", "sample/PosttestBreak.java"),
        ("java", "sample.PosttestBreak"),
    ] {
        let output = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            dir.display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_matches_break_and_latch_values() {
    let a = render("beforeEffects", before_effects(), 2, 2).unwrap();
    let b = render("afterDiamond", after_diamond(), 2, 3).unwrap();
    let c = render_with_hooks("observableEffects", observable_effects(), 2, 2, true).unwrap();
    let source = format!(
        r#"package sample;
class Hook {{
  static String trace = "";
  static char failStage;
  static int failAt;
  static final RuntimeException sentinel = new RuntimeException("sentinel");
  static void mark(char stage, int i) {{
    trace += stage + Integer.toString(i) + ";";
    if (stage == failStage && i == failAt) throw sentinel;
  }}
  static void before(int i) {{ mark('B', i); }}
  static void after(int i) {{ mark('A', i); }}
}}
public class PosttestBreak {{
{a}
{b}
{c}
static int checks;
static void check(boolean value) {{ checks++; if (!value) throw new AssertionError(checks); }}
public static void main(String[] args) {{
  for (int limit : new int[] {{-1, 0, 1, 2, 5}})
    for (int selector : new int[] {{0, 1}}) {{
      int n = Math.max(1, limit);
      check(beforeEffects(limit, selector) == (selector == 0 ? 2 : n * 5));
      for (int stop : new int[] {{-1, 0, 1, 4, 9, 50}}) {{
        int step = selector == 0 ? 5 : 3;
        check(afterDiamond(limit, selector, stop) == step * Math.min(n, Math.max(1, (stop + step - 1) / step)));
      }}
      for (char fail : new char[] {{0, 'B', 'A'}})
        for (int at : new int[] {{-1, 0, 1, 4}}) {{
          StringBuilder expected = new StringBuilder();
          boolean throwsNow = false;
          int count = selector == 0 ? 1 : n;
          outer: for (int i = 0; i < count; i++) {{
            expected.append('B').append(i).append(';');
            if (fail == 'B' && at == i) {{ throwsNow = true; break outer; }}
            if (selector == 0) break;
            expected.append('A').append(i).append(';');
            if (fail == 'A' && at == i) {{ throwsNow = true; break outer; }}
          }}
          Hook.trace = ""; Hook.failStage = fail; Hook.failAt = at;
          try {{ int value = observableEffects(limit, selector); check(!throwsNow && value == (selector == 0 ? 0 : 2 * n)); }}
          catch (RuntimeException e) {{ check(throwsNow && e == Hook.sentinel); }}
          check(Hook.trace.equals(expected.toString()));
        }}
    }}
  check(checks == 310);
}}
}}"#
    );
    let dir =
        std::env::temp_dir().join(format!("rdx-shared-posttest-break-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/PosttestBreak.java"), source).unwrap();
    for (program, argument) in [
        ("javac", "sample/PosttestBreak.java"),
        ("java", "sample.PosttestBreak"),
    ] {
        let output = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            dir.display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
