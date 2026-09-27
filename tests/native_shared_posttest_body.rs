//! G01-C-posttest-body-composition: forward diamonds inside one conditional backward latch.
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
    fn jump(&mut self, target: &'static str) {
        self.edge(0x29, target);
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
    Ok(native_java::render_method("sample.PosttestBody", &class, &class.methods[0])?.source)
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
    // v0 iteration, v1 carried sum, v2 branch-dependent exit; parameters v3 limit/v4 selector.
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("body");
    w.edge(0x0438, "zero");
    w.emit(&[0x01d8, 0x0301, 0x0213, 17]);
    w.jump("join");
    w.label("zero");
    w.emit(&[0x01d8, 0x0501, 0x0213, 29]);
    w.label("join");
    w.increment();
    w.edge(0x3034, "body");
    w.emit(&[0x0190, 0x0201, 0x010f]);
    out.push(plain("carriedAndExit", vec!["I", "I"], "I", 3, w));
    // A branch that changes after iteration zero exercises mandatory execution and per-iteration joins.
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("body");
    w.edge(0x0038, "first");
    w.emit(&[0x01d8, 0x0701]);
    w.jump("join");
    w.label("first");
    w.emit(&[0x01d8, 0x0b01]);
    w.label("join");
    w.edge(0x0338, "second");
    w.emit(&[0x01d8, 0x0201]);
    w.jump("tail");
    w.label("second");
    w.emit(&[0x01d8, 0x0401]);
    w.label("tail");
    w.increment();
    w.edge(0x2034, "body");
    w.emit(&[0x010f]);
    out.push(plain("sequential", vec!["I", "I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("body");
    w.edge(0x0338, "zero");
    w.emit(&[0x4107]);
    w.jump("join");
    w.label("zero");
    w.emit(&[0x5107]);
    w.label("join");
    w.increment();
    w.edge(0x2034, "body");
    w.emit(&[0x0111]);
    out.push(plain(
        "referenceExit",
        vec!["I", "I", "Ljava/lang/Object;", "Ljava/lang/Object;"],
        "Ljava/lang/Object;",
        2,
        w,
    ));
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("body");
    w.edge(0x0438, "zero");
    w.emit(&[0x5104]);
    w.jump("join");
    w.label("zero");
    w.emit(&[0x7104]);
    w.label("join");
    w.increment();
    w.edge(0x3034, "body");
    w.emit(&[0x0110]);
    out.push(plain("wideExit", vec!["I", "I", "J", "J"], "J", 3, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("body");
    w.emit(&[0x1071, 0, 0, 0x020a]);
    w.edge(0x0038, "first");
    w.emit(&[0x1071, 2, 0, 0x030a]);
    w.jump("join");
    w.label("first");
    w.emit(&[0x1071, 1, 0, 0x030a]);
    w.label("join");
    w.emit(&[0x0190, 0x0301]);
    w.increment();
    w.emit(&[0x2071, 3, 0x0040, 0x030a]);
    w.edge(0x0339, "body");
    w.emit(&[0x2071, 4, 0x0021, 0x000a, 0x000f]);
    let mut c = plain("effects", vec!["I"], "I", 4, w);
    c.hooks = true;
    out.push(c);
    let mut w = Words::default();
    w.emit(&[0x0012]);
    w.label("body");
    w.increment();
    w.edge(0x0338, "zero");
    w.emit(&[0x2101]);
    w.jump("latch");
    w.label("zero");
    w.emit(&[0x0112]);
    w.label("latch");
    w.edge(0x1034, "body");
    w.emit(&[0x000f]);
    out.push(plain("conditionAtJoin", vec!["I", "I"], "I", 2, w));
    let mut w = Words::default();
    w.emit(&[0x0012, 0x0112]);
    w.label("body");
    w.edge(0x0038, "first");
    w.emit(&[0x01d8, 0x0701]);
    w.jump("join");
    w.label("first");
    w.edge(0x0338, "zero");
    w.emit(&[0x01d8, 0x0d01]);
    w.jump("join");
    w.label("zero");
    w.emit(&[0x01d8, 0x0b01]);
    w.label("join");
    w.increment();
    w.edge(0x2034, "body");
    w.emit(&[0x010f]);
    out.push(plain("nested", vec!["I", "I"], "I", 2, w));
    out
}
#[test]
fn forward_diamonds_preserve_posttest_shape_and_links() {
    assert_eq!(cases().len(), 7);
    for c in cases() {
        let class = fixture(&c);
        let code = native_java::render_method("sample.PosttestBody", &class, &class.methods[0])
            .unwrap_or_else(|e| panic!("{}: {e:#}", c.name));
        assert_eq!(
            code.source.matches("while (").count(),
            1,
            "{}: {}",
            c.name,
            code.source
        );
        assert!(code.source.contains("if ("), "{}: {}", c.name, code.source);
        if c.hooks {
            for label in ["sample.Hook.header(I)I", "sample.Hook.test(II)Z"] {
                assert!(code.links.iter().any(|l| l.label == label));
            }
        }
    }
}
#[test]
fn malformed_body_edges_and_operands_fail_closed() {
    for offset in [0, 1, 999, 0xffff] {
        let mut c = cases().remove(0);
        c.words[3] = offset;
        assert!(render(&c).is_err(), "forward offset {offset}");
    }
    let mut c = cases().remove(0);
    c.words[2] = 0x0f38;
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    let pc = c.words.iter().position(|w| *w == 0x3034).unwrap();
    c.words[pc + 1] = 1;
    assert!(render(&c).is_err());
    let mut c = cases().remove(0);
    c.parameters[1] = "Ljava/lang/Object;";
    c.words[2] = 0x043a;
    assert!(render(&c).is_err());
    let mut c = cases().into_iter().find(|c| c.name == "wideExit").unwrap();
    let pc = c.words.iter().position(|w| *w == 0x7104).unwrap();
    c.words[pc] = 0x8104;
    assert!(render(&c).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn jvm_matches_branch_values_identity_effects_and_failure_prefixes() {
    let methods = cases()
        .iter()
        .map(|c| render(c).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let java = format!(
        r#"package sample;
class Hook{{static String trace="";static char failure;static int failAt;static final RuntimeException sentinel=new RuntimeException();
static void mark(char s,int i){{trace+=""+s+i+";";if(s==failure&&i==failAt)throw sentinel;}}
static int header(int i){{mark('H',i);return 100+i;}}static int a(int i){{mark('A',i);return 3;}}static int b(int i){{mark('B',i);return 5;}}
static boolean test(int i,int limit){{mark('C',i);return i<limit;}}static int tail(int s,int h){{mark('T',h-100);return s+h+1000;}}}}
public class PosttestBody{{{methods}
static int checks;
static void check(boolean ok){{checks++;if(!ok)throw new AssertionError("check "+checks);}}
static void effect(int limit,char fail,int at){{int n=Math.max(1,limit);StringBuilder expected=new StringBuilder();boolean fails=false;
outer:for(int i=0;i<=n;i++){{char[] stages=i==n?new char[]{{'T'}}:new char[]{{'H',i==0?'A':'B','C'}};for(char s:stages){{int index=s=='C'?i+1:s=='T'?n-1:i;expected.append(s).append(index).append(';');if(s==fail&&index==at){{fails=true;break outer;}}}}}}
Hook.trace="";Hook.failure=fail;Hook.failAt=at;try{{int value=effects(limit);check(!fails&&value==3+5*(n-1)+1100+n-1);}}catch(RuntimeException e){{check(fails&&e==Hook.sentinel);}}check(Hook.trace.equals(expected.toString()));}}
public static void main(String[] args){{Object left=new Object(),right=new Object();
for(int limit:new int[]{{Integer.MIN_VALUE,-1,0,1,2,7}}){{int n=Math.max(1,limit);for(int selector:new int[]{{-1,0,1}}){{check(nested(limit,selector)==(selector==0?11:13)+7*(n-1));check(conditionAtJoin(limit,selector)==(selector==0?1:n));check(carriedAndExit(limit,selector)==n*(selector==0?5:3)+(selector==0?29:17));check(sequential(limit,selector)==11+7*(n-1)+n*(selector==0?4:2));
for(Object x:new Object[]{{null,left,right}})for(Object y:new Object[]{{null,left,right}})check(referenceExit(limit,selector,x,y)==(selector==0?y:x));
for(long x:new long[]{{Long.MIN_VALUE,-1,0,Long.MAX_VALUE}})for(long y:new long[]{{Long.MIN_VALUE,-1,0,Long.MAX_VALUE}})check(wideExit(limit,selector,x,y)==(selector==0?y:x));}}
for(char failure:new char[]{{0,'H','A','B','C','T'}})for(int at:new int[]{{-1,0,1,2,6,7}})effect(limit,failure,at);}}
check(checks==954);System.out.println("954 behavioral assertions; 216 effect/failure scenarios");}}
}}"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-shared-posttest-body-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/PosttestBody.java"), java).unwrap();
    for (program, arg) in [
        ("javac", "sample/PosttestBody.java"),
        ("java", "sample.PosttestBody"),
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
            dir.join("sample/PosttestBody.java").display()
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
