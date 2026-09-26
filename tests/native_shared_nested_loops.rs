//! G01-C-nested-loop-composition: parent-owned nested loops.
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
    patches: Vec<(usize, &'static str)>,
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
        self.patches.push((pc, target));
    }
    fn branch(&mut self, register: u16, opcode: u16, target: &'static str) {
        let pc = self.words.len();
        self.emit(&[(register << 8) | opcode, 0]);
        self.patches.push((pc, target));
    }
    fn jump(&mut self, target: &'static str) {
        let pc = self.words.len();
        self.emit(&[0x29, 0]);
        self.patches.push((pc, target));
    }
    fn inc(&mut self, register: u16) {
        self.emit(&[(register << 8) | 0xd8, 0x0100 | register]);
    }
    fn sum(&mut self, value: u16) {
        self.emit(&[0x03d8, (value << 8) | 3]);
    }
    fn finish(mut self) -> Vec<u16> {
        for (pc, target) in self.patches {
            let delta = self.labels[target] as isize - pc as isize;
            assert!((-32768..32768).contains(&delta) && delta != 0);
            self.words[pc + 1] = delta as i16 as u16;
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
    Ok(native_java::render_method("sample.NestedLoops", &class, &class.methods[0])?.source)
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
const H: [&str; 3] = ["outer", "inner", "deep"];
const E: [&str; 3] = ["outerExit", "innerExit", "deepExit"];
fn scalar_loop(w: &mut Words, index: usize, depth: usize, name: &str) {
    w.emit(&[((index as u16) << 8) | 0x12]);
    w.label(H[index]);
    w.edge(
        ((4 + index as u16) << 12) | ((index as u16) << 8) | 0x35,
        E[index],
    );
    w.sum([3, 5, 7][index]);
    if index + 1 < depth {
        if name == "skipChild" && index == 0 {
            w.branch(9, 0x3c, "skipChild");
        }
        scalar_loop(w, index + 1, depth, name);
        if name == "siblings" && index == 0 {
            scalar_loop(w, 2, 3, name);
        }
        if name == "skipChild" && index == 0 {
            w.label("skipChild");
        }
    }
    if name == "changedOuter" && index == 1 {
        w.inc(0);
    }
    w.inc(index as u16);
    let (ca, ba) = if index == 1 { (8, 7) } else { (7, 8) };
    w.edge((ca << 12) | ((index as u16) << 8) | 0x32, H[index]);
    w.sum([2, 4, 6][index]);
    if !(name == "onlyInnerBreak" && index == 0 || name == "onlyOuterBreak" && index == 1) {
        w.edge((ba << 12) | ((index as u16) << 8) | 0x32, E[index]);
    }
    w.sum([1, 2, 3][index]);
    w.jump(H[index]);
    w.label(E[index]);
}
fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for name in [
        "depthTwo",
        "depthThree",
        "siblings",
        "skipChild",
        "onlyInnerBreak",
        "onlyOuterBreak",
        "changedOuter",
    ] {
        let mut w = Words::default();
        w.emit(&[0x0312]);
        scalar_loop(&mut w, 0, if name == "depthThree" { 3 } else { 2 }, name);
        w.emit(&[0x030f]);
        out.push(plain(name, vec!["I"; 6], "I", 4, w));
    }
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0312]);
    w.label("outer");
    w.edge(0x4035, "outerExit");
    w.sum(3);
    w.inc(0);
    scalar_loop(&mut w, 1, 2, "latchAdjacent");
    w.jump("outer");
    w.label("outerExit");
    w.emit(&[0x030f]);
    out.push(plain("latchAdjacent", vec!["I"; 6], "I", 4, w));
    for wide in [true, false] {
        let mut w = Words::default();
        let first = if wide { 8 } else { 5 };
        let swap = if wide {
            vec![0x2604, 0x4204, 0x6404]
        } else {
            vec![0x2407, 0x3207, 0x4307]
        };
        if wide {
            w.emit(&[0xd204, 0xf404]);
        } else {
            w.emit(&[0xa207, 0xb307]);
        }
        w.emit(&[0x0012]);
        w.label("outer");
        w.edge((first << 12) | 0x35, "outerExit");
        w.emit(&swap);
        w.emit(&[0x0112]);
        w.label("inner");
        w.edge(((first + 1) << 12) | 0x135, "innerExit");
        w.emit(&swap);
        w.inc(1);
        w.edge(((first + 3) << 12) | 0x132, "inner");
        w.emit(&swap);
        w.edge(((first + 2) << 12) | 0x132, "innerExit");
        w.jump("inner");
        w.label("innerExit");
        w.inc(0);
        w.edge(((first + 2) << 12) | 0x32, "outer");
        w.emit(&swap);
        w.edge(((first + 3) << 12) | 0x32, "outerExit");
        w.jump("outer");
        w.label("outerExit");
        w.emit(&[if wide { 0x0210 } else { 0x0211 }]);
        let mut p = vec!["I"; 5];
        p.extend(if wide {
            vec!["J", "J"]
        } else {
            vec!["Ljava/lang/Object;", "Ljava/lang/Object;"]
        });
        out.push(plain(
            if wide {
                "wideNested"
            } else {
                "referenceNested"
            },
            p,
            if wide { "J" } else { "Ljava/lang/Object;" },
            first,
            w,
        ));
    }
    for depth in [2, 3] {
        let mut w = Words::default();
        w.emit(&[0x0312]);
        effects_loop(&mut w, 0, depth);
        w.emit(&[0x2071, 6, 0x0053, 0x000a, 0x000f]);
        let mut c = plain(
            if depth == 2 {
                "effectsTwo"
            } else {
                "effectsThree"
            },
            vec!["I"; 6],
            "I",
            8,
            w,
        );
        c.hooks = true;
        out.push(c);
    }
    for float in [false, true] {
        let mut w = Words::default();
        if float {
            w.emit(&[0x0314, 0x2345, 0x7fc1]);
        } else {
            w.emit(&[0x0312]);
        }
        literal_loop(&mut w, 0, float);
        if float {
            w.emit(&[0x030f]);
        } else {
            w.emit(&[0x031f, 0, 0x0311]);
        }
        let mut c = plain(
            if float { "restoredFloat" } else { "headerNull" },
            vec!["I"; 6],
            if float { "F" } else { "Ljava/lang/String;" },
            5,
            w,
        );
        if !float {
            c.types = vec!["Ljava/lang/String;"];
        }
        out.push(c);
    }
    let mut w = Words::default();
    w.emit(&[0x0412]);
    depth_four(&mut w, 0);
    w.emit(&[0x040f]);
    out.push(plain("depthFour", vec!["I"; 4], "I", 5, w));
    out
}
fn depth_four(w: &mut Words, index: usize) {
    let headers = ["four1", "four2", "four3", "four4"];
    let exits = ["fourExit1", "fourExit2", "fourExit3", "fourExit4"];
    w.emit(&[((index as u16) << 8) | 0x12]);
    w.label(headers[index]);
    w.edge(
        ((5 + index as u16) << 12) | ((index as u16) << 8) | 0x35,
        exits[index],
    );
    w.emit(&[0x04d8, (((index + 1) as u16) << 8) | 4]);
    if index < 3 {
        depth_four(w, index + 1);
    }
    w.inc(index as u16);
    w.jump(headers[index]);
    w.label(exits[index]);
}
fn effects_loop(w: &mut Words, index: usize, depth: usize) {
    w.emit(&[((index as u16) << 8) | 0x12]);
    w.label(H[index]);
    w.emit(&[
        0x1071,
        index as u16,
        index as u16,
        ((5 + index as u16) << 8) | 0x0a,
    ]);
    w.edge(
        ((8 + index as u16) << 12) | ((index as u16) << 8) | 0x35,
        E[index],
    );
    w.emit(&[
        0x1071,
        3 + index as u16,
        index as u16,
        0x040a,
        0x0390,
        0x0403,
    ]);
    if index + 1 < depth {
        if index == 0 {
            w.branch(13, 0x3c, "skipChild");
        }
        effects_loop(w, index + 1, depth);
        if index == 0 {
            w.label("skipChild");
        }
    }
    w.inc(index as u16);
    let (ca, ba) = if index == 1 { (12, 11) } else { (11, 12) };
    w.edge((ca << 12) | ((index as u16) << 8) | 0x32, H[index]);
    w.emit(&[
        0x1071,
        3 + index as u16,
        index as u16,
        0x040a,
        0x0390,
        0x0403,
    ]);
    w.edge((ba << 12) | ((index as u16) << 8) | 0x32, E[index]);
    w.jump(H[index]);
    w.label(E[index]);
}
fn literal_loop(w: &mut Words, index: usize, float: bool) {
    w.emit(&[((index as u16) << 8) | 0x12]);
    w.label(H[index]);
    if !float {
        w.emit(&[0x0312]);
    }
    w.edge(
        ((5 + index as u16) << 12) | ((index as u16) << 8) | 0x35,
        E[index],
    );
    if float {
        w.emit(&[0x3401, 0x0312, 0x4301]);
    }
    if index == 0 {
        literal_loop(w, 1, float);
    }
    w.inc(index as u16);
    let (ca, ba) = if index == 1 { (9, 8) } else { (8, 9) };
    w.edge((ca << 12) | ((index as u16) << 8) | 0x32, H[index]);
    if float {
        w.emit(&[0x3401, 0x0312, 0x4301]);
    }
    w.edge((ba << 12) | ((index as u16) << 8) | 0x32, E[index]);
    w.jump(H[index]);
    w.label(E[index]);
}
#[test]
fn nested_loops_preserve_owned_regions_liveouts_and_links() {
    assert_eq!(cases().len(), 15);
    for c in cases() {
        let class = fixture(&c);
        let code = native_java::render_method("sample.NestedLoops", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", c.name));
        let count = if c.name == "depthFour" {
            4
        } else if matches!(c.name, "depthThree" | "siblings" | "effectsThree") {
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
            assert!(code.links.iter().any(|l| l.label == "sample.Hook.h1(I)I"));
        }
        if c.name == "headerNull" {
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
fn malformed_nested_edges_and_owned_operands_fail_closed() {
    for opcode in [0x4035, 0x5135, 0x6235] {
        for offset in [1, 999] {
            let mut c = cases()
                .into_iter()
                .find(|c| c.name == "depthThree")
                .unwrap();
            let pc = c.words.iter().position(|x| *x == opcode).unwrap();
            c.words[pc + 1] = offset;
            assert!(render(&c).is_err());
        }
    }
    let mut c = cases().remove(0);
    let pc = c.words.iter().position(|x| *x == 0x8132).unwrap();
    c.words[pc] = 0xf132;
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    let pc = c.words.iter().position(|x| *x == 0x7132).unwrap();
    c.words.truncate(pc + 1);
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    c.parameters[4] = "Ljava/lang/Object;";
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    c.words[0] = 0;
    assert!(render(&c).is_err());
    let mut c = cases()
        .into_iter()
        .find(|c| c.name == "headerNull")
        .unwrap();
    c.types.clear();
    assert!(render(&c).is_err());
    let mut c = cases()
        .into_iter()
        .find(|c| c.name == "wideNested")
        .unwrap();
    let pc = c.words.iter().rposition(|x| *x == 0x2604).unwrap();
    c.words[pc] = 0x0605;
    c.words[pc + 1] = 16;
    assert!(render(&c).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn nested_loops_jvm_matches_parent_child_values_and_exception_prefixes() {
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
static int tail(int s,int h){{mark('T',h-100);return s+h+1000;}}
}}
public class NestedLoops{{
{methods}
static void scalarRun(String name,int index,int depth,int[] limits,int ca,int ba,int mode,int[] state){{
state[index]=0;int cont=index==1?ba:ca,brk=index==1?ca:ba;
while(state[index]<Math.max(0,limits[index])){{state[3]+=new int[]{{3,5,7}}[index];
if(index+1<depth&&!(name.equals("skipChild")&&index==0&&mode>0)){{scalarRun(name,index+1,depth,limits,ca,ba,mode,state);if(name.equals("siblings")&&index==0)scalarRun(name,2,3,limits,ca,ba,mode,state);}}
if(name.equals("changedOuter")&&index==1)state[0]++;state[index]++;
if(state[index]==cont)continue;state[3]+=new int[]{{2,4,6}}[index];
if(!(name.equals("onlyInnerBreak")&&index==0||name.equals("onlyOuterBreak")&&index==1)&&state[index]==brk)break;
state[3]+=new int[]{{1,2,3}}[index];
}}}}
static int scalar(String name,int l1,int l2,int l3,int ca,int ba,int mode){{int[] state=new int[4],limits={{l1,l2,l3}};
if(name.equals("latchAdjacent")){{while(state[0]<Math.max(0,l1)){{state[3]+=3;state[0]++;scalarRun(name,1,2,limits,ca,ba,mode,state);}}}}
else scalarRun(name,0,name.equals("depthThree")?3:2,limits,ca,ba,mode,state);return state[3];}}
static int swaps(int l1,int l2,int ca,int ba){{int count=0;for(int outer=1;outer<=Math.max(0,l1);outer++){{count++;
for(int inner=1;inner<=Math.max(0,l2);inner++){{count++;if(inner==ba)continue;count++;if(inner==ca)break;}}
if(outer==ca)continue;count++;if(outer==ba)break;
}}return count;}}
static void effectEvents(int index,int depth,int[] limits,int ca,int ba,int mode,java.util.ArrayList<String> events,int[] state){{
int n=Math.max(0,limits[index]),cont=index==1?ba:ca,brk=index==1?ca:ba;char header="HJK".charAt(index),body="ABC".charAt(index);int value=new int[]{{3,5,7}}[index];
for(int i=0;i<=n;i++){{if(index==0)state[1]=100+i;events.add(""+header+i+";");if(i==n)break;
events.add(""+body+i+";");state[0]+=value;
if(index+1<depth&&!(index==0&&mode>0))effectEvents(index+1,depth,limits,ca,ba,mode,events,state);
int k=i+1;if(k==cont)continue;events.add(""+body+k+";");state[0]+=value;if(k==brk)break;
}}}}
static void checkEffects(int depth,int l1,int l2,int l3,int ca,int ba,int mode,char failure,int at){{
java.util.ArrayList<String> events=new java.util.ArrayList<>();int[] state=new int[2];effectEvents(0,depth,new int[]{{l1,l2,l3}},ca,ba,mode,events,state);events.add("T"+(state[1]-100)+";");
StringBuilder expected=new StringBuilder();boolean fails=false;for(String e:events){{expected.append(e);if(e.charAt(0)==failure&&(failure=='T'||Integer.parseInt(e.substring(1,e.length()-1))==at)){{fails=true;break;}}}}
Hook.trace="";Hook.failure=failure;Hook.failAt=at;
try{{int actual=depth==2?effectsTwo(l1,l2,l3,ca,ba,mode):effectsThree(l1,l2,l3,ca,ba,mode);if(fails||actual!=state[0]+state[1]+1000)throw new AssertionError("effects result");}}
catch(RuntimeException actual){{if(!fails||actual!=Hook.sentinel)throw new AssertionError("exception identity",actual);}}
if(!Hook.trace.equals(expected.toString()))throw new AssertionError("nested effect trace "+Hook.trace+" expected "+expected);
}}
public static void main(String[]args){{Object left=new Object(),right=new Object();
for(int n1=0;n1<=2;n1++)for(int n2=0;n2<=2;n2++)for(int n3=0;n3<=2;n3++)for(int n4=0;n4<=2;n4++){{
int expected=0;for(int i=0;i<n1;i++){{expected++;for(int j=0;j<n2;j++){{expected+=2;for(int k=0;k<n3;k++){{expected+=3;for(int l=0;l<n4;l++)expected+=4;}}}}}}
if(depthFour(n1,n2,n3,n4)!=expected)throw new AssertionError("depth4 bounded semantics");
}}
for(int l1:new int[]{{-1,0,1,3}})for(int l2:new int[]{{-1,0,1,3}})for(int l3:new int[]{{-1,0,1,3}})for(int ca:new int[]{{-1,1,3}})for(int ba:new int[]{{-1,1,3}})for(int mode:new int[]{{-1,0,1}}){{
if(depthTwo(l1,l2,l3,ca,ba,mode)!=scalar("depthTwo",l1,l2,l3,ca,ba,mode))throw new AssertionError("depth2");
if(depthThree(l1,l2,l3,ca,ba,mode)!=scalar("depthThree",l1,l2,l3,ca,ba,mode))throw new AssertionError("depth3");
if(siblings(l1,l2,l3,ca,ba,mode)!=scalar("siblings",l1,l2,l3,ca,ba,mode))throw new AssertionError("sibling child liveout");
if(skipChild(l1,l2,l3,ca,ba,mode)!=scalar("skipChild",l1,l2,l3,ca,ba,mode))throw new AssertionError("skipped child");
if(onlyInnerBreak(l1,l2,l3,ca,ba,mode)!=scalar("onlyInnerBreak",l1,l2,l3,ca,ba,mode))throw new AssertionError("inner break owner");
if(onlyOuterBreak(l1,l2,l3,ca,ba,mode)!=scalar("onlyOuterBreak",l1,l2,l3,ca,ba,mode))throw new AssertionError("outer break owner");
if(changedOuter(l1,l2,l3,ca,ba,mode)!=scalar("changedOuter",l1,l2,l3,ca,ba,mode))throw new AssertionError("child changed parent counter");
if(latchAdjacent(l1,l2,l3,ca,ba,mode)!=scalar("latchAdjacent",l1,l2,l3,ca,ba,mode))throw new AssertionError("child exit parent latch");
int count=swaps(l1,l2,ca,ba);
for(long a:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})for(long b:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})
if(wideNested(l1,l2,ca,ba,mode,a,b)!=((count&1)==0?a:b))throw new AssertionError("nested wide snapshot");
for(Object a:new Object[]{{null,left,right}})for(Object b:new Object[]{{null,left,right}})
if(referenceNested(l1,l2,ca,ba,mode,a,b)!=((count&1)==0?a:b))throw new AssertionError("nested reference identity");
if(headerNull(l1,l2,l3,ca,ba,mode)!=null)throw new AssertionError("nested null");
if(Float.floatToRawIntBits(restoredFloat(l1,l2,l3,ca,ba,mode))!=0x7fc12345)throw new AssertionError("nested restored float");
for(int depth:new int[]{{2,3}})for(char failure:new char[]{{0,'H','J','K','A','B','C','T'}})for(int at:new int[]{{-1,0,1,2,3}})checkEffects(depth,l1,l2,l3,ca,ba,mode,failure,at);
}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-nested-loops-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/NestedLoops.java"), java).unwrap();
    for (program, arg) in [
        ("javac", "sample/NestedLoops.java"),
        ("java", "sample.NestedLoops"),
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
            dir.join("sample/NestedLoops.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
