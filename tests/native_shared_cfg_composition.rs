//! G01-C-forward-composition: independent public-renderer/JVM semantic fixtures.
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{collections::BTreeMap, fs, process::Command, sync::Arc};
#[derive(Default)]
struct Words {
    words: Vec<u16>,
    labels: BTreeMap<&'static str, usize>,
    patches: Vec<(usize, &'static str, bool)>,
}
impl Words {
    fn emit(&mut self, words: &[u16]) {
        self.words.extend_from_slice(words);
    }
    fn label(&mut self, name: &'static str) {
        assert!(self.labels.insert(name, self.words.len()).is_none());
    }
    fn branch(&mut self, register: u16, opcode: u16, target: &'static str) {
        let pc = self.words.len();
        self.emit(&[(register << 8) | opcode, 0]);
        self.patches.push((pc, target, false));
    }
    fn jump(&mut self, target: &'static str) {
        let pc = self.words.len();
        self.emit(&[0x28]);
        self.patches.push((pc, target, true));
    }
    fn constant(&mut self, register: u16, value: i8) {
        assert!((-8..=7).contains(&value));
        self.emit(&[((value as u16 & 15) << 12) | (register << 8) | 0x12]);
    }
    fn add(&mut self, value: i8) {
        self.emit(&[0x00d8, (value as u8 as u16) << 8]);
    }
    fn call(&mut self, method: u16, destination: Option<u16>) {
        self.emit(&[0x0071, method, 0]);
        if let Some(dst) = destination {
            self.emit(&[(dst << 8) | 0x0a]);
        }
    }
    fn finish(mut self) -> Vec<u16> {
        for (pc, target, jump) in self.patches {
            let delta = self.labels[target] as isize - pc as isize;
            assert!(delta > 0);
            if jump {
                assert!(delta <= 127);
                self.words[pc] |= (delta as u16) << 8;
            } else {
                self.words[pc + 1] = delta as u16;
            }
        }
        self.words
    }
}
struct Case {
    name: &'static str,
    parameters: Vec<&'static str>,
    result: &'static str,
    words: Vec<u16>,
    hooks: bool,
}
fn width(ty: &str) -> u16 {
    if matches!(ty, "J" | "D") { 2 } else { 1 }
}
fn fixture(case: &Case) -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    let m = &mut class.methods[0];
    m.name = case.name.into();
    m.access_flags = 9;
    m.parameters = case.parameters.iter().copied().map(Into::into).collect();
    m.return_type = case.result.into();
    let code = m.code.as_mut().unwrap();
    code.ins = case.parameters.iter().map(|ty| width(ty)).sum();
    code.registers = 2 + code.ins;
    code.outs = if case.hooks { 2 } else { 0 };
    code.instructions = case.words.clone();
    if case.hooks {
        class.symbols = Arc::new(DexSymbols {
            strings: ["entry", "a", "b", "c", "d", "combine", "setup", "tail"]
                .into_iter()
                .map(Into::into)
                .collect(),
            types: vec!["Lsample/Hook;".into()],
            protos: vec![
                ("V".into(), vec![]),
                ("I".into(), vec![]),
                ("I".into(), vec!["I".into(), "I".into()]),
                ("I".into(), vec!["I".into()]),
            ],
            methods: vec![
                (0, 0, 0),
                (0, 1, 1),
                (0, 1, 2),
                (0, 1, 3),
                (0, 1, 4),
                (0, 2, 5),
                (0, 0, 6),
                (0, 3, 7),
            ],
            ..Default::default()
        });
    }
    class
}
fn render(case: &Case) -> anyhow::Result<String> {
    let class = fixture(case);
    Ok(native_java::render_method("sample.Composition", &class, &class.methods[0])?.source)
}
fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let mut w = Words::default();
    w.branch(2, 0x38, "firstTrue");
    w.constant(0, 1);
    w.jump("firstJoin");
    w.label("firstTrue");
    w.constant(0, 2);
    w.label("firstJoin");
    w.branch(3, 0x38, "secondTrue");
    w.add(3);
    w.jump("end");
    w.label("secondTrue");
    w.add(5);
    w.label("end");
    w.emit(&[0x000f]);
    cases.push(Case {
        name: "siblings",
        parameters: vec!["I", "I"],
        result: "I",
        words: w.finish(),
        hooks: false,
    });
    let mut w = Words::default();
    w.branch(2, 0x38, "outerTrue");
    w.branch(3, 0x38, "leftTrue");
    w.constant(0, 1);
    w.jump("leftJoin");
    w.label("leftTrue");
    w.constant(0, 3);
    w.label("leftJoin");
    w.jump("end");
    w.label("outerTrue");
    w.branch(3, 0x3a, "rightTrue");
    w.constant(0, 7);
    w.jump("end");
    w.label("rightTrue");
    w.constant(0, 5);
    w.label("end");
    w.emit(&[0x000f]);
    cases.push(Case {
        name: "nested",
        parameters: vec!["I", "I"],
        result: "I",
        words: w.finish(),
        hooks: false,
    });
    let mut w = Words::default();
    w.branch(2, 0x3b, "second");
    w.constant(0, -1);
    w.emit(&[0x000f]);
    w.label("second");
    w.branch(3, 0x39, "third");
    w.constant(0, 3);
    w.emit(&[0x000f]);
    w.label("third");
    w.constant(0, 7);
    w.emit(&[0x000f]);
    cases.push(Case {
        name: "guards",
        parameters: vec!["I", "I"],
        result: "I",
        words: w.finish(),
        hooks: false,
    });
    let mut w = Words::default();
    w.branch(2, 0x38, "setup");
    w.branch(3, 0x39, "tail");
    w.label("setup");
    w.call(6, None);
    w.label("tail");
    w.emit(&[0x1071, 7, 2, 0x000a, 0x000f]);
    cases.push(Case {
        name: "postdomBypass",
        parameters: vec!["I", "I"],
        result: "I",
        words: w.finish(),
        hooks: true,
    });
    let mut w = Words::default();
    w.call(0, None);
    w.branch(2, 0x38, "firstTrue");
    w.call(1, Some(0));
    w.jump("firstJoin");
    w.label("firstTrue");
    w.call(2, Some(0));
    w.label("firstJoin");
    w.branch(3, 0x38, "secondTrue");
    w.call(3, Some(1));
    w.jump("end");
    w.label("secondTrue");
    w.call(4, Some(1));
    w.label("end");
    w.emit(&[0x2071, 5, 0x0010, 0x000a, 0x000f]);
    cases.push(Case {
        name: "siblingEffects",
        parameters: vec!["I", "I"],
        result: "I",
        words: w.finish(),
        hooks: true,
    });
    let mut w = Words::default();
    w.call(0, None);
    w.branch(2, 0x38, "outerTrue");
    w.branch(3, 0x38, "leftTrue");
    w.call(1, Some(0));
    w.jump("end");
    w.label("leftTrue");
    w.call(2, Some(0));
    w.jump("end");
    w.label("outerTrue");
    w.branch(3, 0x3a, "rightTrue");
    w.call(4, Some(0));
    w.jump("end");
    w.label("rightTrue");
    w.call(3, Some(0));
    w.label("end");
    w.constant(1, 7);
    w.emit(&[0x2071, 5, 0x0010, 0x000a, 0x000f]);
    cases.push(Case {
        name: "nestedEffects",
        parameters: vec!["I", "I"],
        result: "I",
        words: w.finish(),
        hooks: true,
    });
    for (name, ty, left, right, move_op, return_op) in [
        ("wideLiveout", "J", 4, 6, 0x04, 0x10),
        ("referenceLiveout", "Ljava/lang/Object;", 4, 5, 0x07, 0x11),
    ] {
        let mut w = Words::default();
        w.branch(2, 0x38, "firstTrue");
        w.emit(&[(right << 12) | move_op]);
        w.jump("firstJoin");
        w.label("firstTrue");
        w.emit(&[(left << 12) | move_op]);
        w.label("firstJoin");
        w.branch(3, 0x38, "end");
        w.emit(&[(left << 12) | move_op]);
        w.label("end");
        w.emit(&[return_op]);
        cases.push(Case {
            name,
            parameters: vec!["I", "I", ty, ty],
            result: ty,
            words: w.finish(),
            hooks: false,
        });
    }
    let mut w = Words::default();
    w.branch(2, 0x38, "firstTrue");
    w.constant(0, -7);
    w.constant(1, 1);
    w.jump("firstJoin");
    w.label("firstTrue");
    w.constant(0, 5);
    w.emit(&[0x4107]);
    w.label("firstJoin");
    w.branch(3, 0x38, "secondTrue");
    w.add(5);
    w.jump("end");
    w.label("secondTrue");
    w.add(3);
    w.label("end");
    w.emit(&[0x000f]);
    cases.push(Case {
        name: "deadLive",
        parameters: vec!["I", "I", "Ljava/lang/Object;"],
        result: "I",
        words: w.finish(),
        hooks: false,
    });
    let mut w = Words::default();
    w.branch(2, 0x38, "firstTrue");
    w.constant(0, -1);
    w.jump("firstJoin");
    w.label("firstTrue");
    w.constant(0, 0);
    w.label("firstJoin");
    w.emit(&[0x0301]);
    w.branch(3, 0x38, "secondTrue");
    w.constant(0, 3);
    w.jump("end");
    w.label("secondTrue");
    w.constant(0, 7);
    w.label("end");
    w.emit(&[0x000f]);
    cases.push(Case {
        name: "lateCondition",
        parameters: vec!["I", "I"],
        result: "I",
        words: w.finish(),
        hooks: false,
    });
    for terminal_throw in [true, false] {
        let mut w = Words::default();
        w.branch(2, 0x38, "outerTrue");
        w.branch(3, 0x39, "continue");
        if terminal_throw {
            w.emit(&[0x0427]);
        } else {
            w.constant(0, -5);
            w.emit(&[0x000f]);
        }
        w.label("continue");
        w.constant(0, 3);
        w.jump("end");
        w.label("outerTrue");
        w.constant(0, 7);
        w.label("end");
        w.add(11);
        w.emit(&[0x000f]);
        cases.push(Case {
            name: if terminal_throw {
                "nestedThrow"
            } else {
                "nestedReturn"
            },
            parameters: vec!["I", "I", "Ljava/lang/RuntimeException;"],
            result: "I",
            words: w.finish(),
            hooks: false,
        });
    }
    cases
}
#[test]
fn composed_forward_regions_preserve_single_shared_tails_and_navigation() {
    let cases = cases();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let class = fixture(&case);
        let code = native_java::render_method("sample.Composition", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert!(
            code.source.matches("if (").count() >= 2,
            "{}: {}",
            case.name,
            code.source
        );
        if matches!(case.name, "siblingEffects" | "nestedEffects") {
            for method in ["entry", "a", "b", "c", "d", "combine"] {
                assert_eq!(
                    code.source.matches(&format!("Hook.{method}(")).count(),
                    1,
                    "{} duplicated effect: {}",
                    case.name,
                    code.source
                );
            }
            assert_eq!(
                code.links
                    .iter()
                    .filter(|l| l.label == "sample.Hook.combine(II)I")
                    .count(),
                1
            );
        }
        if case.name == "postdomBypass" {
            // This shared setup may appear in mutually exclusive Java arms;
            // the JVM trace below proves it executes exactly once when selected.
            assert!((1..=2).contains(&code.source.matches("Hook.setup()").count()));
            assert_eq!(code.source.matches("Hook.tail(").count(), 1);
        }
    }
}
#[test]
fn malformed_composed_edges_operands_and_liveouts_fail_closed() {
    for offset in [1, 0, 999] {
        let mut case = cases().remove(0);
        case.words[1] = offset;
        assert!(render(&case).is_err(), "accepted outer offset {offset}");
    }
    let mut case = cases().remove(0);
    case.words[6] = 1;
    assert!(
        render(&case).is_err(),
        "accepted second branch into operand"
    );
    let mut case = cases().remove(0);
    case.words[5] = 0xff38;
    assert!(render(&case).is_err(), "accepted second branch register");
    let mut case = cases().remove(0);
    case.words.truncate(6);
    assert!(render(&case).is_err(), "accepted truncated second branch");
    let mut case = cases().remove(0);
    case.parameters[1] = "Ljava/lang/Object;";
    case.words[2] = 0x3012;
    case.words[4] = 0x3007;
    assert!(
        render(&case).is_err(),
        "accepted reference/integer live merge"
    );
    let mut case = cases().remove(0);
    case.words[2] = 0x1112;
    assert!(render(&case).is_err(), "accepted undefined first liveout");
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn composition_jvm_matches_independent_regions_values_traces_and_failures() {
    let methods = cases()
        .iter()
        .map(|case| render(case).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let java = format!(
        r#"package sample;
class Hook {{
static String trace = ""; static char fail; static final RuntimeException sentinel = new RuntimeException("sentinel");
static void mark(char stage) {{ trace += stage; if (stage == fail) throw sentinel; }}
static void entry() {{ mark('E'); }} static int a() {{ mark('A'); return 11; }} static int b() {{ mark('B'); return 22; }}
static int c() {{ mark('C'); return 33; }} static int d() {{ mark('D'); return 44; }} static int combine(int a, int b) {{ mark('J'); return a+b; }}
static void setup() {{ mark('S'); }} static int tail(int x) {{ mark('T'); return x*3; }}
}}
public class Composition {{
{methods}
static void traceResult(int x, int y, boolean nested, char failure) {{
Hook.trace = ""; Hook.fail = failure;
String stages; int expected;
if (nested) {{ char selected = x == 0 ? (y < 0 ? 'C' : 'D') : (y == 0 ? 'B' : 'A'); stages = "E" + selected + "J"; expected = (selected == 'A' ? 11 : selected == 'B' ? 22 : selected == 'C' ? 33 : 44) + 7; }}
else {{ char first = x == 0 ? 'B' : 'A', second = y == 0 ? 'D' : 'C'; stages = "E" + first + second + "J"; expected = (x == 0 ? 22 : 11) + (y == 0 ? 44 : 33); }}
int failedAt = stages.indexOf(failure); boolean fails = failure != 0 && failedAt >= 0;
String expectedTrace = fails ? stages.substring(0, failedAt+1) : stages;
try {{ int actual = nested ? nestedEffects(x,y) : siblingEffects(x,y); if (fails || actual != expected) throw new AssertionError("effect result"); }}
catch (RuntimeException actual) {{ if (!fails || actual != Hook.sentinel) throw new AssertionError("effect identity", actual); }}
if (!Hook.trace.equals(expectedTrace)) throw new AssertionError("effect trace " + Hook.trace + " expected " + expectedTrace);
}}
public static void main(String[] args) {{
Object left = new Object(), right = new Object(); RuntimeException terminal = new RuntimeException("terminal");
for (int x : new int[]{{Integer.MIN_VALUE,-1,0,1,Integer.MAX_VALUE}}) for (int y : new int[]{{Integer.MIN_VALUE,-1,0,1,Integer.MAX_VALUE}}) {{
if (siblings(x,y) != (x == 0 ? 2 : 1) + (y == 0 ? 5 : 3)) throw new AssertionError("siblings");
if (nested(x,y) != (x == 0 ? (y < 0 ? 5 : 7) : (y == 0 ? 3 : 1))) throw new AssertionError("nested");
if (guards(x,y) != (x < 0 ? -1 : y == 0 ? 3 : 7)) throw new AssertionError("guards");
if (deadLive(x,y,left) != (x == 0 ? 5 : -7) + (y == 0 ? 3 : 5)) throw new AssertionError("dead/live");
if (lateCondition(x,y) != (x == 0 ? 7 : 3)) throw new AssertionError("late overwritten condition");
for (Object a : new Object[]{{null,left,right}}) for (Object b : new Object[]{{null,left,right}})
 if (referenceLiveout(x,y,a,b) != (y == 0 ? (x == 0 ? a : b) : a)) throw new AssertionError("reference liveout");
for (long a : new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}}) for (long b : new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})
 if (wideLiveout(x,y,a,b) != (y == 0 ? (x == 0 ? a : b) : a)) throw new AssertionError("wide liveout");
if (nestedReturn(x,y,terminal) != (x == 0 ? 18 : y != 0 ? 14 : -5)) throw new AssertionError("nested return");
try {{ int actual = nestedThrow(x,y,terminal); if ((x != 0 && y == 0) || actual != (x == 0 ? 18 : 14)) throw new AssertionError("nested throw result"); }}
catch (RuntimeException actual) {{ if (x == 0 || y != 0 || actual != terminal) throw new AssertionError("nested throw identity", actual); }}
for (char failure : new char[]{{0,'E','A','B','C','D','J'}}) {{ traceResult(x,y,false,failure); traceResult(x,y,true,failure); }}
for (char failure : new char[]{{0,'S','T'}}) {{
Hook.trace = ""; Hook.fail = failure; String stages = (x == 0 || y == 0) ? "ST" : "T";
int failedAt = stages.indexOf(failure); boolean fails = failure != 0 && failedAt >= 0;
try {{ int actual = postdomBypass(x,y); if (fails || actual != x*3) throw new AssertionError("bypass result"); }}
catch (RuntimeException actual) {{ if (!fails || actual != Hook.sentinel) throw new AssertionError("bypass exception identity", actual); }}
String expectedTrace = fails ? stages.substring(0,failedAt+1) : stages;
if (!Hook.trace.equals(expectedTrace)) throw new AssertionError("bypass trace " + Hook.trace + " expected " + expectedTrace);
}}
}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-cfg-composition-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Composition.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/Composition.java"),
        ("java", "sample.Composition"),
    ] {
        let output = Command::new(program)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\nSource: {}",
            String::from_utf8_lossy(&output.stderr),
            dir.join("sample/Composition.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
