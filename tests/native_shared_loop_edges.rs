//! G01-C-loop-edge-composition: ordered conditional edges to the same header/common exit.
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
    Ok(native_java::render_method("sample.LoopEdges", &class, &class.methods[0])?.source)
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
fn scalar_edges(w: &mut Words, first: u16, nested: bool) {
    w.add_sum(3);
    w.increment();
    w.edge(((first + 1) << 12) | 0x32, "header");
    w.add_sum(1);
    w.edge(((first + 2) << 12) | 0x32, "exit");
    if nested {
        w.branch(first + 5, 0x39, "join");
    }
    w.add_sum(5);
    w.edge(((first + 3) << 12) | 0x32, "header");
    w.add_sum(7);
    w.edge(((first + 4) << 12) | 0x32, "exit");
    if nested {
        w.label("join");
    }
    w.add_sum(2);
}
fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for name in ["sequential", "nested"] {
        let mut w = Words::default();
        w.emit(&[0x0012, 0x0112]);
        w.label("header");
        w.guard(0x2035);
        scalar_edges(&mut w, 2, name == "nested");
        w.latch();
        w.label("exit");
        w.emit(&[0x010f]);
        let mut p = vec!["I"; 5];
        if name == "nested" {
            p.push("I");
        }
        out.push(plain(name, p, "I", 2, w));
    }
    for wide in [true, false] {
        let mut w = Words::default();
        let first = if wide { 7 } else { 4 };
        let swap = if wide {
            vec![0x1504, 0x3104, 0x5304]
        } else {
            vec![0x1307, 0x2107, 0x3207]
        };
        if wide {
            w.emit(&[0x0012, 0xc104, 0xe304]);
        } else {
            w.emit(&[0x0012, 0x9107, 0xa207]);
        }
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
        w.emit(&[if wide { 0x0110 } else { 0x0111 }]);
        let mut p = vec!["I"; 5];
        p.extend(if wide {
            vec!["J", "J"]
        } else {
            vec!["Ljava/lang/Object;", "Ljava/lang/Object;"]
        });
        out.push(plain(
            if wide { "wideSwap" } else { "referenceSwap" },
            p,
            if wide { "J" } else { "Ljava/lang/Object;" },
            first,
            w,
        ));
    }
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("header");
    w.emit(&[0x1071, 0, 0, 0x020a]);
    w.guard(0x4035);
    w.stage(1);
    w.accumulate();
    w.increment();
    w.edge(0x5032, "header");
    w.stage(2);
    w.accumulate();
    w.edge(0x6032, "exit");
    w.branch(9, 0x39, "join");
    w.stage(3);
    w.accumulate();
    w.edge(0x7032, "header");
    w.stage(4);
    w.accumulate();
    w.edge(0x8032, "exit");
    w.label("join");
    w.latch();
    w.label("exit");
    w.emit(&[0x2071, 5, 0x0021, 0x000a, 0x000f]);
    let mut c = plain("effects", vec!["I"; 6], "I", 4, w);
    c.hooks = true;
    out.push(c);
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("header");
    w.emit(&[0x1071, 0, 0, 0x010a]);
    w.guard(0x2035);
    w.increment();
    for (operand, target) in [(3, "header"), (4, "exit"), (5, "header"), (6, "exit")] {
        w.edge((operand << 12) | 0x32, target);
    }
    w.latch();
    w.label("exit");
    w.emit(&[0x010f]);
    let mut c = plain("headerValue", vec!["I"; 5], "I", 2, w);
    c.hooks = true;
    out.push(c);
    for float in [false, true] {
        let mut w = Words::default();
        w.emit(&[0x0012]);
        let first = if float { 4 } else { 2 };
        if float {
            w.emit(&[0x0114, 0x2345, 0x7fc1]);
        }
        w.label("header");
        if !float {
            w.emit(&[0x0112]);
        }
        w.guard((first << 12) | 0x35);
        w.increment();
        for (operand, target) in [
            (first + 1, "header"),
            (first + 2, "exit"),
            (first + 3, "header"),
            (first + 4, "exit"),
        ] {
            if float {
                w.emit(&[0x1201, 0x0112, 0x2101]);
            }
            w.edge((operand << 12) | 0x32, target);
        }
        w.latch();
        w.label("exit");
        if float {
            w.emit(&[0x010f]);
        } else {
            w.emit(&[0x011f, 0, 0x0111]);
        }
        let mut c = plain(
            if float {
                "restoredFloat"
            } else {
                "headerNullExit"
            },
            vec!["I"; 5],
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
fn multiple_edges_preserve_snapshots_and_navigation() {
    assert_eq!(cases().len(), 8);
    for c in cases() {
        let class = fixture(&c);
        let code = native_java::render_method("sample.LoopEdges", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", c.name));
        assert_eq!(
            code.source.matches("while (").count(),
            1,
            "{}: {}",
            c.name,
            code.source
        );
        assert!(
            code.source.matches("continue;").count() >= 2,
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
        if c.name == "headerNullExit" {
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
fn malformed_edges_and_live_operands_fail_closed() {
    for word in [0x3032, 0x4032, 0x5032, 0x6032] {
        for offset in [1, 999] {
            let mut c = cases().remove(0);
            let pc = c.words.iter().position(|x| *x == word).unwrap();
            c.words[pc + 1] = offset;
            assert!(render(&c).is_err(), "edge {word:x} offset {offset}");
        }
    }
    let mut c = cases().remove(0);
    let pc = c.words.iter().position(|x| *x == 0x6032).unwrap();
    c.words[pc] = 0xf032;
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    let pc = c.words.iter().position(|x| *x == 0x5032).unwrap();
    c.words.truncate(pc + 1);
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    c.parameters[3] = "Ljava/lang/Object;";
    let pc = c.words.iter().position(|x| *x == 0x5032).unwrap();
    c.words[pc] = 0x5034;
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
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
    c.words[pc] = 0xf504;
    assert!(render(&c).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn loop_edges_jvm_matches_order_snapshots_and_exception_identity() {
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
public class LoopEdges{{
{methods}
static int sum(int n,int ca,int ba,int cb,int bb,int mode){{
int s=0;for(int k=1;k<=n;k++){{s+=3;if(k==ca)continue;s+=1;if(k==ba)break;if(mode==0){{s+=5;if(k==cb)continue;s+=7;if(k==bb)break;}}s+=2;}}return s;
}}
static int[] state(int n,int ca,int ba,int cb,int bb){{
int swaps=0,last=n;for(int k=1;k<=n;k++){{swaps++;if(k==ca)continue;if(k==ba){{last=k-1;break;}}swaps++;if(k==cb)continue;if(k==bb){{last=k-1;break;}}swaps++;}}return new int[]{{swaps,last}};
}}
static void effectsCheck(int limit,int ca,int ba,int cb,int bb,int mode,char failure,int at){{
int n=Math.max(0,limit),sum=0,last=0;boolean fails=false;StringBuilder expected=new StringBuilder();
for(int i=0;i<=n;i++){{last=i;expected.append('H').append(i).append(';');if(failure=='H'&&i==at){{fails=true;break;}}
if(i==n){{expected.append('T').append(i).append(';');fails=failure=='T';break;}}
expected.append('A').append(i).append(';');if(failure=='A'&&i==at){{fails=true;break;}}sum+=3;int k=i+1;if(k==ca)continue;
expected.append('B').append(k).append(';');if(failure=='B'&&k==at){{fails=true;break;}}sum++;
if(k==ba){{expected.append('T').append(i).append(';');fails=failure=='T';break;}}
if(mode==0){{expected.append('C').append(k).append(';');if(failure=='C'&&k==at){{fails=true;break;}}sum+=5;if(k==cb)continue;
expected.append('D').append(k).append(';');if(failure=='D'&&k==at){{fails=true;break;}}sum+=7;
if(k==bb){{expected.append('T').append(i).append(';');fails=failure=='T';break;}}}}
}}
Hook.trace="";Hook.failure=failure;Hook.failAt=at;
try{{int actual=effects(limit,ca,ba,cb,bb,mode);if(fails||actual!=sum+1100+last)throw new AssertionError("effect result");}}
catch(RuntimeException actual){{if(!fails||actual!=Hook.sentinel)throw new AssertionError("exception identity",actual);}}
if(!Hook.trace.equals(expected.toString()))throw new AssertionError("effect trace "+Hook.trace+" expected "+expected);
}}
public static void main(String[]args){{Object left=new Object(),right=new Object();
for(int limit:new int[]{{-1,0,1,3,6}})for(int ca:new int[]{{-1,1,3,6}})for(int ba:new int[]{{-1,1,3,6}})for(int cb:new int[]{{-1,1,3,6}})for(int bb:new int[]{{-1,1,3,6}}){{
int n=Math.max(0,limit);int[] state=state(n,ca,ba,cb,bb);
if(sequential(limit,ca,ba,cb,bb)!=sum(n,ca,ba,cb,bb,0))throw new AssertionError("sequential priority");
for(int mode:new int[]{{-1,0,1}})if(nested(limit,ca,ba,cb,bb,mode)!=sum(n,ca,ba,cb,bb,mode))throw new AssertionError("nested edges");
for(long a:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})for(long b:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})
if(wideSwap(limit,ca,ba,cb,bb,a,b)!=((state[0]&1)==0?a:b))throw new AssertionError("wide snapshots");
for(Object a:new Object[]{{null,left,right}})for(Object b:new Object[]{{null,left,right}})
if(referenceSwap(limit,ca,ba,cb,bb,a,b)!=((state[0]&1)==0?a:b))throw new AssertionError("reference snapshots");
Hook.trace="";Hook.failure=0;if(headerValue(limit,ca,ba,cb,bb)!=100+state[1])throw new AssertionError("header exit value");
StringBuilder h=new StringBuilder();for(int i=0;i<=state[1];i++)h.append('H').append(i).append(';');if(!Hook.trace.equals(h.toString()))throw new AssertionError("header trace");
if(headerNullExit(limit,ca,ba,cb,bb)!=null)throw new AssertionError("null exit");
if(Float.floatToRawIntBits(restoredFloat(limit,ca,ba,cb,bb))!=0x7fc12345)throw new AssertionError("restored float all edges");
for(int mode:new int[]{{-1,0,1}})for(char failure:new char[]{{0,'H','A','B','C','D','T'}})for(int at:new int[]{{-1,0,1,2,3,5,6}})effectsCheck(limit,ca,ba,cb,bb,mode,failure,at);
}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-loop-edges-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/LoopEdges.java"), java).unwrap();
    for (program, arg) in [
        ("javac", "sample/LoopEdges.java"),
        ("java", "sample.LoopEdges"),
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
            dir.join("sample/LoopEdges.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
