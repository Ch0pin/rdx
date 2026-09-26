//! G01-C-loop-continue: one conditional body edge to the same loop header.
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
                assert!((-32768..32768).contains(&delta) && delta != 0);
                self.words[pc + 1] = delta as i16 as u16;
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
    Ok(native_java::render_method("sample.LoopContinue", &class, &class.methods[0])?.source)
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
    for name in [
        "beforeAfter",
        "breakFirst",
        "continueFirst",
        "nestedContinue",
    ] {
        let mut w = Words::default();
        w.emit(&[0x0012, 0x0112]);
        w.label("header");
        w.guard(0x2035);
        w.add_sum(3);
        w.increment();
        if name == "nestedContinue" {
            w.branch(4, 0x39, "afterContinue");
        }
        if name == "breakFirst" {
            w.edge(0x4032, "exit");
        }
        w.edge(0x3032, "header");
        if name == "continueFirst" {
            w.edge(0x4032, "exit");
        }
        if name == "nestedContinue" {
            w.label("afterContinue");
        }
        w.add_sum(5);
        w.latch();
        w.label("exit");
        w.emit(&[0x010f]);
        let params = if name == "beforeAfter" {
            vec!["I", "I"]
        } else {
            vec!["I", "I", "I"]
        };
        out.push(plain(name, params, "I", 2, w));
    }
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("header");
    w.emit(&[0x1071, 0, 0, 0x010a]);
    w.guard(0x2035);
    w.increment();
    w.edge(0x3032, "header");
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    let mut c = plain("headerValue", vec!["I", "I"], "I", 2, w);
    c.hooks = true;
    out.push(c);
    let mut w = Words::default();
    w.emit(&[0x0012, 0x9104, 0xb304]);
    w.label("header");
    w.guard(0x7035);
    w.emit(&[0x1504, 0x3104, 0x5304]);
    w.increment();
    w.edge(0x8032, "header");
    w.emit(&[0x1504, 0x3104, 0x5304]);
    w.latch();
    w.label("exit");
    w.emit(&[0x0110]);
    out.push(plain("wideSwap", vec!["I", "I", "J", "J"], "J", 7, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x6107, 0x7207]);
    w.label("header");
    w.guard(0x4035);
    w.emit(&[0x1307, 0x2107, 0x3207]);
    w.increment();
    w.edge(0x5032, "header");
    w.emit(&[0x1307, 0x2107, 0x3207]);
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
    w.emit(&[0x1071, 0, 0, 0x020a]);
    w.guard(0x4035);
    w.branch(7, 0x38, "armA");
    w.stage(2);
    w.accumulate();
    w.jump("beforeJoin");
    w.label("armA");
    w.stage(1);
    w.accumulate();
    w.label("beforeJoin");
    w.increment();
    w.edge(0x5032, "header");
    w.edge(0x6032, "exit");
    w.branch(7, 0x38, "armC");
    w.stage(4);
    w.accumulate();
    w.jump("afterJoin");
    w.label("armC");
    w.stage(3);
    w.accumulate();
    w.label("afterJoin");
    w.latch();
    w.label("exit");
    w.emit(&[0x2071, 5, 0x0021, 0x000a, 0x000f]);
    let mut c = plain("effects", vec!["I", "I", "I", "I"], "I", 4, w);
    c.hooks = true;
    out.push(c);
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("header");
    w.emit(&[0x0112]);
    w.guard(0x2035);
    w.increment();
    w.edge(0x3032, "header");
    w.latch();
    w.label("exit");
    w.emit(&[0x011f, 0, 0x0111]);
    let mut c = plain("headerNullExit", vec!["I", "I"], "Ljava/lang/String;", 2, w);
    c.types = vec!["Ljava/lang/String;"];
    out.push(c);
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0114, 0x2345, 0x7fc1]);
    w.label("header");
    w.guard(0x4035);
    w.emit(&[0x1201, 0x0112, 0x2101]);
    w.increment();
    w.edge(0x5032, "header");
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    out.push(plain("restoredFloat", vec!["I", "I"], "F", 4, w));
    out
}
#[test]
fn conditional_continues_preserve_snapshots_and_navigation() {
    let cases = cases();
    assert_eq!(cases.len(), 10);
    for case in cases {
        let class = fixture(&case);
        let code = native_java::render_method("sample.LoopContinue", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", case.name));
        assert_eq!(
            code.source.matches("while (").count(),
            1,
            "{}: {}",
            case.name,
            code.source
        );
        assert!(
            code.source.contains("continue;"),
            "{}: {}",
            case.name,
            code.source
        );
        if case.hooks {
            assert!(
                code.links
                    .iter()
                    .any(|l| l.label == "sample.Hook.header(I)I")
            );
        }
        if case.name == "headerNullExit" {
            let links = code
                .links
                .iter()
                .filter(|l| l.label == "java.lang.String")
                .collect::<Vec<_>>();
            assert!(!links.is_empty());
            for l in links {
                assert_eq!(
                    code.source
                        .chars()
                        .skip(l.start)
                        .take(l.end - l.start)
                        .collect::<String>(),
                    "java.lang.String"
                );
            }
        }
    }
}
#[test]
fn malformed_continue_edges_and_operands_fail_closed() {
    let base = || cases().remove(0);
    for offset in [1, 999, 0xffff] {
        let mut c = base();
        let pc = c.words.iter().position(|x| *x == 0x3032).unwrap();
        c.words[pc + 1] = offset;
        assert!(render(&c).is_err(), "accepted offset {offset}");
    }
    let mut c = base();
    let pc = c.words.iter().position(|x| *x == 0x3032).unwrap();
    c.words[pc] = 0xf032;
    assert!(render(&c).is_err());
    let mut c = base();
    let pc = c.words.iter().position(|x| *x == 0x3032).unwrap();
    c.words.truncate(pc + 1);
    assert!(render(&c).is_err());
    let mut c = base();
    c.parameters[1] = "Ljava/lang/Object;";
    let pc = c.words.iter().position(|x| *x == 0x3032).unwrap();
    c.words[pc] = 0x3034;
    assert!(render(&c).is_err());
    let mut c = base();
    c.words[1] = 0;
    assert!(render(&c).is_err());
    let mut c = cases()
        .into_iter()
        .find(|c| c.name == "headerNullExit")
        .unwrap();
    c.types.clear();
    assert!(render(&c).is_err());
    let mut c = cases().into_iter().find(|c| c.name == "wideSwap").unwrap();
    let pc = c.words.iter().position(|x| *x == 0x1504).unwrap();
    c.words[pc] = 0xc504;
    assert!(render(&c).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn loop_continue_jvm_matches_skipped_effects_snapshots_and_break_priority() {
    let methods = cases()
        .iter()
        .map(|c| render(c).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let java = format!(
        r#"package sample;
class Hook{{
static String trace="";static char failure;static int failAt;static final RuntimeException sentinel=new RuntimeException("sentinel");
static void mark(char s,int i){{trace+=""+s+i+";";if(s==failure&&(s=='T'||i==failAt))throw sentinel;}}
static int header(int i){{mark('H',i);return 100+i;}}
static int a(int i){{mark('A',i);return 3;}}static int b(int i){{mark('B',i);return 1;}}
static int c(int i){{mark('C',i);return 5;}}static int d(int i){{mark('D',i);return 7;}}
static int tail(int sum,int h){{mark('T',h-100);return sum+h+1000;}}
}}
public class LoopContinue{{
{methods}
static int expectedSum(int n,int skip,int stop,boolean breakFirst){{
int sum=0;for(int i=1;i<=n;i++){{sum+=3;if(breakFirst&&i==stop)break;if(i==skip)continue;if(!breakFirst&&i==stop)break;sum+=5;}}return sum;
}}
static void checkEffects(int limit,int skip,int stop,int mode,char failure,int at){{
int n=Math.max(0,limit),sum=0,last=0;StringBuilder expected=new StringBuilder();boolean fails=false;
for(int i=0;i<=n;i++){{
 last=i;expected.append('H').append(i).append(';');if(failure=='H'&&i==at){{fails=true;break;}}
 if(i==n){{expected.append('T').append(i).append(';');fails=failure=='T';break;}}
 char before=mode==0?'A':'B';expected.append(before).append(i).append(';');if(failure==before&&i==at){{fails=true;break;}}sum+=mode==0?3:1;
 int updated=i+1;if(updated==skip)continue;
 if(updated==stop){{expected.append('T').append(i).append(';');fails=failure=='T';break;}}
 char after=mode==0?'C':'D';expected.append(after).append(updated).append(';');if(failure==after&&updated==at){{fails=true;break;}}sum+=mode==0?5:7;
}}
Hook.trace="";Hook.failure=failure;Hook.failAt=at;
try{{int actual=effects(limit,skip,stop,mode);if(fails||actual!=sum+1100+last)throw new AssertionError("effect result");}}
catch(RuntimeException actual){{if(!fails||actual!=Hook.sentinel)throw new AssertionError("exception identity",actual);}}
if(!Hook.trace.equals(expected.toString()))throw new AssertionError("effect trace "+Hook.trace+" expected "+expected);
}}
public static void main(String[]args){{
Object left=new Object(),right=new Object();
for(int limit:new int[]{{-2,0,1,2,7,16}})for(int skip:new int[]{{-1,0,1,2,7,16,17}}){{
 int n=Math.max(0,limit);boolean takes=skip>0&&skip<=n;
 if(beforeAfter(limit,skip)!=8*n-(takes?5:0))throw new AssertionError("skipped sum");
 if(headerNullExit(limit,skip)!=null)throw new AssertionError("header null exit");
 if(Float.floatToRawIntBits(restoredFloat(limit,skip))!=0x7fc12345)throw new AssertionError("restored float continue");
 Hook.trace="";Hook.failure=0;if(headerValue(limit,skip)!=100+n)throw new AssertionError("header value");
 StringBuilder headers=new StringBuilder();for(int i=0;i<=n;i++)headers.append('H').append(i).append(';');if(!Hook.trace.equals(headers.toString()))throw new AssertionError("header reexecution");
 for(long a:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})for(long b:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})
  if(wideSwap(limit,skip,a,b)!=(takes?b:a))throw new AssertionError("wide snapshot swaps");
 for(Object a:new Object[]{{null,left,right}})for(Object b:new Object[]{{null,left,right}})
  if(referenceSwap(limit,skip,a,b)!=(takes?b:a))throw new AssertionError("reference snapshot swaps");
 for(int mode:new int[]{{-1,0,1}})if(nestedContinue(limit,skip,mode)!=8*n-(mode==0&&takes?5:0))throw new AssertionError("nested continue");
 for(int stop:new int[]{{-1,0,1,2,7,16,17}}){{
  if(breakFirst(limit,skip,stop)!=expectedSum(n,skip,stop,true))throw new AssertionError("break first");
  if(continueFirst(limit,skip,stop)!=expectedSum(n,skip,stop,false))throw new AssertionError("continue first");
  for(int mode:new int[]{{-1,0,1}})for(char failure:new char[]{{0,'H','A','B','C','D','T'}})for(int at:new int[]{{-1,0,1,2,7,16,17}})checkEffects(limit,skip,stop,mode,failure,at);
 }}
}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-loop-continue-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/LoopContinue.java"), &java).unwrap();
    for (program, argument) in [
        ("javac", "sample/LoopContinue.java"),
        ("java", "sample.LoopContinue"),
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
            dir.join("sample/LoopContinue.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
