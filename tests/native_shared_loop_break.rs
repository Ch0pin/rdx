//! G01-C-loop-break: one body edge to the common natural-loop exit.
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
    fn edge(&mut self, word: u16, target: &'static str) {
        let pc = self.words.len();
        self.emit(&[word, 0]);
        self.patches.push((pc, target, false));
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
    fn add_sum(&mut self, value: u16) {
        self.emit(&[0x01d8, (value << 8) | 1]);
    }
    fn stage(&mut self, method: u16) {
        self.emit(&[0x1071, method, 0, 0x030a]);
    }
    fn accumulate(&mut self) {
        self.emit(&[0x0190, 0x0301]);
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
                assert!((-128..128).contains(&delta) && delta != 0);
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
    code.outs = if case.hooks { 2 } else { 0 };
    code.instructions = case.words.clone();
    class.symbols = if case.hooks {
        Arc::new(DexSymbols {
            strings: ["header", "a", "b", "c", "d", "tail"]
                .into_iter()
                .map(Into::into)
                .collect(),
            types: vec!["Lsample/Hook;".into()],
            protos: vec![
                ("I".into(), vec!["I".into()]),
                ("I".into(), vec!["I".into(), "I".into()]),
            ],
            methods: vec![
                (0, 0, 0),
                (0, 0, 1),
                (0, 0, 2),
                (0, 0, 3),
                (0, 0, 4),
                (0, 1, 5),
            ],
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
    Ok(native_java::render_method("sample.LoopBreak", &class, &class.methods[0])?.source)
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
    w.add_sum(3);
    w.edge(0x3032, "exit");
    w.add_sum(5);
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("beforeAfter", vec!["I", "I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("header");
    w.emit(&[0x1071, 0, 0, 0x010a]);
    w.guard(0x2035);
    w.edge(0x3032, "exit");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    let mut case = plain("headerValue", vec!["I", "I"], "I", 2, w);
    case.hooks = true;
    out.push(case);
    let mut w = Words::default();
    w.emit(&[0x0012, 0x9104, 0xb304]);
    w.label("header");
    w.guard(0x7035);
    w.emit(&[0x1504, 0x3104, 0x5304]);
    w.edge(0x8032, "exit");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x0110]);
    out.push(plain("wideSwap", vec!["I", "I", "J", "J"], "J", 7, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x6107, 0x7207]);
    w.label("header");
    w.guard(0x4035);
    w.emit(&[0x1307, 0x2107, 0x3207]);
    w.edge(0x5032, "exit");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x0111]);
    out.push(plain(
        "referenceSwap",
        vec!["I", "I", "Ljava/lang/Object;", "Ljava/lang/Object;"],
        "Ljava/lang/Object;",
        4,
        w,
    ));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("header");
    w.guard(0x2035);
    w.branch(4, 0x38, "breakingArm");
    w.add_sum(8);
    w.jump("bodyJoin");
    w.label("breakingArm");
    w.add_sum(3);
    w.edge(0x3032, "exit");
    w.add_sum(5);
    w.label("bodyJoin");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("nestedBreak", vec!["I", "I", "I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("header");
    w.emit(&[0x1071, 0, 0, 0x020a]);
    w.guard(0x4035);
    w.branch(6, 0x38, "breakingArm");
    w.stage(2);
    w.accumulate();
    w.stage(4);
    w.accumulate();
    w.jump("bodyJoin");
    w.label("breakingArm");
    w.stage(1);
    w.accumulate();
    w.edge(0x5032, "exit");
    w.stage(3);
    w.accumulate();
    w.label("bodyJoin");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x2071, 5, 0x0021, 0x000a, 0x000f]);
    let mut case = plain("effects", vec!["I", "I", "I"], "I", 4, w);
    case.hooks = true;
    out.push(case);
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("header");
    w.emit(&[0x0112]);
    w.guard(0x2035);
    w.edge(0x3032, "exit");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x011f, 0, 0x0111]);
    let mut case = plain("headerNullExit", vec!["I", "I"], "Ljava/lang/String;", 2, w);
    case.types = vec!["Ljava/lang/String;"];
    out.push(case);
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0114, 0x2345, 0x7fc1]);
    w.label("header");
    w.guard(0x4035);
    w.emit(&[0x1201, 0x0112, 0x2101]);
    w.edge(0x5032, "exit");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("restoredFloat", vec!["I", "I"], "F", 4, w));
    out
}
#[test]
fn conditional_breaks_preserve_changed_exit_values_header_state_and_links() {
    let cases = cases();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let class = fixture(&case);
        let code = native_java::render_method("sample.LoopBreak", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert_eq!(
            code.source.matches("while (").count(),
            1,
            "{}: {}",
            case.name,
            code.source
        );
        assert!(
            code.source.matches("break;").count() >= 2,
            "{} omitted an exit: {}",
            case.name,
            code.source
        );
        if case.name == "effects" {
            for method in ["header", "a", "b", "c", "d", "tail"] {
                assert_eq!(
                    code.source.matches(&format!("Hook.{method}(")).count(),
                    1,
                    "{}",
                    code.source
                );
            }
            assert_eq!(
                code.links
                    .iter()
                    .filter(|l| l.label == "sample.Hook.tail(II)I")
                    .count(),
                1
            );
        }
        if case.name == "headerNullExit" {
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
fn malformed_break_edges_and_exit_operands_fail_closed() {
    let base = || cases().remove(0);
    for offset in [1, 999] {
        let mut case = base();
        let pc = case.words.iter().position(|word| *word == 0x3032).unwrap();
        case.words[pc + 1] = offset;
        assert!(render(&case).is_err(), "accepted break offset {offset}");
    }
    let mut case = base();
    let pc = case.words.iter().position(|word| *word == 0x3032).unwrap();
    case.words[pc] = 0xf032;
    assert!(
        render(&case).is_err(),
        "accepted out-of-frame break operand"
    );
    let mut case = base();
    let pc = case.words.iter().position(|word| *word == 0x3032).unwrap();
    case.words.truncate(pc + 1);
    assert!(render(&case).is_err(), "accepted truncated break");
    let mut case = base();
    case.parameters[1] = "Ljava/lang/Object;";
    let pc = case.words.iter().position(|word| *word == 0x3032).unwrap();
    case.words[pc] = 0x3034;
    assert!(render(&case).is_err(), "accepted ordered reference break");
    let mut case = base();
    case.words[1] = 0;
    assert!(
        render(&case).is_err(),
        "accepted undefined zero-iteration exit value"
    );
    let mut case = cases()
        .into_iter()
        .find(|c| c.name == "headerNullExit")
        .unwrap();
    case.types.clear();
    assert!(render(&case).is_err(), "accepted missing exit cast pool");
    let mut case = cases().into_iter().find(|c| c.name == "wideSwap").unwrap();
    let pc = case.words.iter().position(|word| *word == 0x1504).unwrap();
    case.words[pc] = 0xc504;
    assert!(render(&case).is_err(), "accepted out-of-frame wide source");
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn loop_break_jvm_matches_early_normal_exits_identity_and_exact_effect_prefixes() {
    let methods = cases()
        .iter()
        .map(|c| render(c).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let java = format!(
        r#"package sample;
class Hook{{
static String trace="";static char failure;static int failAt;static final RuntimeException sentinel=new RuntimeException("sentinel");
static void mark(char stage,int i){{trace+=""+stage+i+";";if(stage==failure&&(stage=='T'||i==failAt))throw sentinel;}}
static int header(int i){{mark('H',i);return 100+i;}}
static int a(int i){{mark('A',i);return 3;}}static int b(int i){{mark('B',i);return 1;}}
static int c(int i){{mark('C',i);return 5;}}static int d(int i){{mark('D',i);return 7;}}
static int tail(int sum,int header){{mark('T',header-100);return sum+header+1000;}}
}}
public class LoopBreak{{
{methods}
static void checkEffects(int limit,int stop,int mode,char failure,int at){{
int n=Math.max(0,limit);Hook.trace="";Hook.failure=failure;Hook.failAt=at;
StringBuilder expected=new StringBuilder();boolean fails=false;int sum=0,last=0;
for(int i=0;i<=n;i++){{
 last=i;expected.append('H').append(i).append(';');if(failure=='H'&&i==at){{fails=true;break;}}
 if(i==n){{expected.append('T').append(i).append(';');fails=failure=='T';break;}}
 char before=mode==0?'A':'B';expected.append(before).append(i).append(';');if(failure==before&&i==at){{fails=true;break;}}sum+=mode==0?3:1;
 if(mode==0&&i==stop){{expected.append('T').append(i).append(';');fails=failure=='T';break;}}
 char after=mode==0?'C':'D';expected.append(after).append(i).append(';');if(failure==after&&i==at){{fails=true;break;}}sum+=mode==0?5:7;
}}
try{{int actual=effects(limit,stop,mode);if(fails||actual!=sum+1100+last)throw new AssertionError("effect result");}}
catch(RuntimeException actual){{if(!fails||actual!=Hook.sentinel)throw new AssertionError("effect exception identity",actual);}}
if(!Hook.trace.equals(expected.toString()))throw new AssertionError("effect trace "+Hook.trace+" expected "+expected);
}}
public static void main(String[]args){{
Object left=new Object(),right=new Object();
for(int limit:new int[]{{-2,0,1,2,7,16}})for(int stop:new int[]{{-1,0,1,6,7,15,16,17}}){{
 int n=Math.max(0,limit);boolean breaks=stop>=0&&stop<n;int last=breaks?stop:n,swaps=breaks?stop+1:n;
 if(beforeAfter(limit,stop)!=(breaks?8*stop+3:8*n))throw new AssertionError("before/after break values");
 if(headerNullExit(limit,stop)!=null)throw new AssertionError("header null break exit");
 if(Float.floatToRawIntBits(restoredFloat(limit,stop))!=0x7fc12345)throw new AssertionError("restored float break exit");
 Hook.trace="";Hook.failure=0;if(headerValue(limit,stop)!=100+last)throw new AssertionError("header final value");
 StringBuilder headers=new StringBuilder();for(int i=0;i<=last;i++)headers.append('H').append(i).append(';');if(!Hook.trace.equals(headers.toString()))throw new AssertionError("header test count");
 for(long a:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})for(long b:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})
  if(wideSwap(limit,stop,a,b)!=((swaps&1)==0?a:b))throw new AssertionError("wide swap before break");
 for(Object a:new Object[]{{null,left,right}})for(Object b:new Object[]{{null,left,right}})
  if(referenceSwap(limit,stop,a,b)!=((swaps&1)==0?a:b))throw new AssertionError("reference swap before break");
 for(int mode:new int[]{{-1,0,1}}){{
  if(nestedBreak(limit,stop,mode)!=(mode==0&&breaks?8*stop+3:8*n))throw new AssertionError("nested arm break");
  for(char failure:new char[]{{0,'H','A','B','C','D','T'}})for(int at:new int[]{{-1,0,1,6,7,15,16,17}})checkEffects(limit,stop,mode,failure,at);
 }}
}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-loop-break-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/LoopBreak.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/LoopBreak.java"),
        ("java", "sample.LoopBreak"),
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
            dir.join("sample/LoopBreak.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
