//! G01-C-loop-outer-composition: forward regions around one canonical loop.
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
    Ok(native_java::render_method("sample.LoopOuter", &class, &class.methods[0])?.source)
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
fn edges(w: &mut Words, first: u16) {
    for (r, t) in [
        (first + 1, "header"),
        (first + 2, "exit"),
        (first + 3, "header"),
        (first + 4, "exit"),
    ] {
        w.edge((r << 12) | 0x32, t);
    }
}
fn scalar_body(w: &mut Words) {
    w.add_sum(3);
    w.increment();
    w.edge(0x3032, "header");
    w.add_sum(1);
    w.edge(0x4032, "exit");
    w.add_sum(5);
    w.edge(0x5032, "header");
    w.add_sum(7);
    w.edge(0x6032, "exit");
    w.add_sum(2);
}
fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for name in ["beforeAfter", "around", "takenLoop", "terminalBypass"] {
        let mut w = Words::default();
        w.emit(&[0x0012, 0x0112]);
        w.branch(7, 0x3a, "preNegative");
        w.add_sum(30);
        w.jump("preJoin");
        w.label("preNegative");
        w.add_sum(20);
        w.label("preJoin");
        if name == "around" {
            w.branch(7, 0x3c, "exit");
        }
        if matches!(name, "takenLoop" | "terminalBypass") {
            w.branch(7, 0x3d, "header");
            w.add_sum(50);
            if name == "terminalBypass" {
                w.emit(&[0x010f]);
            } else {
                w.jump("exit");
            }
        }
        w.label("header");
        w.guard(0x2035);
        scalar_body(&mut w);
        w.latch();
        w.label("exit");
        w.branch(7, 0x38, "postZero");
        w.add_sum(70);
        w.jump("postJoin");
        w.label("postZero");
        w.add_sum(60);
        w.label("postJoin");
        w.emit(&[0x010f]);
        out.push(plain(name, vec!["I"; 6], "I", 2, w));
    }
    for wide in [true, false] {
        let mut w = Words::default();
        let first = if wide { 7 } else { 4 };
        let mode = first + 5;
        let swap = if wide {
            vec![0x1504, 0x3104, 0x5304]
        } else {
            vec![0x1307, 0x2107, 0x3207]
        };
        if wide {
            w.emit(&[0x0012, 0xd104, 0xf304]);
        } else {
            w.emit(&[0x0012, 0xa107, 0xb207]);
        }
        w.branch(mode, 0x3c, "exit");
        w.label("header");
        w.guard((first << 12) | 0x35);
        w.emit(&swap);
        w.increment();
        w.edge(((first + 1) << 12) | 0x32, "header");
        w.edge(((first + 2) << 12) | 0x32, "exit");
        w.emit(&swap);
        w.edge(((first + 3) << 12) | 0x32, "header");
        w.edge(((first + 4) << 12) | 0x32, "exit");
        w.emit(&swap);
        w.latch();
        w.label("exit");
        w.branch(mode, 0x39, "postJoin");
        w.branch(0, 0x38, "postJoin");
        w.emit(&swap);
        w.label("postJoin");
        w.emit(&[if wide { 0x0110 } else { 0x0111 }]);
        let mut p = vec!["I"; 6];
        p.extend(if wide {
            vec!["J", "J"]
        } else {
            vec!["Ljava/lang/Object;", "Ljava/lang/Object;"]
        });
        out.push(plain(
            if wide { "wideOuter" } else { "referenceOuter" },
            p,
            if wide { "J" } else { "Ljava/lang/Object;" },
            first,
            w,
        ));
    }
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112, 0x0213, 100]);
    w.branch(9, 0x38, "preA");
    w.stage(2);
    w.accumulate();
    w.jump("preJoin");
    w.label("preA");
    w.stage(1);
    w.accumulate();
    w.label("preJoin");
    w.branch(9, 0x3c, "exit");
    w.label("header");
    w.emit(&[0x1071, 0, 0, 0x020a]);
    w.guard(0x4035);
    w.stage(3);
    w.accumulate();
    w.increment();
    w.edge(0x5032, "header");
    w.edge(0x6032, "exit");
    w.stage(4);
    w.accumulate();
    w.edge(0x7032, "header");
    w.edge(0x8032, "exit");
    w.latch();
    w.label("exit");
    w.branch(9, 0x38, "postB");
    w.stage(1);
    w.accumulate();
    w.jump("postJoin");
    w.label("postB");
    w.stage(2);
    w.accumulate();
    w.label("postJoin");
    w.emit(&[0x2071, 5, 0x0021, 0x000a, 0x000f]);
    let mut c = plain("effects", vec!["I"; 6], "I", 4, w);
    c.hooks = true;
    out.push(c);
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0113, 100]);
    w.branch(7, 0x3c, "exit");
    w.label("header");
    w.emit(&[0x1071, 0, 0, 0x010a]);
    w.guard(0x2035);
    w.increment();
    edges(&mut w, 2);
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    let mut c = plain("headerValue", vec!["I"; 6], "I", 2, w);
    c.hooks = true;
    out.push(c);
    for float in [false, true] {
        let mut w = Words::default();
        w.emit(&[0x0012]);
        let first = if float { 4 } else { 2 };
        if float {
            w.emit(&[0x0114, 0x2345, 0x7fc1]);
        } else {
            w.emit(&[0x0112]);
        }
        w.branch(first + 5, 0x3c, "exit");
        w.label("header");
        if !float {
            w.emit(&[0x0112]);
        }
        w.guard((first << 12) | 0x35);
        w.increment();
        for (r, t) in [
            (first + 1, "header"),
            (first + 2, "exit"),
            (first + 3, "header"),
            (first + 4, "exit"),
        ] {
            if float {
                w.emit(&[0x1201, 0x0112, 0x2101]);
            }
            w.edge((r << 12) | 0x32, t);
        }
        w.latch();
        w.label("exit");
        if float {
            w.emit(&[0x010f]);
        } else {
            w.emit(&[0x011f, 0, 0x0111]);
        }
        let mut c = plain(
            if float { "restoredFloat" } else { "nullOuter" },
            vec!["I"; 6],
            if float { "F" } else { "Ljava/lang/String;" },
            first,
            w,
        );
        if !float {
            c.types = vec!["Ljava/lang/String;"];
        }
        out.push(c);
    }
    out
}
#[test]
fn outer_regions_preserve_one_loop_and_navigation() {
    assert_eq!(cases().len(), 10);
    for c in cases() {
        let class = fixture(&c);
        let code = native_java::render_method("sample.LoopOuter", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", c.name));
        assert_eq!(
            code.source.matches("while (").count(),
            1,
            "{}: {}",
            c.name,
            code.source
        );
        if c.hooks {
            assert!(
                code.links
                    .iter()
                    .any(|l| l.label == "sample.Hook.header(I)I")
            );
        }
        if c.name == "nullOuter" {
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
fn malformed_outer_and_loop_operands_fail_closed() {
    for name in ["around", "takenLoop"] {
        for offset in [1, 999] {
            let mut c = cases().into_iter().find(|c| c.name == name).unwrap();
            let op = if name == "around" { 0x073c } else { 0x073d };
            let pc = c.words.iter().position(|x| *x == op).unwrap();
            c.words[pc + 1] = offset;
            assert!(render(&c).is_err());
        }
    }
    let mut c = cases().remove(0);
    c.words[2] = 0xff3a;
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    c.words.truncate(3);
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    c.parameters[5] = "Ljava/lang/Object;";
    assert!(render(&c).is_err());
    let mut c = cases().into_iter().find(|c| c.name == "nullOuter").unwrap();
    c.types.clear();
    assert!(render(&c).is_err());
    let mut c = cases().into_iter().find(|c| c.name == "wideOuter").unwrap();
    let pc = c.words.iter().position(|x| *x == 0x1504).unwrap();
    c.words[pc] = 0x0105;
    c.words[pc + 1] = 16;
    assert!(render(&c).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn outer_loop_jvm_matches_bypass_merges_and_ordered_failures() {
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
public class LoopOuter{{
{methods}
static int[] state(int n,int ca,int ba,int cb,int bb){{int s=0,swaps=0,last=n,counter=n;for(int k=1;k<=n;k++){{s+=3;swaps++;if(k==ca)continue;s++;if(k==ba){{last=k-1;counter=k;break;}}s+=5;swaps++;if(k==cb)continue;s+=7;if(k==bb){{last=k-1;counter=k;break;}}s+=2;swaps++;}}return new int[]{{s,swaps,last,counter}};}}
static void effectsCheck(int limit,int ca,int ba,int cb,int bb,int mode,char failure,int at){{
int n=Math.max(0,limit),sum=0,last=0,counter=0;StringBuilder expected=new StringBuilder();
char pre=mode==0?'A':'B';expected.append(pre).append(0).append(';');sum+=mode==0?3:1;
if(mode<=0)for(int i=0;i<=n;i++){{last=i;counter=i;expected.append('H').append(i).append(';');if(i==n)break;
expected.append('C').append(i).append(';');sum+=5;int k=i+1;counter=k;if(k==ca)continue;if(k==ba)break;
expected.append('D').append(k).append(';');sum+=7;if(k==cb)continue;if(k==bb)break;
}}
char post=mode==0?'B':'A';expected.append(post).append(counter).append(';');sum+=mode==0?1:3;expected.append('T').append(last).append(';');
String trace=expected.toString();boolean fails=false;String prefix=trace;
for(String event:trace.split(";")){{char stage=event.charAt(0);int i=Integer.parseInt(event.substring(1));if(stage==failure&&(stage=='T'||i==at)){{fails=true;prefix=trace.substring(0,trace.indexOf(event+";")+event.length()+1);break;}}}}
Hook.trace="";Hook.failure=failure;Hook.failAt=at;
try{{int actual=effects(limit,ca,ba,cb,bb,mode);if(fails||actual!=sum+1100+last)throw new AssertionError("effect result");}}
catch(RuntimeException actual){{if(!fails||actual!=Hook.sentinel)throw new AssertionError("exception identity",actual);}}
if(!Hook.trace.equals(prefix))throw new AssertionError("effect trace "+Hook.trace+" expected "+prefix);
}}
public static void main(String[]args){{Object left=new Object(),right=new Object();
for(int limit:new int[]{{-1,0,1,3,6}})for(int ca:new int[]{{-1,1,3,6}})for(int ba:new int[]{{-1,1,3,6}})for(int cb:new int[]{{-1,1,3,6}})for(int bb:new int[]{{-1,1,3,6}})for(int mode:new int[]{{-1,0,1}}){{
int n=Math.max(0,limit);int[] full=state(n,ca,ba,cb,bb),selected=mode>0?state(0,ca,ba,cb,bb):full;
int pre=mode<0?20:30,post=mode==0?60:70;
if(beforeAfter(limit,ca,ba,cb,bb,mode)!=pre+full[0]+post)throw new AssertionError("prefix/tail joins");
if(around(limit,ca,ba,cb,bb,mode)!=pre+selected[0]+post)throw new AssertionError("fallthrough loop bypass");
if(takenLoop(limit,ca,ba,cb,bb,mode)!=pre+selected[0]+post+(mode>0?50:0))throw new AssertionError("taken loop bypass");
if(terminalBypass(limit,ca,ba,cb,bb,mode)!=(mode>0?pre+50:pre+full[0]+post))throw new AssertionError("terminal bypass return");
int swaps=selected[1]+(mode==0&&selected[3]>0?1:0);
for(long a:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})for(long b:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})
if(wideOuter(limit,ca,ba,cb,bb,mode,a,b)!=((swaps&1)==0?a:b))throw new AssertionError("wide outer phi");
for(Object a:new Object[]{{null,left,right}})for(Object b:new Object[]{{null,left,right}})
if(referenceOuter(limit,ca,ba,cb,bb,mode,a,b)!=((swaps&1)==0?a:b))throw new AssertionError("reference outer phi");
Hook.trace="";Hook.failure=0;if(headerValue(limit,ca,ba,cb,bb,mode)!=100+selected[2])throw new AssertionError("header/bypass value");
StringBuilder headers=new StringBuilder();if(mode<=0)for(int i=0;i<=selected[2];i++)headers.append('H').append(i).append(';');if(!Hook.trace.equals(headers.toString()))throw new AssertionError("bypass skips header");
if(nullOuter(limit,ca,ba,cb,bb,mode)!=null)throw new AssertionError("null merge");
if(Float.floatToRawIntBits(restoredFloat(limit,ca,ba,cb,bb,mode))!=0x7fc12345)throw new AssertionError("float invariant outer merge");
for(char failure:new char[]{{0,'H','A','B','C','D','T'}})for(int at:new int[]{{-1,0,1,2,3,5,6}})effectsCheck(limit,ca,ba,cb,bb,mode,failure,at);
}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-loop-outer-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/LoopOuter.java"), java).unwrap();
    for (program, arg) in [
        ("javac", "sample/LoopOuter.java"),
        ("java", "sample.LoopOuter"),
    ] {
        let output = Command::new(program)
            .arg(arg)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}\nSource: {}",
            String::from_utf8_lossy(&output.stderr),
            dir.join("sample/LoopOuter.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
