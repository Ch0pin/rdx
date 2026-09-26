//! G01-C-natural-loop: one pretest guard, one backedge, one exit.
//! JVM expected values/traces are independent of production loop/CFG helpers.
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
    fn guard(&mut self, word: u16) {
        let pc = self.words.len();
        self.emit(&[word, 0]);
        self.patches.push((pc, "exit", false));
    }
    fn latch(&mut self) {
        let pc = self.words.len();
        self.emit(&[0x28]);
        self.patches.push((pc, "header", true));
    }
    fn increment(&mut self) {
        self.emit(&[0x00d8, 0x0100]);
    }
    fn finish(mut self) -> Vec<u16> {
        for (pc, target, jump) in self.patches {
            let delta = self.labels[target] as isize - pc as isize;
            if jump {
                assert!((-128..0).contains(&delta));
                self.words[pc] |= (delta as i8 as u8 as u16) << 8;
            } else {
                assert!(delta > 0);
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
    locals: u16,
    words: Vec<u16>,
    hooks: bool,
    types: Vec<&'static str>,
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
    code.registers = case.locals + code.ins;
    code.outs = u16::from(case.hooks);
    code.instructions = case.words.clone();
    class.symbols = if case.hooks {
        Arc::new(DexSymbols {
            strings: ["header", "body", "tail"]
                .into_iter()
                .map(Into::into)
                .collect(),
            types: vec!["Lsample/Hook;".into()],
            protos: vec![
                ("I".into(), vec!["I".into()]),
                ("V".into(), vec!["I".into()]),
            ],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 0, 2)],
            ..Default::default()
        })
    } else {
        Arc::new(DexSymbols {
            types: case.types.iter().copied().map(Into::into).collect(),
            ..Default::default()
        })
    };
    class
}
fn render(case: &Case) -> anyhow::Result<String> {
    let class = fixture(case);
    Ok(native_java::render_method("sample.NaturalLoops", &class, &class.methods[0])?.source)
}
fn plain(
    name: &'static str,
    parameters: Vec<&'static str>,
    result: &'static str,
    locals: u16,
    w: Words,
) -> Case {
    Case {
        name,
        parameters,
        result,
        locals,
        words: w.finish(),
        hooks: false,
        types: vec![],
    }
}
fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("header");
    w.guard(0x2035);
    w.emit(&[0x0190, 0x0001]);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("sum", vec!["I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x3101]);
    w.label("header");
    w.guard(0x2035);
    w.emit(&[0x0190, 0x0401]);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("intCarry", vec!["I", "I", "I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x4104]);
    w.label("header");
    w.guard(0x3035);
    w.emit(&[0x019b, 0x0601]);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x0110]);
    out.push(plain("wideCarry", vec!["I", "J", "J"], "J", 3, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x5107, 0x6207]);
    w.label("header");
    w.guard(0x4035);
    w.emit(&[0x1307, 0x2107, 0x3207]);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x0111]);
    out.push(plain(
        "referenceCarry",
        vec!["I", "Ljava/lang/Object;", "Ljava/lang/Object;"],
        "Ljava/lang/Object;",
        4,
        w,
    ));
    for effects in [false, true] {
        let mut w = Words::default();
        w.emit(&[0x0012]);
        w.label("header");
        w.emit(&[0x1071, 0, 0, 0x010a]);
        w.guard(0x2035);
        if effects {
            w.emit(&[0x1071, 1, 0]);
        }
        w.increment();
        w.latch();
        w.label("exit");
        if effects {
            w.emit(&[0x1071, 2, 1, 0x000a, 0x000f]);
        } else {
            w.emit(&[0x010f]);
        }
        let mut case = plain(
            if effects { "effects" } else { "headerLiveout" },
            vec!["I"],
            "I",
            2,
            w,
        );
        case.hooks = true;
        out.push(case);
    }
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("header");
    w.guard(0x2035);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x011f, 0, 0x0111]);
    let mut case = plain("nullExit", vec!["I"], "Ljava/lang/String;", 2, w);
    case.types = vec!["Ljava/lang/String;"];
    out.push(case);
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0114, 0, 0x8000]);
    w.label("header");
    w.guard(0x2035);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("invariantFloat", vec!["I"], "F", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0118, 0, 0, 0, 0x8000]);
    w.label("header");
    w.guard(0x3035);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x0110]);
    out.push(plain("invariantDouble", vec!["I"], "D", 3, w));
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("header");
    w.guard(0x023d);
    w.emit(&[0x02d8, 0xff02]);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x000f]);
    out.push(plain("countdown", vec!["I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("header");
    w.emit(&[0x0112]);
    w.guard(0x2035);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x011f, 0, 0x0111]);
    let mut case = plain("headerNullExit", vec!["I"], "Ljava/lang/String;", 2, w);
    case.types = vec!["Ljava/lang/String;"];
    out.push(case);
    let mut w = Words::default();
    w.emit(&[0x0014, 0x2345, 0x7fc1, 0x0112]);
    w.label("header");
    w.guard(0x4135);
    w.emit(&[0x0201, 0x0012, 0x2001, 0x01d8, 0x0101]);
    w.latch();
    w.label("exit");
    w.emit(&[0x000f]);
    out.push(plain("restoredFloat", vec!["I"], "F", 4, w));
    out
}
#[test]
fn bounded_pretest_loops_preserve_carried_values_header_exit_definitions_and_links() {
    let cases = cases();
    assert_eq!(cases.len(), 12);
    for case in cases {
        let class = fixture(&case);
        let code = native_java::render_method("sample.NaturalLoops", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert_eq!(
            code.source.matches("while (").count(),
            1,
            "{}: {}",
            case.name,
            code.source
        );
        assert_eq!(
            code.source.matches("return ").count(),
            1,
            "{}: {}",
            case.name,
            code.source
        );
        if case.hooks {
            assert_eq!(
                code.source.matches("Hook.header(").count(),
                1,
                "{}",
                code.source
            );
            assert!(
                code.source.find("while (").unwrap() < code.source.find("Hook.header(").unwrap()
            );
            assert_eq!(
                code.links
                    .iter()
                    .filter(|l| l.label == "sample.Hook.header(I)I")
                    .count(),
                1
            );
            if case.name == "effects" {
                assert_eq!(code.source.matches("Hook.body(").count(), 1);
                assert_eq!(code.source.matches("Hook.tail(").count(), 1);
            }
        }
        if matches!(case.name, "nullExit" | "headerNullExit") {
            let links = code
                .links
                .iter()
                .filter(|l| l.label == "java.lang.String")
                .collect::<Vec<_>>();
            assert!(!links.is_empty());
            for link in links {
                assert_eq!(
                    code.source
                        .chars()
                        .skip(link.start)
                        .take(link.end - link.start)
                        .collect::<String>(),
                    "java.lang.String"
                );
            }
        }
    }
}
#[test]
fn malformed_loop_edges_guard_operands_and_carried_pairs_fail_closed() {
    for target in [1, 99] {
        let mut case = cases().remove(0);
        case.words[3] = target;
        assert!(render(&case).is_err(), "accepted exit target {target}");
    }
    let mut case = cases().remove(0);
    case.words[8] = 0xfb28;
    assert!(render(&case).is_err(), "accepted latch into guard operand");
    let mut case = cases().remove(0);
    case.words[8] = 0x7f28;
    assert!(render(&case).is_err(), "accepted latch out of method");
    let mut case = cases().remove(0);
    case.words[2] = 0xf035;
    assert!(render(&case).is_err(), "accepted out-of-frame guard");
    let mut case = cases().remove(0);
    case.words[0] = 0;
    assert!(render(&case).is_err(), "accepted undefined counter");
    let mut case = cases().remove(0);
    case.parameters[0] = "Ljava/lang/Object;";
    assert!(render(&case).is_err(), "accepted ordered reference guard");
    let mut case = cases().into_iter().find(|c| c.name == "wideCarry").unwrap();
    case.words[5] = 0x0701;
    assert!(render(&case).is_err(), "accepted out-of-frame wide operand");
    let mut case = cases().into_iter().find(|c| c.name == "nullExit").unwrap();
    case.types.clear();
    assert!(render(&case).is_err(), "accepted missing exit cast pool");
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn natural_loops_jvm_match_iterations_carried_identity_literals_and_effect_order() {
    let methods = cases()
        .iter()
        .map(|case| render(case).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let java = format!(
        r#"package sample;
class Hook {{
static String trace = ""; static char failure; static int failAt; static final RuntimeException sentinel = new RuntimeException("sentinel");
static void mark(char stage, int i) {{ trace += "" + stage + i + ";"; if (failure == stage && (stage == 'T' || i == failAt)) throw sentinel; }}
static int header(int i) {{ mark('H', i); return 100+i; }} static void body(int i) {{ mark('B', i); }} static int tail(int last) {{ mark('T', last-100); return last+1000; }}
}}
public class NaturalLoops {{
{methods}
static void checkEffects(int limit, boolean body, char failure, int at) {{
int n = Math.max(0,limit); Hook.trace = ""; Hook.failure = failure; Hook.failAt = at;
boolean fails = failure == 'H' ? at >= 0 && at <= n : failure == 'B' ? body && at >= 0 && at < n : failure == 'T' && body;
StringBuilder expectedTrace = new StringBuilder();
for (int i=0; i<=n; i++) {{
  expectedTrace.append('H').append(i).append(';');
  if (failure == 'H' && i == at) break;
  if (body && i<n) {{ expectedTrace.append('B').append(i).append(';'); if (failure == 'B' && i == at) break; }}
}}
if (body && !(failure == 'H' && at >= 0 && at <= n) && !(failure == 'B' && at >= 0 && at < n)) expectedTrace.append('T').append(n).append(';');
try {{ int actual = body ? effects(limit) : headerLiveout(limit); if (fails || actual != (body ? 1100+n : 100+n)) throw new AssertionError("effect result"); }}
catch (RuntimeException actual) {{ if (!fails || actual != Hook.sentinel) throw new AssertionError("effect exception identity",actual); }}
if (!Hook.trace.equals(expectedTrace.toString())) throw new AssertionError("trace " + Hook.trace + " expected " + expectedTrace);
}}
public static void main(String[] args) {{
Object left = new Object(), right = new Object();
for (int limit : new int[]{{-3,0,1,2,7,64}}) {{
int n = Math.max(0,limit);
if (sum(limit) != n*(n-1)/2 || countdown(limit) != n) throw new AssertionError("iteration counts");
if (nullExit(limit) != null || headerNullExit(limit) != null) throw new AssertionError("invariant/header null exit cast");
if (Float.floatToRawIntBits(restoredFloat(limit)) != 0x7fc12345) throw new AssertionError("restored invariant NaN payload");
if (Float.floatToRawIntBits(invariantFloat(limit)) != 0x80000000) throw new AssertionError("float invariant bits");
if (Double.doubleToRawLongBits(invariantDouble(limit)) != 0x8000000000000000L) throw new AssertionError("double invariant bits");
for (int seed : new int[]{{Integer.MIN_VALUE,-1,0,Integer.MAX_VALUE}}) for (int step : new int[]{{Integer.MIN_VALUE,-1,0,Integer.MAX_VALUE}})
 if (intCarry(limit,seed,step) != (int)((long)seed + (long)n*step)) throw new AssertionError("int carried wrap");
for (long seed : new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}}) for (long step : new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})
 if (wideCarry(limit,seed,step) != seed + (long)n*step) throw new AssertionError("wide carried wrap");
for (Object a : new Object[]{{null,left,right}}) for (Object b : new Object[]{{null,left,right}})
 if (referenceCarry(limit,a,b) != ((n&1)==0 ? a : b)) throw new AssertionError("reference carried swap identity");
for (char failure : new char[]{{0,'H','B','T'}}) for (int at : new int[]{{-1,0,1,6,7,63,64,65}}) {{ checkEffects(limit,false,failure,at); checkEffects(limit,true,failure,at); }}
}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-natural-loop-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/NaturalLoops.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/NaturalLoops.java"),
        ("java", "sample.NaturalLoops"),
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
            dir.join("sample/NaturalLoops.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
