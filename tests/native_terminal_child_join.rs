//! Certified continuation edges bound joins in conditional parent pre-bodies.
pub use rdx::{native_cfg, native_dominators, native_ir};
#[path = "../src/native_java/terminal_child_loop.rs"]
mod plan;
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{collections::BTreeMap, fs, process::Command, sync::Arc};

#[derive(Default)]
struct Assembler {
    words: Vec<u16>,
    labels: BTreeMap<&'static str, usize>,
    branches: Vec<(usize, &'static str)>,
}
impl Assembler {
    fn label(&mut self, name: &'static str) {
        assert!(self.labels.insert(name, self.words.len()).is_none());
    }
    fn emit(&mut self, words: &[u16]) {
        self.words.extend_from_slice(words);
    }
    fn branch(&mut self, word: u16, label: &'static str) {
        self.branches.push((self.words.len(), label));
        self.emit(&[word, 0]);
    }
    fn jump(&mut self, label: &'static str) {
        self.branch(0x29, label);
    }
    fn finish(mut self) -> Vec<u16> {
        for (from, to) in self.branches {
            self.words[from + 1] = (self.labels[to] as isize - from as isize) as i16 as u16;
        }
        self.words
    }
    fn arm(&mut self, tag: u16) {
        self.emit(&[
            (tag << 12) | 0x0512,
            0x3071,
            1,
            0x05a0,
            0x050a,
            0x5681,
            0x029b,
            0x0602,
            0x2071,
            2,
            0x0054,
            0x040c,
        ]);
        self.branch(0x0538, "parent");
    }
}
fn fixture(orientation: bool, nested: bool, terminal: bool) -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Entry;".into();
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.fields.clear();
    c.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: vec![
            "header".into(),
            "pre".into(),
            "alias".into(),
            "body".into(),
            "finish".into(),
        ],
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("I".into(), vec!["I".into(), "I".into(), "I".into()]),
            (
                "Ljava/lang/Object;".into(),
                vec!["Ljava/lang/Object;".into(), "I".into()],
            ),
            ("I".into(), vec!["I".into(), "I".into()]),
            (
                "J".into(),
                vec!["J".into(), "Ljava/lang/Object;".into(), "I".into()],
            ),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 3, 3), (0, 4, 4)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.declaring_type = c.descriptor.clone();
    m.access_flags = 9;
    m.return_type = "J".into();
    m.parameters = vec![
        "I".into(),
        "I".into(),
        "I".into(),
        "J".into(),
        "Ljava/lang/Object;".into(),
    ];
    let code = m.code.as_mut().unwrap();
    code.registers = 14;
    code.ins = 6;
    code.outs = 4;
    let mut a = Assembler::default();
    a.emit(&[0x0012, 0xb204, 0xd407]);
    a.label("parent");
    a.emit(&[0x1071, 0, 0]);
    a.branch(0x8035, "exit");
    a.emit(&[0x00d8, 0x0100]);
    a.branch(if orientation { 0x0a39 } else { 0x0a38 }, "arm_b");
    if nested {
        a.branch(0x0a3a, "arm_c");
    }
    a.arm(1);
    a.jump("join");
    if nested {
        a.label("arm_c");
        a.arm(3);
        a.jump("join");
    }
    a.label("arm_b");
    a.arm(2);
    a.label("join");
    a.emit(&[0x0112]);
    a.label("child");
    a.branch(0x9135, "parent");
    a.emit(&[
        0x2071, 3, 0x0010, 0x050a, 0x5681, 0x029b, 0x0602, 0x2071, 2, 0x0054, 0x040c, 0x01d8,
        0x0101,
    ]);
    if terminal {
        a.branch(0x9134, "child");
    } else {
        a.jump("child");
    }
    a.label("exit");
    a.emit(&[0x4071, 4, 0x0432, 0x020b, 0x0210]);
    code.instructions = a.finish();
    c
}
#[test]
fn certified_parent_diamonds_keep_both_routes_nested_joins_and_exit_frames() {
    for orientation in [false, true] {
        for nested in [false, true] {
            for terminal in [false, true] {
                let c = fixture(orientation, nested, terminal);
                let code = c.methods[0].code.as_ref().unwrap();
                let p = plan::select(code).expect("canonical terminal child certificate");
                assert!(plan::validate(code, &p));
                assert_eq!(p.parent_backedges.len(), if nested { 4 } else { 3 });
                assert_eq!(p.terminal_fallthrough.is_some(), terminal);
                let rendered = native_java::render_method("sample.Entry", &c, &c.methods[0])
                    .unwrap_or_else(|e| {
                        panic!("orientation={orientation} nested={nested} terminal={terminal}: {e}")
                    });
                assert!(rendered.source.contains("while"));
                assert!(format!("{:?}", rendered.links).contains("sample.Effects.pre(III)I"));
                assert_eq!(
                    rendered.source.matches("sample.Effects.pre(").count(),
                    if nested { 3 } else { 2 }
                );
            }
        }
    }
}
#[test]
fn exact_certificates_reject_foreign_entries_backedges_and_protected_overlap() {
    let c = fixture(false, true, true);
    let code = c.methods[0].code.as_ref().unwrap();
    let p = plan::select(code).unwrap();
    for index in 0..4 {
        let mut bad = p.clone();
        match index {
            0 => bad
                .edges
                .retain(|e| e.owner != plan::EdgeOwner::ParentContinue),
            1 => {
                bad.parent_members.remove(0);
            }
            2 => bad.parent_exit += 1,
            3 => {
                bad.edges
                    .iter_mut()
                    .find(|e| e.owner == plan::EdgeOwner::ChildContinue)
                    .unwrap()
                    .owner = plan::EdgeOwner::Internal
            }
            _ => unreachable!(),
        };
        assert!(!plan::validate(code, &bad));
    }
    for index in 0..4 {
        let mut fresh = fixture(false, true, true);
        let bad = fresh.methods[0].code.as_mut().unwrap();
        match index {
            0 => {
                bad.instructions[p.parent_guard + 1] = (p.child_header + 2 - p.parent_guard) as u16
            }
            1 => {
                bad.instructions[p.child_latch + 1] =
                    (p.child_header as isize + 2 - p.child_latch as isize) as i16 as u16
            }
            2 => bad.try_regions.push(native_dex::DexTryRegion {
                start: p.parent_guard as u32,
                end: p.child_header as u32,
                catches: vec![(None, p.parent_exit as u32)].into(),
            }),
            3 => bad.instructions[p.parent_header] = 0x001d,
            _ => unreachable!(),
        };
        assert!(plan::select(bad).is_none(), "negative {index}");
    }
}

#[test]
#[ignore = "requires pinned JDK; emitted source compiled unchanged"]
fn unchanged_sources_match_original_dex_parallel_words_aliases_effects_and_faults() {
    let dir = std::env::temp_dir().join(format!("rdx-terminal-child-join-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let mut arrays = Vec::new();
    let mut calls = Vec::new();
    let mut index = 0;
    for orientation in [false, true] {
        for nested in [false, true] {
            for terminal in [false, true] {
                let c = fixture(orientation, nested, terminal);
                let rendered =
                    native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
                fs::write(
                    dir.join(format!("sample/Entry{index}.java")),
                    format!(
                        "package sample; public class Entry{index} {{ {} }}",
                        rendered.source
                    ),
                )
                .unwrap();
                arrays.push(format!(
                    "{{{}}}",
                    c.methods[0]
                        .code
                        .as_ref()
                        .unwrap()
                        .instructions
                        .iter()
                        .map(|w| w.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ));
                calls.push(format!(
                    "case {index}:return Entry{index}.run(outer,inner,mode,seed,ref);"
                ));
                index += 1;
            }
        }
    }
    let harness = HARNESS
        .replace("__WORDS__", &arrays.join(","))
        .replace("__CALLS__", &calls.join(""));
    fs::write(dir.join("sample/Main.java"), harness).unwrap();
    let home =
        std::path::PathBuf::from(std::env::var_os("RDX_JAVA25_HOME").expect("pinned JDK required"));
    let mut compile = Command::new(home.join("bin/javac"));
    compile.arg("-d").arg(&dir);
    for file in fs::read_dir(dir.join("sample")).unwrap() {
        compile.arg(file.unwrap().path());
    }
    let output = compile.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new(home.join("bin/java"))
        .arg("-Xverify:all")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Main")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    fs::remove_dir_all(dir).unwrap();
}
const HARNESS: &str = r#"package sample;
class Effects {
static final Object A=new Object(),B=new Object();static final RuntimeException R=new RuntimeException();static final Error E=new Error();
static StringBuilder trace;static int calls,fault;static long state;static Throwable token;
static void reset(int f,Throwable t){trace=new StringBuilder();calls=0;fault=f;state=0;token=t;}
static int identity(Object o){return o==null?0:o==A?1:o==B?2:3;}
static void effect(String kind,long a,long b){trace.append(kind).append(a).append(':').append(b).append(';');state=state*31+a*7+b;if(++calls==fault){if(token instanceof Error)throw (Error)token;throw (RuntimeException)token;}}
static void header(int i){effect("h",i,0);}
static int pre(int i,int mode,int tag){effect("p",i,tag);return (i+mode+tag)%4==0?0:i*tag-mode;}
static Object alias(Object o,int n){effect("a",identity(o),n);return n==0?o:(n&1)==0?A:B;}
static int body(int i,int j){effect("b",i,j);return i*31+j;}
static long finish(long sum,Object o,int i){effect("f",sum,identity(o));return sum+state+identity(o)*101L+i;}
}
public class Main {
static final int[][] W={__WORDS__};
static long get(int[]r,int a){return Integer.toUnsignedLong(r[a])|((long)r[a+1]<<32);}
static void set(int[]r,int a,long v){r[a]=(int)v;r[a+1]=(int)(v>>>32);}
static long oracle(int[]w,int outer,int inner,int mode,long seed,Object ref){int[]r=new int[14];Object[]o=new Object[14];r[8]=outer;r[9]=inner;r[10]=mode;set(r,11,seed);o[13]=ref;int pc=0,pending=0,budget=10000;long wide=0;Object object=null;
while(--budget>0){int word=w[pc],a=word>>>8;switch(word&255){
case 0x12:r[a&15]=(a>>>4)>=8?(a>>>4)-16:a>>>4;pc++;break;
case 0x04:set(r,a&15,get(r,a>>>4));pc++;break;case 0x07:o[a&15]=o[a>>>4];pc++;break;
case 0x38:pc+=r[a]==0?(short)w[pc+1]:2;break;case 0x39:pc+=r[a]!=0?(short)w[pc+1]:2;break;case 0x3a:pc+=r[a]<0?(short)w[pc+1]:2;break;
case 0x34:pc+=r[a&15]<r[a>>>4]?(short)w[pc+1]:2;break;case 0x35:pc+=r[a&15]>=r[a>>>4]?(short)w[pc+1]:2;break;
case 0x29:pc+=(short)w[pc+1];break;
case 0x71:{int x=w[pc+2],b=x&15,c=(x>>>4)&15,d=(x>>>8)&15,e=(x>>>12)&15;switch(w[pc+1]){
case 0:Effects.header(r[b]);break;case 1:pending=Effects.pre(r[b],r[c],r[d]);break;case 2:object=Effects.alias(o[b],r[c]);break;case 3:pending=Effects.body(r[b],r[c]);break;case 4:wide=Effects.finish(get(r,b),o[d],r[e]);break;default:throw new AssertionError();}pc+=3;break;}
case 0x0a:r[a]=pending;pc++;break;case 0x0b:set(r,a,wide);pc++;break;case 0x0c:o[a]=object;pc++;break;
case 0x81:set(r,a&15,r[a>>>4]);pc++;break;case 0x9b:{int x=w[pc+1];set(r,a,get(r,x&255)+get(r,x>>>8));pc+=2;break;}
case 0xd8:{int x=w[pc+1];r[a]=r[x&255]+(byte)(x>>>8);pc+=2;break;}
case 0x10:return get(r,a);default:throw new AssertionError("opcode "+Integer.toHexString(word)+" at "+pc);}}throw new AssertionError("budget");}
static long actual(int variant,int outer,int inner,int mode,long seed,Object ref){switch(variant){__CALLS__default:throw new AssertionError();}}
public static void main(String[]args){int count=0;for(int variant=0;variant<8;variant++)for(int outer:new int[]{-1,0,1,2,5,8})for(int inner:new int[]{-1,0,1,2,4})for(int mode:new int[]{-3,0,1,4})for(long seed:new long[]{0,Long.MIN_VALUE,Long.MAX_VALUE,0x7fffffff00000001L})for(Object ref:new Object[]{null,Effects.A,Effects.B})for(int fault=0;fault<36;fault++)for(Throwable token:new Throwable[]{Effects.R,Effects.E}){
Effects.reset(fault,token);long a=0;Throwable x=null;try{a=actual(variant,outer,inner,mode,seed,ref);}catch(Throwable t){x=t;}String trace=Effects.trace.toString();long state=Effects.state;
Effects.reset(fault,token);long b=0;Throwable y=null;try{b=oracle(W[variant],outer,inner,mode,seed,ref);}catch(Throwable t){y=t;}
if(a!=b||x!=y||state!=Effects.state||!trace.equals(Effects.trace.toString()))throw new AssertionError(variant+":"+outer+":"+inner+":"+mode+":"+seed+":"+fault+":"+a+":"+b+":"+x+":"+y+":"+trace+":"+Effects.trace);count++;}
System.out.println(count+" raw DEX parent-prebody join comparisons");}}
"#;
