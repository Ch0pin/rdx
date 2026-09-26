//! G01-C-posttest-loop: one straightline body with a conditional backward latch.
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
    fn increment(&mut self) {
        self.emit(&[0x00d8, 0x0100]);
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
            strings: ["header", "a", "b", "test", "tail"]
                .into_iter()
                .map(Into::into)
                .collect(),
            types: vec!["Lsample/Hook;".into()],
            protos: vec![
                ("I".into(), vec!["I".into()]),
                ("I".into(), vec!["I".into(), "I".into()]),
                ("Z".into(), vec!["I".into(), "I".into()]),
            ],
            methods: vec![(0, 0, 0), (0, 0, 1), (0, 0, 2), (0, 2, 3), (0, 1, 4)],
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
    Ok(native_java::render_method("sample.PosttestLoops", &class, &class.methods[0])?.source)
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
    w.emit(&[0x0012]);
    w.label("body");
    w.increment();
    w.edge(0x1034, "body");
    w.emit(&[0x000f]);
    out.push(plain("mandatorySigned", vec!["I"], "I", 1, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x3207]);
    w.label("body");
    w.emit(&[0x2107, 0x0212]);
    w.increment();
    w.edge(0x0139, "body");
    w.emit(&[0x000f]);
    out.push(plain(
        "referenceNull",
        vec!["Ljava/lang/Object;"],
        "I",
        3,
        w,
    ));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x4207]);
    w.label("body");
    w.emit(&[0x2107, 0x3207]);
    w.increment();
    w.edge(0x3133, "body");
    w.emit(&[0x000f]);
    out.push(plain(
        "referenceIdentity",
        vec!["Ljava/lang/Object;", "Ljava/lang/Object;"],
        "I",
        3,
        w,
    ));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x3201]);
    w.label("body");
    w.emit(&[0x2101, 0x0212]);
    w.increment();
    w.edge(0x0139, "body");
    w.emit(&[0x000f]);
    out.push(plain("booleanZero", vec!["Z"], "I", 3, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x4201]);
    w.label("body");
    w.emit(&[0x2101, 0x02df, 0x0103]);
    w.increment();
    w.edge(0x3132, "body");
    w.emit(&[0x000f]);
    out.push(plain("booleanPair", vec!["Z", "Z"], "I", 3, w));
    let mut w = Words::default();
    w.emit(&[0x3001, 0x4101]);
    w.label("body");
    w.emit(&[0x0201, 0x1001, 0x2101]);
    w.edge(0x1034, "body");
    w.emit(&[0x0091, 0x0100, 0x000f]);
    out.push(plain("snapshotLatch", vec!["I", "I"], "I", 3, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x8104, 0xa304]);
    w.label("body");
    w.emit(&[0x1504, 0x3104, 0x5304]);
    w.increment();
    w.edge(0x7034, "body");
    w.emit(&[0x0110]);
    out.push(plain("wideCarry", vec!["I", "J", "J"], "J", 7, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x5107, 0x6207]);
    w.label("body");
    w.emit(&[0x1307, 0x2107, 0x3207]);
    w.increment();
    w.edge(0x4034, "body");
    w.emit(&[0x0111]);
    out.push(plain(
        "referenceCarry",
        vec!["I", "Ljava/lang/Object;", "Ljava/lang/Object;"],
        "Ljava/lang/Object;",
        4,
        w,
    ));
    for ty in ["I", "J", "Ljava/lang/Object;", "Ljava/lang/String;", "F"] {
        let mut w = Words::default();
        w.emit(&[0x0012]);
        w.label("body");
        let locals = if ty == "J" { 3 } else { 2 };
        match ty {
            "I" => w.emit(&[0x0113, 42]),
            "J" => w.emit(&[0x4104]),
            "Ljava/lang/Object;" => w.emit(&[0x3107]),
            "Ljava/lang/String;" => w.emit(&[0x0112]),
            "F" => w.emit(&[0x0114, 0x2345, 0x7fc1]),
            _ => unreachable!(),
        };
        w.increment();
        w.edge((locals << 12) | 0x34, "body");
        match ty {
            "J" => w.emit(&[0x0110]),
            "Ljava/lang/Object;" => w.emit(&[0x0111]),
            "Ljava/lang/String;" => w.emit(&[0x011f, 0, 0x0111]),
            _ => w.emit(&[0x010f]),
        };
        let (name, params) = match ty {
            "I" => ("exitInt", vec!["I"]),
            "J" => ("exitWide", vec!["I", "J"]),
            "Ljava/lang/Object;" => ("exitReference", vec!["I", "Ljava/lang/Object;"]),
            "Ljava/lang/String;" => ("exitNull", vec!["I"]),
            _ => ("exitFloat", vec!["I"]),
        };
        let mut c = plain(name, params, ty, locals, w);
        if ty == "Ljava/lang/String;" {
            c.types = vec![ty];
        }
        out.push(c);
    }
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("body");
    w.emit(&[0x1071, 0, 0, 0x020a, 0x1071, 1, 0, 0x030a, 0x0190, 0x0301]);
    w.increment();
    w.emit(&[
        0x1071, 2, 0, 0x030a, 0x0190, 0x0301, 0x2071, 3, 0x0040, 0x030a,
    ]);
    w.edge(0x0339, "body");
    w.emit(&[0x2071, 4, 0x0021, 0x000a, 0x000f]);
    let mut c = plain("effects", vec!["I"], "I", 4, w);
    c.hooks = true;
    out.push(c);
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0114, 0x2345, 0x7fc1]);
    w.label("body");
    w.emit(&[0x1201, 0x0112, 0x2101]);
    w.increment();
    w.edge(0x3034, "body");
    w.emit(&[0x010f]);
    out.push(plain("restoredFloat", vec!["I"], "F", 3, w));
    out
}
#[test]
fn posttest_loops_preserve_first_iteration_exit_only_values_and_links() {
    assert_eq!(cases().len(), 15);
    for c in cases() {
        let class = fixture(&c);
        let code = native_java::render_method("sample.PosttestLoops", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", c.name));
        assert_eq!(
            code.source.matches("while (").count(),
            1,
            "{}: {}",
            c.name,
            code.source
        );
        if c.hooks {
            for label in ["sample.Hook.header(I)I", "sample.Hook.test(II)Z"] {
                assert!(code.links.iter().any(|l| l.label == label));
            }
        }
        if c.name == "exitNull" {
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
fn malformed_posttest_conditions_and_exit_operands_fail_closed() {
    for offset in [0, 1, 999, 0xffff] {
        let mut c = cases().remove(0);
        let pc = c.words.iter().position(|x| *x == 0x1034).unwrap();
        c.words[pc + 1] = offset;
        assert!(render(&c).is_err(), "offset {offset}");
    }
    let mut c = cases().remove(0);
    let pc = c.words.iter().position(|x| *x == 0x1034).unwrap();
    c.words[pc] = 0xf034;
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    let pc = c.words.iter().position(|x| *x == 0x1034).unwrap();
    c.words.truncate(pc + 1);
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    c.parameters[0] = "Ljava/lang/Object;";
    assert!(render(&c).is_err());
    let mut c = cases().into_iter().find(|c| c.name == "exitWide").unwrap();
    let pc = c.words.iter().position(|x| *x == 0x4104).unwrap();
    c.words[pc] = 0x5104;
    assert!(render(&c).is_err());
    let mut c = cases().into_iter().find(|c| c.name == "exitNull").unwrap();
    c.types.clear();
    assert!(render(&c).is_err());
    let mut c = cases()
        .into_iter()
        .find(|c| c.name == "referenceIdentity")
        .unwrap();
    let pc = c.words.iter().position(|x| *x == 0x3133).unwrap();
    c.words[pc] = 0x3134;
    assert!(render(&c).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn posttest_jvm_matches_mandatory_iterations_snapshots_and_exception_prefixes() {
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
static int header(int i){{mark('H',i);return 100+i;}}static int a(int i){{mark('A',i);return 3;}}static int b(int i){{mark('B',i);return 5;}}
static boolean test(int i,int limit){{mark('C',i);return i<limit;}}static int tail(int s,int h){{mark('T',h-100);return s+h+1000;}}
}}
public class PosttestLoops{{
{methods}
static void checkEffects(int limit,char failure,int at){{int n=Math.max(1,limit);java.util.ArrayList<String> events=new java.util.ArrayList<>();for(int i=0;i<n;i++){{events.add("H"+i+";");events.add("A"+i+";");events.add("B"+(i+1)+";");events.add("C"+(i+1)+";");}}events.add("T"+(n-1)+";");
StringBuilder expected=new StringBuilder();boolean fails=false;for(String e:events){{expected.append(e);if(e.charAt(0)==failure&&(failure=='T'||Integer.parseInt(e.substring(1,e.length()-1))==at)){{fails=true;break;}}}}
Hook.trace="";Hook.failure=failure;Hook.failAt=at;try{{int result=effects(limit);if(fails||result!=8*n+1100+n-1)throw new AssertionError("effect result");}}catch(RuntimeException actual){{if(!fails||actual!=Hook.sentinel)throw new AssertionError("exception identity",actual);}}if(!Hook.trace.equals(expected.toString()))throw new AssertionError("posttest trace "+Hook.trace+" expected "+expected);
}}
public static void main(String[]args){{Object left=new Object(),right=new Object();
for(int limit:new int[]{{Integer.MIN_VALUE,-7,-1,0,1,2,7,25}}){{int n=Math.max(1,limit);
if(mandatorySigned(limit)!=n)throw new AssertionError("mandatory signed first");
if(exitInt(limit)!=42)throw new AssertionError("body-only int");
if(exitNull(limit)!=null)throw new AssertionError("body-only null");
if(Float.floatToRawIntBits(exitFloat(limit))!=0x7fc12345)throw new AssertionError("body-only float");
if(Float.floatToRawIntBits(restoredFloat(limit))!=0x7fc12345)throw new AssertionError("restored raw float");
for(long x:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}}){{if(exitWide(limit,x)!=x)throw new AssertionError("body-only wide");for(long y:new long[]{{Long.MIN_VALUE,-1L,0L,Long.MAX_VALUE}})if(wideCarry(limit,x,y)!=((n&1)==0?x:y))throw new AssertionError("wide snapshot");}}
for(Object x:new Object[]{{null,left,right}}){{if(exitReference(limit,x)!=x)throw new AssertionError("body-only reference");for(Object y:new Object[]{{null,left,right}})if(referenceCarry(limit,x,y)!=((n&1)==0?x:y))throw new AssertionError("reference snapshot");}}
for(char failure:new char[]{{0,'H','A','B','C','T'}})for(int at:new int[]{{-1,0,1,2,6,7,24,25}})checkEffects(limit,failure,at);
}}
for(Object x:new Object[]{{null,left,right}}){{if(referenceNull(x)!=(x==null?1:2))throw new AssertionError("ref-null first");for(Object y:new Object[]{{null,left,right}})if(referenceIdentity(x,y)!=(x==y?1:2))throw new AssertionError("ref identity first");}}
for(boolean x:new boolean[]{{false,true}}){{if(booleanZero(x)!=(x?2:1))throw new AssertionError("bool-zero first");for(boolean y:new boolean[]{{false,true}})if(booleanPair(x,y)!=(x==y?2:1))throw new AssertionError("bool-pair first");}}
for(int a:new int[]{{Integer.MIN_VALUE,-1,0,1,Integer.MAX_VALUE}})for(int b:new int[]{{Integer.MIN_VALUE,-1,0,1,Integer.MAX_VALUE}}){{int finalA=b<a?a:b,finalB=b<a?b:a;if(snapshotLatch(a,b)!=finalA-finalB)throw new AssertionError("condition before swapped slots");}}
}}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-posttest-loop-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/PosttestLoops.java"), java).unwrap();
    for (program, arg) in [
        ("javac", "sample/PosttestLoops.java"),
        ("java", "sample.PosttestLoops"),
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
            dir.join("sample/PosttestLoops.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
