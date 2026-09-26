//! G01-C-loop-body-composition: forward regions inside one natural loop.
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
        self.emit(&[0x1071, method, 0, 0x020a]);
    }
    fn accumulate(&mut self) {
        self.emit(&[0x0190, 0x0201]);
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
    Ok(native_java::render_method("sample.LoopComposition", &class, &class.methods[0])?.source)
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
    w.branch(0, 0x38, "firstTrue");
    w.add_sum(1);
    w.jump("firstJoin");
    w.label("firstTrue");
    w.add_sum(3);
    w.label("firstJoin");
    w.branch(3, 0x38, "secondTrue");
    w.add_sum(7);
    w.jump("bodyJoin");
    w.label("secondTrue");
    w.add_sum(5);
    w.label("bodyJoin");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("sequential", vec!["I", "I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("header");
    w.guard(0x2035);
    w.branch(0, 0x38, "outerTrue");
    w.branch(3, 0x38, "leftTrue");
    w.add_sum(1);
    w.jump("bodyJoin");
    w.label("leftTrue");
    w.add_sum(3);
    w.jump("bodyJoin");
    w.label("outerTrue");
    w.branch(3, 0x3a, "rightTrue");
    w.add_sum(7);
    w.jump("bodyJoin");
    w.label("rightTrue");
    w.add_sum(5);
    w.label("bodyJoin");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("nested", vec!["I", "I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("header");
    w.guard(0x3035);
    w.branch(4, 0x38, "trueArm");
    w.emit(&[0x1212]);
    w.add_sum(1);
    w.jump("bodyJoin");
    w.label("trueArm");
    w.emit(&[0x5207]);
    w.add_sum(3);
    w.label("bodyJoin");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain(
        "deadLive",
        vec!["I", "I", "Ljava/lang/Object;"],
        "I",
        3,
        w,
    ));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x4104]);
    w.label("header");
    w.guard(0x3035);
    w.branch(8, 0x38, "trueArm");
    w.emit(&[0x019c, 0x0601]);
    w.jump("bodyJoin");
    w.label("trueArm");
    w.emit(&[0x019b, 0x0601]);
    w.label("bodyJoin");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x0110]);
    out.push(plain("wideLiveout", vec!["I", "J", "J", "I"], "J", 3, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x4107]);
    w.label("header");
    w.guard(0x2035);
    w.branch(3, 0x38, "trueArm");
    w.emit(&[0x5107]);
    w.jump("bodyJoin");
    w.label("trueArm");
    w.emit(&[0x4107]);
    w.label("bodyJoin");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x011f, 0, 0x0111]);
    let mut case = plain(
        "referenceLiveout",
        vec!["I", "I", "Ljava/lang/String;", "Ljava/lang/String;"],
        "Ljava/lang/String;",
        2,
        w,
    );
    case.types = vec!["Ljava/lang/String;"];
    out.push(case);
    let mut w = Words::default();
    w.emit(&[0x0012, 0x6107, 0x7207]);
    w.label("header");
    w.guard(0x4035);
    w.branch(5, 0x39, "bodyJoin");
    w.emit(&[0x1307, 0x2107, 0x3207]);
    w.label("bodyJoin");
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
    w.emit(&[0x0012, 0x9104, 0xb304]);
    w.label("header");
    w.guard(0x7035);
    w.branch(8, 0x39, "bodyJoin");
    w.emit(&[0x1504, 0x3104, 0x5304]);
    w.label("bodyJoin");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x0110]);
    out.push(plain("wideSwap", vec!["I", "I", "J", "J"], "J", 7, w));
    for nested in [false, true] {
        let mut w = Words::default();
        w.emit(&[0x0012, 0x0112]);
        w.label("header");
        w.emit(&[0x1071, 0, 0, 0x020a]);
        w.guard(0x3035);
        if nested {
            w.branch(0, 0x38, "outerTrue");
            w.branch(4, 0x38, "leftTrue");
            w.stage(2);
            w.jump("selected");
            w.label("leftTrue");
            w.stage(1);
            w.jump("selected");
            w.label("outerTrue");
            w.branch(4, 0x3a, "rightTrue");
            w.stage(4);
            w.jump("selected");
            w.label("rightTrue");
            w.stage(3);
            w.label("selected");
            w.accumulate();
        } else {
            w.branch(0, 0x38, "firstTrue");
            w.stage(2);
            w.jump("firstJoin");
            w.label("firstTrue");
            w.stage(1);
            w.label("firstJoin");
            w.accumulate();
            w.branch(4, 0x38, "secondTrue");
            w.stage(4);
            w.jump("secondJoin");
            w.label("secondTrue");
            w.stage(3);
            w.label("secondJoin");
            w.accumulate();
        }
        w.increment();
        w.latch();
        w.label("exit");
        w.emit(&[0x2071, 5, 0x0021, 0x000a, 0x000f]);
        let mut case = plain(
            if nested {
                "nestedEffects"
            } else {
                "sequentialEffects"
            },
            vec!["I", "I"],
            "I",
            3,
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
    w.branch(3, 0x39, "latch");
    w.add_sum(3);
    w.label("latch");
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("latchJoin", vec!["I", "I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0114, 0x2345, 0x7fc1]);
    w.label("header");
    w.guard(0x4035);
    w.emit(&[0x1201]);
    w.branch(5, 0x38, "trueArm");
    w.emit(&[0x0112, 0x2101]);
    w.jump("bodyJoin");
    w.label("trueArm");
    w.emit(&[0xf112, 0x2101]);
    w.label("bodyJoin");
    w.increment();
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("restoredFloat", vec!["I", "I"], "F", 4, w));
    out
}
#[test]
fn loop_body_regions_preserve_liveouts_effect_sites_and_navigation() {
    let cases = cases();
    assert_eq!(cases.len(), 11);
    for case in cases {
        let class = fixture(&case);
        let code = native_java::render_method("sample.LoopComposition", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert_eq!(
            code.source.matches("while (").count(),
            1,
            "{}: {}",
            case.name,
            code.source
        );
        assert!(
            code.source.matches("if (").count() >= 2,
            "{}: {}",
            case.name,
            code.source
        );
        if case.hooks {
            for method in ["header", "a", "b", "c", "d", "tail"] {
                assert_eq!(
                    code.source.matches(&format!("Hook.{method}(")).count(),
                    1,
                    "{}: {}",
                    case.name,
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
        if case.name == "referenceLiveout" {
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
fn malformed_body_regions_and_live_operands_fail_closed() {
    let base = || cases().remove(0);
    for offset in [1, 999] {
        let mut case = base();
        case.words[5] = offset;
        assert!(render(&case).is_err(), "accepted body target {offset}");
    }
    let mut case = base();
    case.words[4] = 0xff38;
    assert!(render(&case).is_err(), "accepted body condition register");
    let mut case = base();
    case.words.truncate(5);
    assert!(render(&case).is_err(), "accepted truncated body condition");
    let mut case = base();
    case.words[1] = 0;
    assert!(
        render(&case).is_err(),
        "accepted undefined loop-carried sum"
    );
    let mut case = cases()
        .into_iter()
        .find(|c| c.name == "wideLiveout")
        .unwrap();
    // Body subtraction reads a wide pair beginning at the last frame word.
    let frame = case.locals + case.parameters.iter().map(|ty| width(ty)).sum::<u16>();
    let pc = case.words.iter().position(|word| *word == 0x019c).unwrap();
    case.words[pc + 1] = ((frame - 1) << 8) | 1;
    assert!(render(&case).is_err());
    let mut case = cases()
        .into_iter()
        .find(|c| c.name == "referenceLiveout")
        .unwrap();
    case.types.clear();
    assert!(render(&case).is_err());
    let mut case = cases().into_iter().find(|c| c.name == "deadLive").unwrap();
    // Initialize the local for zero iterations, then make its incompatible
    // integer/reference body definitions observable after the loop.
    case.words.insert(2, 0x0212);
    *case.words.last_mut().unwrap() = 0x020f;
    assert!(render(&case).is_err(), "accepted incompatible liveout");
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn loop_composition_jvm_matches_carried_values_identity_and_per_iteration_failures() {
    let methods = cases()
        .iter()
        .map(|c| render(c).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let java = format!(
        r#"package sample;
class Hook {{
static String trace="";static char failure;static int failAt;static final RuntimeException sentinel=new RuntimeException("sentinel");
static void mark(char stage,int i){{trace+=""+stage+i+";";if(stage==failure&&(stage=='T'||i==failAt))throw sentinel;}}
static int header(int i){{mark('H',i);return 100+i;}}
static int a(int i){{mark('A',i);return 3;}}static int b(int i){{mark('B',i);return 1;}}
static int c(int i){{mark('C',i);return 5;}}static int d(int i){{mark('D',i);return 7;}}
static int tail(int sum,int header){{mark('T',header-100);return sum+header+1000;}}
}}
public class LoopComposition{{
{methods}
static void checkEffects(int limit,int mode,boolean nested,char failure,int at){{
int n=Math.max(0,limit);Hook.trace="";Hook.failure=failure;Hook.failAt=at;
StringBuilder expected=new StringBuilder();boolean fails=false;int sum=0;
for(int i=0;i<=n;i++){{
 expected.append('H').append(i).append(';');if(failure=='H'&&i==at){{fails=true;break;}}
 if(i==n){{expected.append('T').append(i).append(';');fails=failure=='T';break;}}
 char first=nested?(i==0?(mode<0?'C':'D'):(mode==0?'A':'B')):(i==0?'A':'B');
 expected.append(first).append(i).append(';');if(failure==first&&i==at){{fails=true;break;}}
 sum+=first=='A'?3:first=='B'?1:first=='C'?5:7;
 if(!nested){{char second=mode==0?'C':'D';expected.append(second).append(i).append(';');if(failure==second&&i==at){{fails=true;break;}}sum+=second=='C'?5:7;}}
}}
try{{int actual=nested?nestedEffects(limit,mode):sequentialEffects(limit,mode);if(fails||actual!=sum+1100+n)throw new AssertionError("effect result");}}
catch(RuntimeException actual){{if(!fails||actual!=Hook.sentinel)throw new AssertionError("effect identity",actual);}}
if(!Hook.trace.equals(expected.toString()))throw new AssertionError("effect trace "+Hook.trace+" expected "+expected);
}}
public static void main(String[]args){{
Object a=new Object(),b=new Object();String left=new String("same"),right=new String("same");
for(int limit:new int[]{{-2,0,1,2,7,16}})for(int mode:new int[]{{-1,0,1}}){{
 int n=Math.max(0,limit);
 if(sequential(limit,mode)!=(n==0?0:n+2)+n*(mode==0?5:7))throw new AssertionError("sequential body");
 if(nested(limit,mode)!=(n==0?0:(mode<0?5:7)+(n-1)*(mode==0?3:1)))throw new AssertionError("nested body");
 if(deadLive(limit,mode,a)!=n*(mode==0?3:1))throw new AssertionError("dead incompatible register");
 if(latchJoin(limit,mode)!=n*(mode==0?3:0))throw new AssertionError("join at latch");
 if(Float.floatToRawIntBits(restoredFloat(limit,mode))!=0x7fc12345)throw new AssertionError("restored raw float");
 for(String x:new String[]{{null,left,right}})for(String y:new String[]{{null,left,right}})
  if(referenceLiveout(limit,mode,x,y)!=(n==0||mode==0?x:y))throw new AssertionError("reference liveout");
 for(Object x:new Object[]{{null,a,b}})for(Object y:new Object[]{{null,a,b}})
  if(referenceSwap(limit,mode,x,y)!=(mode==0&&(n&1)!=0?y:x))throw new AssertionError("reference carried swap");
 for(long seed:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})for(long step:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}}){{
  if(wideLiveout(limit,seed,step,mode)!=seed+(mode==0?(long)n*step:-(long)n*step))throw new AssertionError("wide liveout");
  if(wideSwap(limit,mode,seed,step)!=(mode==0&&(n&1)!=0?step:seed))throw new AssertionError("wide carried swap");
 }}
 for(char failure:new char[]{{0,'H','A','B','C','D','T'}})for(int at:new int[]{{-1,0,1,6,7,15,16,17}}){{checkEffects(limit,mode,false,failure,at);checkEffects(limit,mode,true,failure,at);}}
}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!(
        "rdx-shared-loop-composition-{}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/LoopComposition.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/LoopComposition.java"),
        ("java", "sample.LoopComposition"),
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
            dir.join("sample/LoopComposition.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
