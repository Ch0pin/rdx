//! G01-C-dual-conditional-backedges: two canonical branches reenter one header.
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
fn fixture() -> Vec<u16> {
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]); // index = 0; sum = 0
    w.label("body");
    w.emit(&[0x1071, 0, 0, 0x01d8, 0x0101]); // before(index); sum += 1
    w.branch(0x2032, "exit"); // first common-exit guard
    w.emit(&[0x1071, 1, 0, 0x01d8, 0x0201]); // middle(index); sum += 2
    w.branch(0x3032, "exit"); // second common-exit guard
    w.emit(&[0x1071, 2, 0, 0x01d8, 0x0301]); // late(index); sum += 3
    w.branch(0x4032, "terminal"); // separate terminal tail
    w.emit(&[0x00d8, 0x0100]); // index++
    w.branch(0x5034, "body"); // first conditional backedge
    w.emit(&[0x1071, 3, 0, 0x01d8, 0x0401]); // after(index); sum += 4
    w.branch(0x6034, "body"); // second conditional backedge
    w.label("terminal");
    w.emit(&[0x010f]);
    w.label("exit");
    w.emit(&[0x1071, 4, 0, 0x010f]); // exit(index); return sum
    w.finish()
}
fn render(words: Vec<u16>) -> anyhow::Result<String> {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))?
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let method = &mut class.methods[0];
    method.name = "run".into();
    method.access_flags = 9;
    method.parameters = (0..5).map(|_| "I".into()).collect();
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 7;
    code.ins = 5;
    code.outs = 1;
    code.instructions = words;
    class.symbols = Arc::new(DexSymbols {
        strings: ["before", "middle", "late", "after", "exit"]
            .map(Into::into)
            .to_vec(),
        types: vec!["Lsample/Hook;".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: (0..5).map(|n| (0, 0, n)).collect(),
        ..Default::default()
    });
    Ok(native_java::render_method("sample.DualBackedges", &class, &class.methods[0])?.source)
}
#[test]
fn two_backedges_keep_breaks_and_terminal_tail_distinct() {
    let source = render(fixture()).unwrap();
    assert_eq!(source.matches("while (").count(), 1, "{source}");
    assert_eq!(source.matches("break;").count(), 2, "{source}");
    assert_eq!(source.matches("continue;").count(), 2, "{source}");
    for name in ["before", "middle", "late", "after", "exit"] {
        assert_eq!(
            source.matches(&format!("Hook.{name}(")).count(),
            1,
            "{source}"
        );
    }
}
#[test]
fn undefined_loop_header_value_fails_closed() {
    let mut words = fixture();
    words[0] = 0; // index is read by Hook.before and both backedges before assignment
    assert!(render(words).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_preserves_call_order_and_exception_identity() {
    let method = render(fixture()).unwrap();
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
  static void middle(int i) {{ mark('M', i); }}
  static void late(int i) {{ mark('L', i); }}
  static void after(int i) {{ mark('A', i); }}
  static void exit(int i) {{ mark('E', i); }}
}}
public class DualBackedges {{
{method}
  static void check(boolean okay) {{ if (!okay) throw new AssertionError(); }}
  public static void main(String[] args) {{
    for (int first : new int[] {{-1,0,1,3}})
      for (int second : new int[] {{-1,0,1,3}})
        for (int terminal : new int[] {{-1,0,1,3}})
          for (int early : new int[] {{0,1,2}})
            for (int finalLimit : new int[] {{1,2,4}})
              for (char fail : new char[] {{0,'B','M','L','A','E'}}) {{
                int sum = 0, i = 0; String trace = ""; boolean threw = false;
                for (int budget = 0; budget < 8; budget++) {{
                  trace += "B" + i + ";"; if (fail == 'B' && i == 1) {{ threw = true; break; }}
                  sum += 1;
                  if (i == first) {{ trace += "E" + i + ";"; if (fail == 'E' && i == 1) threw = true; break; }}
                  trace += "M" + i + ";"; if (fail == 'M' && i == 1) {{ threw = true; break; }}
                  sum += 2;
                  if (i == second) {{ trace += "E" + i + ";"; if (fail == 'E' && i == 1) threw = true; break; }}
                  trace += "L" + i + ";"; if (fail == 'L' && i == 1) {{ threw = true; break; }}
                  sum += 3;
                  if (i == terminal) break;
                  i++;
                  if (i < early) continue;
                  trace += "A" + i + ";"; if (fail == 'A' && i == 1) {{ threw = true; break; }}
                  sum += 4;
                  if (i >= finalLimit) break;
                }}
                Hook.trace = ""; Hook.failStage = fail; Hook.failAt = 1;
                try {{ int got = run(first,second,terminal,early,finalLimit); check(!threw && got == sum); }}
                catch (RuntimeException ex) {{ check(threw && ex == Hook.sentinel); }}
                check(Hook.trace.equals(trace));
              }}
  }}
}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-dual-backedges-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/DualBackedges.java"), source).unwrap();
    for (program, argument) in [
        ("javac", "sample/DualBackedges.java"),
        ("java", "sample.DualBackedges"),
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
