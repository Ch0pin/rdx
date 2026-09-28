//! G01-C-pretest-terminal-exits: a guard default return and body/latch result return.
use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols},
    native_java,
};
use std::{collections::BTreeMap, fs, process::Command, sync::Arc};

#[derive(Default)]
struct Words {
    words: Vec<u16>,
    labels: BTreeMap<&'static str, usize>,
    patches: Vec<(usize, &'static str)>,
}
impl Words {
    fn emit(&mut self, words: &[u16]) {
        self.words.extend_from_slice(words);
    }
    fn label(&mut self, label: &'static str) {
        assert!(self.labels.insert(label, self.words.len()).is_none());
    }
    fn branch(&mut self, opcode: u16, target: &'static str) {
        let pc = self.words.len();
        self.emit(&[opcode, 0]);
        self.patches.push((pc, target));
    }
    fn finish(mut self) -> Vec<u16> {
        for (pc, label) in self.patches {
            self.words[pc + 1] = (self.labels[label] as isize - pc as isize) as i16 as u16;
        }
        self.words
    }
}
fn words() -> Vec<u16> {
    let mut w = Words::default();
    w.emit(&[0x0012, 0xf112]); // index=0, default=-1
    w.label("header");
    w.emit(&[0x1071, 0, 0]); // Hook.header(index)
    w.branch(0x2035, "default"); // if index >= limit
    w.emit(&[0x1071, 1, 0]); // Hook.body(index)
    w.branch(0x3032, "selected"); // if index == selector
    w.emit(&[0x00d8, 0x0100]); // index++
    w.branch(0x2034, "header"); // if index < limit
    w.label("selected");
    w.emit(&[0x000f]);
    w.label("default");
    w.emit(&[0x010f]);
    w.finish()
}
fn words_with_two_prefix_returns() -> Vec<u16> {
    // Two closed early returns precede the conditional-latch loop. This
    // exceeds the shared terminal prefix budget but is valid legacy input.
    let mut w = Words::default();
    w.branch(0x023b, "second"); // if limit >= 0
    w.emit(&[0x020f]); // negative limit
    w.label("second");
    w.branch(0x033b, "init"); // if selector >= 0
    w.emit(&[0x030f]); // negative selector
    w.label("init");
    w.emit(&[0x0012, 0xf112]); // index=0, default=-1
    w.label("header");
    w.emit(&[0x1071, 0, 0]);
    w.branch(0x2035, "default");
    w.emit(&[0x1071, 1, 0]);
    w.emit(&[0x00d8, 0x0100]);
    w.branch(0x2034, "header");
    w.label("selected");
    w.emit(&[0x000f]);
    w.label("default");
    w.emit(&[0x010f]);
    w.finish()
}
fn fixture(words: Vec<u16>) -> DexClass {
    DexClass {
        descriptor: "Lsample/TerminalLoop;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["header".into(), "body".into()],
            types: vec!["Lsample/Hook;".into()],
            protos: vec![("V".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0), (0, 0, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/TerminalLoop;".into(),
            name: "choose".into(),
            return_type: "I".into(),
            parameters: vec!["I".into(), "I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 4,
                ins: 2,
                outs: 1,
                tries: 0,
                try_regions: vec![],
                instructions: words,
                offset: 0,
            }),
        }],
    }
}
fn render(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.TerminalLoop", class, &class.methods[0])?.source)
}
#[test]
fn retains_distinct_default_and_selected_terminal_tails() {
    let source = render(&fixture(words())).unwrap();
    assert_eq!(source.matches("while (").count(), 1, "{source}");
    assert_eq!(source.matches("Hook.header(").count(), 1, "{source}");
    assert_eq!(source.matches("Hook.body(").count(), 1, "{source}");
    assert!(source.matches("return ").count() >= 2, "{source}");
}
#[test]
fn rejects_nonterminal_tail_and_bad_targets() {
    let mut class = fixture(words());
    let code = &mut class.methods[0].code.as_mut().unwrap().instructions;
    let end = code.len();
    code[end - 1] = 0x0000;
    assert!(render(&class).is_err());

    let mut class = fixture(words());
    let code = &mut class.methods[0].code.as_mut().unwrap().instructions;
    let guard = code.iter().position(|word| *word == 0x2035).unwrap();
    code[guard + 1] -= 1; // redirect default exit into selected return
    assert!(render(&class).is_err());
}
#[test]
fn keeps_multiple_closed_prefix_returns_on_existing_route() {
    let source = render(&fixture(words_with_two_prefix_returns())).unwrap();
    assert!(source.contains("Hook.header("), "{source}");
    assert!(source.contains("Hook.body("), "{source}");
    assert!(source.matches("return ").count() >= 4, "{source}");

    let mut malformed = words_with_two_prefix_returns();
    malformed[1] = 4; // Prefix edge skips an instruction boundary/closed arm.
    assert!(render(&fixture(malformed)).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_preserves_zero_iteration_effect_order_and_exception_identity() {
    let method = render(&fixture(words())).unwrap();
    let java = format!(
        r#"package sample;
class Hook {{
  static String trace = "";
  static char failStage;
  static int failAt;
  static final RuntimeException SENTINEL = new RuntimeException("sentinel");
  static void mark(char stage, int value) {{
    trace += stage + Integer.toString(value) + ";";
    if (failStage == stage && failAt == value) throw SENTINEL;
  }}
  static void header(int value) {{ mark('H', value); }}
  static void body(int value) {{ mark('B', value); }}
}}
public class TerminalLoop {{
{method}
  static int checks;
  static void check(boolean okay) {{ checks++; if (!okay) throw new AssertionError(checks); }}
  public static void main(String[] args) {{
    for (int limit : new int[] {{-2, 0, 1, 2, 5}})
      for (int selector : new int[] {{-1, 0, 1, 4}})
        for (char fail : new char[] {{0, 'H', 'B'}})
          for (int at : new int[] {{-1, 0, 1, 4}}) {{
            StringBuilder expected = new StringBuilder();
            boolean throwsNow = false;
            int value = -1;
            int index = 0;
            while (true) {{
              expected.append('H').append(index).append(';');
              if (fail == 'H' && at == index) {{ throwsNow = true; break; }}
              if (index >= limit) break;
              expected.append('B').append(index).append(';');
              if (fail == 'B' && at == index) {{ throwsNow = true; break; }}
              if (index == selector) {{ value = index; break; }}
              index++;
              if (index >= limit) {{ value = index; break; }}
            }}
            Hook.trace = ""; Hook.failStage = fail; Hook.failAt = at;
            try {{ int actual = choose(limit, selector); check(!throwsNow && actual == value); }}
            catch (RuntimeException error) {{ check(throwsNow && error == Hook.SENTINEL); }}
            check(Hook.trace.equals(expected.toString()));
          }}
    check(checks == 480);
  }}
}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-pretest-terminal-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/TerminalLoop.java"), java).unwrap();
    for (program, arg) in [
        ("javac", "sample/TerminalLoop.java"),
        ("java", "sample.TerminalLoop"),
    ] {
        let output = Command::new(program)
            .arg(arg)
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
