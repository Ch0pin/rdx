//! G01-C-disjoint-loop-composition: nonoverlapping loops within forward regions.
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
            strings: ["h1", "h2", "h3", "a", "b", "c", "tail"]
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
                (0, 0, 5),
                (0, 1, 6),
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
    Ok(native_java::render_method("sample.DisjointLoops", &class, &class.methods[0])?.source)
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
const HEADERS: [&str; 3] = ["header1", "header2", "header3"];
const EXITS: [&str; 3] = ["exit1", "exit2", "exit3"];
const PRE: [&str; 3] = ["pre1", "pre2", "pre3"];
fn scalar_loop(w: &mut Words, index: usize, has_break: bool) {
    w.label(PRE[index]);
    w.emit(&[0x0012]);
    w.label(HEADERS[index]);
    w.edge(((2 + index as u16) << 12) | 0x35, EXITS[index]);
    w.add_sum([3, 5, 7][index]);
    w.increment();
    let (ca, ba) = if index == 1 { (6, 5) } else { (5, 6) };
    w.edge((ca << 12) | 0x32, HEADERS[index]);
    w.add_sum([2, 4, 6][index]);
    if has_break {
        w.edge((ba << 12) | 0x32, EXITS[index]);
    }
    let arm = ["arm1", "arm2", "arm3"][index];
    let join = ["join1", "join2", "join3"][index];
    w.branch(7, 0x38, arm);
    w.add_sum(2);
    w.jump(join);
    w.label(arm);
    w.add_sum(1);
    w.label(join);
    w.jump(HEADERS[index]);
    w.label(EXITS[index]);
}
fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for name in [
        "twoSequential",
        "threeSequential",
        "alternative",
        "bypass",
        "noBreakFirst",
        "noBreakSecond",
        "dependentBound",
        "onlyThirdBreak",
        "alternativeThird",
    ] {
        let mut w = Words::default();
        w.emit(&[0x0112]);
        if matches!(name, "alternative" | "alternativeThird") {
            w.branch(7, 0x38, "pre2");
        }
        if name == "bypass" {
            w.branch(7, 0x3a, "exit1");
        }
        scalar_loop(
            &mut w,
            0,
            !matches!(name, "noBreakFirst" | "onlyThirdBreak"),
        );
        if matches!(name, "alternative" | "alternativeThird") {
            w.jump(if name == "alternativeThird" {
                "pre3"
            } else {
                "tail"
            });
        }
        if name == "bypass" {
            w.branch(7, 0x3c, "tail");
        }
        if name == "dependentBound" {
            w.emit(&[0x0301]);
        }
        scalar_loop(
            &mut w,
            1,
            !matches!(name, "noBreakSecond" | "onlyThirdBreak"),
        );
        if matches!(
            name,
            "threeSequential" | "onlyThirdBreak" | "alternativeThird"
        ) {
            scalar_loop(&mut w, 2, true);
        }
        w.label("tail");
        w.add_sum(11);
        w.emit(&[0x010f]);
        out.push(plain(name, vec!["I"; 6], "I", 2, w));
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
            w.emit(&[0xc104, 0xe304]);
        } else {
            w.emit(&[0x9107, 0xa207]);
        }
        for index in 0..2 {
            w.label(PRE[index]);
            w.emit(&[0x0012]);
            w.label(HEADERS[index]);
            w.edge(((first + index as u16) << 12) | 0x35, EXITS[index]);
            w.emit(&swap);
            w.increment();
            let (ca, ba) = if index == 0 {
                (first + 2, first + 3)
            } else {
                (first + 3, first + 2)
            };
            w.edge((ca << 12) | 0x32, HEADERS[index]);
            w.edge((ba << 12) | 0x32, EXITS[index]);
            if index == 1 {
                w.emit(&swap);
            }
            w.jump(HEADERS[index]);
            w.label(EXITS[index]);
        }
        w.emit(&[if wide { 0x0110 } else { 0x0111 }]);
        let mut p = vec!["I"; 5];
        p.extend(if wide {
            vec!["J", "J"]
        } else {
            vec!["Ljava/lang/Object;", "Ljava/lang/Object;"]
        });
        out.push(plain(
            if wide { "wideCross" } else { "referenceCross" },
            p,
            if wide { "J" } else { "Ljava/lang/Object;" },
            first,
            w,
        ));
    }
    for effects in [false, true] {
        let mut w = Words::default();
        w.emit(&[0x0112, 0x0213, 100]);
        for index in 0..3 {
            if index == 0 {
                w.branch(9, 0x3a, EXITS[index]);
            }
            if index == 1 {
                w.branch(9, 0x3c, EXITS[index]);
            }
            w.label(PRE[index]);
            w.emit(&[0x0012]);
            w.label(HEADERS[index]);
            w.emit(&[0x1071, index as u16, 0, 0x020a]);
            w.edge(((4 + index as u16) << 12) | 0x35, EXITS[index]);
            if effects {
                w.stage(3 + index as u16);
                w.accumulate();
            }
            w.increment();
            let (ca, ba) = if index == 1 { (8, 7) } else { (7, 8) };
            w.edge((ca << 12) | 0x32, HEADERS[index]);
            if effects {
                w.stage(3 + index as u16);
                w.accumulate();
            }
            w.edge((ba << 12) | 0x32, EXITS[index]);
            w.jump(HEADERS[index]);
            w.label(EXITS[index]);
        }
        if effects {
            w.emit(&[0x2071, 6, 0x0021, 0x000a, 0x000f]);
        } else {
            w.emit(&[0x020f]);
        }
        let mut c = plain(
            if effects { "effects" } else { "headerValues" },
            vec!["I"; 6],
            "I",
            4,
            w,
        );
        c.hooks = true;
        out.push(c);
    }
    let mut w = Words::default();
    w.emit(&[0x0114, 0x2345, 0x7fc1]);
    for index in 0..2 {
        w.emit(&[0x0012]);
        w.label(HEADERS[index]);
        w.edge(((4 + index as u16) << 12) | 0x35, EXITS[index]);
        w.emit(&[0x1201, 0x0112, 0x2101]);
        w.increment();
        let (ca, ba) = if index == 0 { (6, 7) } else { (7, 6) };
        w.edge((ca << 12) | 0x32, HEADERS[index]);
        w.emit(&[0x1201, 0x0112, 0x2101]);
        w.edge((ba << 12) | 0x32, EXITS[index]);
        w.jump(HEADERS[index]);
        w.label(EXITS[index]);
    }
    w.emit(&[0x010f]);
    out.push(plain("restoredFloat", vec!["I"; 5], "F", 4, w));
    out
}
#[test]
fn disjoint_loops_preserve_regions_liveouts_and_links() {
    assert_eq!(cases().len(), 14);
    for c in cases() {
        let class = fixture(&c);
        let code = native_java::render_method("sample.DisjointLoops", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", c.name));
        let count = if matches!(
            c.name,
            "threeSequential" | "onlyThirdBreak" | "alternativeThird" | "effects" | "headerValues"
        ) {
            3
        } else {
            2
        };
        assert_eq!(
            code.source.matches("while (").count(),
            count,
            "{}: {}",
            c.name,
            code.source
        );
        if c.hooks {
            for name in ["h1", "h2", "h3"] {
                assert!(
                    code.links
                        .iter()
                        .any(|l| l.label == format!("sample.Hook.{name}(I)I"))
                );
            }
        }
    }
}
#[test]
fn malformed_per_loop_operands_and_edges_fail_closed() {
    for index in 0..3 {
        for offset in [1, 999] {
            let mut c = cases()
                .into_iter()
                .find(|c| c.name == "threeSequential")
                .unwrap();
            let pc = c
                .words
                .iter()
                .enumerate()
                .filter(|(_, x)| matches!(**x, 0x5032 | 0x6032))
                .map(|(pc, _)| pc)
                .nth(index * 2)
                .unwrap();
            c.words[pc + 1] = offset;
            assert!(render(&c).is_err());
        }
    }
    let mut c = cases().remove(0);
    let pc = c.words.iter().rposition(|x| *x == 0x5032).unwrap();
    c.words[pc] = 0xf032;
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    let pc = c.words.iter().rposition(|x| *x == 0x5032).unwrap();
    c.words.truncate(pc + 1);
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    c.parameters[4] = "Ljava/lang/Object;";
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    c.words[0] = 0;
    assert!(render(&c).is_err());
    let mut c = cases().into_iter().find(|c| c.name == "wideCross").unwrap();
    let pc = c.words.iter().rposition(|x| *x == 0x1504).unwrap();
    c.words[pc] = 0xf504;
    assert!(render(&c).is_err());
    let mut c = cases()
        .into_iter()
        .find(|c| c.name == "headerValues")
        .unwrap();
    c.hooks = false;
    assert!(render(&c).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn disjoint_loops_jvm_matches_cross_loop_values_and_exception_prefixes() {
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
static int h1(int i){{mark('H',i);return 100+i;}}static int h2(int i){{mark('J',i);return 200+i;}}static int h3(int i){{mark('K',i);return 300+i;}}
static int a(int i){{mark('A',i);return 3;}}static int b(int i){{mark('B',i);return 5;}}static int c(int i){{mark('C',i);return 7;}}
static int tail(int s,int h){{mark('T',h-300);return s+h+1000;}}
}}
public class DisjointLoops{{
{methods}
static int[] scalar(int limit,int ca,int ba,int mode,int index,boolean hasBreak){{int n=Math.max(0,limit),sum=0,counter=n;
for(int k=1;k<=n;k++){{sum+=new int[]{{3,5,7}}[index];if(k==ca)continue;sum+=new int[]{{2,4,6}}[index];if(hasBreak&&k==ba){{counter=k;break;}}sum+=mode==0?1:2;}}return new int[]{{sum,counter}};}}
static int swaps(int limit,int ca,int ba,boolean second){{int n=Math.max(0,limit),s=0;for(int k=1;k<=n;k++){{s++;if(k==ca)continue;if(k==ba)break;if(second)s++;}}return s;}}
static void effectsCheck(int l1,int l2,int l3,int ca,int ba,int mode,char failure,int at,boolean valuesOnly){{
int sum=0,h=100;java.util.ArrayList<String> events=new java.util.ArrayList<>();int[] limits={{l1,l2,l3}};
for(int index=0;index<3;index++){{if(index==0&&mode<0||index==1&&mode>0)continue;
int n=Math.max(0,limits[index]),cont=index==1?ba:ca,brk=index==1?ca:ba;char header="HJK".charAt(index),body="ABC".charAt(index);int value=new int[]{{3,5,7}}[index];
for(int i=0;i<=n;i++){{h=100*(index+1)+i;events.add(""+header+i+";");if(i==n)break;
if(!valuesOnly){{events.add(""+body+i+";");sum+=value;}}int k=i+1;if(k==cont)continue;
if(!valuesOnly){{events.add(""+body+k+";");sum+=value;}}if(k==brk)break;
}}}}
if(!valuesOnly)events.add("T"+(h-300)+";");StringBuilder expected=new StringBuilder();boolean fails=false;
for(String e:events){{expected.append(e);if(e.charAt(0)==failure&&(failure=='T'||Integer.parseInt(e.substring(1,e.length()-1))==at)){{fails=true;break;}}}}
Hook.trace="";Hook.failure=failure;Hook.failAt=at;
try{{int actual=valuesOnly?headerValues(l1,l2,l3,ca,ba,mode):effects(l1,l2,l3,ca,ba,mode);if(fails||actual!=(valuesOnly?h:sum+h+1000))throw new AssertionError("effects result");}}
catch(RuntimeException actual){{if(!fails||actual!=Hook.sentinel)throw new AssertionError("exception identity",actual);}}
if(!Hook.trace.equals(expected.toString()))throw new AssertionError("loop effect trace "+Hook.trace+" expected "+expected);
}}
public static void main(String[]args){{Object left=new Object(),right=new Object();
for(int l1:new int[]{{-1,0,1,3}})for(int l2:new int[]{{-1,0,1,3}})for(int l3:new int[]{{-1,0,1,3}})for(int ca:new int[]{{-1,1,3}})for(int ba:new int[]{{-1,1,3}})for(int mode:new int[]{{-1,0,1}}){{
int[] a=scalar(l1,ca,ba,mode,0,true),b=scalar(l2,ba,ca,mode,1,true),c=scalar(l3,ca,ba,mode,2,true);
if(twoSequential(l1,l2,l3,ca,ba,mode)!=a[0]+b[0]+11)throw new AssertionError("two sequential");
if(threeSequential(l1,l2,l3,ca,ba,mode)!=a[0]+b[0]+c[0]+11)throw new AssertionError("three sequential");
if(alternative(l1,l2,l3,ca,ba,mode)!=(mode==0?b[0]:a[0])+11)throw new AssertionError("alternative ownership");
if(alternativeThird(l1,l2,l3,ca,ba,mode)!=(mode==0?b[0]:a[0])+c[0]+11)throw new AssertionError("alternative merge then third");
if(bypass(l1,l2,l3,ca,ba,mode)!=(mode<0?0:a[0])+(mode>0?0:b[0])+11)throw new AssertionError("independent bypass");
if(noBreakFirst(l1,l2,l3,ca,ba,mode)!=scalar(l1,ca,ba,mode,0,false)[0]+b[0]+11)throw new AssertionError("second break owner");
if(noBreakSecond(l1,l2,l3,ca,ba,mode)!=a[0]+scalar(l2,ba,ca,mode,1,false)[0]+11)throw new AssertionError("first break owner");
if(onlyThirdBreak(l1,l2,l3,ca,ba,mode)!=scalar(l1,ca,ba,mode,0,false)[0]+scalar(l2,ba,ca,mode,1,false)[0]+c[0]+11)throw new AssertionError("third break owner");
if(dependentBound(l1,l2,l3,ca,ba,mode)!=a[0]+scalar(a[1],ba,ca,mode,1,true)[0]+11)throw new AssertionError("later bound liveout");
int count=swaps(l1,ca,ba,false)+swaps(l2,ba,ca,true);
for(long x:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})for(long y:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})
if(wideCross(l1,l2,ca,ba,mode,x,y)!=((count&1)==0?x:y))throw new AssertionError("cross-loop wide snapshot");
for(Object x:new Object[]{{null,left,right}})for(Object y:new Object[]{{null,left,right}})
if(referenceCross(l1,l2,ca,ba,mode,x,y)!=((count&1)==0?x:y))throw new AssertionError("cross-loop reference identity");
if(Float.floatToRawIntBits(restoredFloat(l1,l2,ca,ba,mode))!=0x7fc12345)throw new AssertionError("per-loop restored raw literal");
effectsCheck(l1,l2,l3,ca,ba,mode,(char)0,0,true);
for(char failure:new char[]{{0,'H','J','K','A','B','C','T'}})for(int at:new int[]{{-1,0,1,2,3}})effectsCheck(l1,l2,l3,ca,ba,mode,failure,at,false);
}}
}}
}}
"#
    );
    let dir =
        std::env::temp_dir().join(format!("rdx-shared-disjoint-loops-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/DisjointLoops.java"), java).unwrap();
    for (program, arg) in [
        ("javac", "sample/DisjointLoops.java"),
        ("java", "sample.DisjointLoops"),
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
            dir.join("sample/DisjointLoops.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
