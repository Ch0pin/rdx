pub use rdx::{native_cfg, native_dominators, native_ir, native_ssa};
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod plan;
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};
fn fixture() -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Entry;".into();
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: vec![
            "header".into(),
            "body".into(),
            "tail".into(),
            "after".into(),
        ],
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("I".into(), vec!["I".into()]),
            ("I".into(), vec!["I".into(), "I".into(), "I".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 1, 3)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.return_type = "I".into();
    m.parameters = vec!["I".into(), "I".into(), "I".into()];
    m.declaring_type = c.descriptor.clone();
    let code = m.code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 3;
    code.outs = 3;
    code.instructions = vec![
        0x0012, 0x5101, 0x0339, 13, 0x1071, 0, 0, 0x4035, 18, 0x1071, 1, 0, 0x020a, 0x0239, 7,
        0x3071, 2, 0x0051, 0x020a, 0x020f, 0x00d8, 0x0100, 0x0190, 0x0001, 0xec28, 0x1071, 3, 1,
        0x020a, 0x020f,
    ];
    c
}
#[test]
fn terminal_foreign_tail_has_exact_raw_input_certificate() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = plan::select(code).unwrap();
    assert!(p.copies.iter().all(|c| c.header.is_none()));
    assert!(!p.terminal_copy_ssa_reads.is_empty());
    assert!(plan::validate(code, &p));
    let rendered = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
    assert!(rendered.source.contains("while"));
    assert!(format!("{:?}", rendered.links).contains("sample.Effects.tail(III)I"));
    let mut bad = p.clone();
    bad.terminal_copy_ssa_reads.clear();
    assert!(!plan::validate(code, &bad));
}
#[test]
fn protected_undefined_and_register_overflow_reject() {
    for mutation in 0..3 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match mutation {
            0 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 15,
                    end: 19,
                    catches: vec![(None, 25)].into(),
                });
            }
            1 => code.instructions[1] = 0x2101,
            2 => code.registers = 257,
            _ => unreachable!(),
        }
        assert!(plan::select(code).is_none(), "mutation {mutation}");
    }
}
struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
#[ignore = "requires javac/java; emitted source compiled unchanged"]
fn unchanged_java_preserves_terminal_tail_frames_effects_and_fault_identity() {
    let c = fixture();
    let source = native_java::render_method("sample.Entry", &c, &c.methods[0])
        .unwrap()
        .source;
    let dir = Temp(std::env::temp_dir().join(format!("rdx-terminal-entry-{}", std::process::id())));
    fs::create_dir_all(dir.0.join("sample")).unwrap();
    fs::write(
        dir.0.join("sample/Entry.java"),
        format!("package sample; public class Entry {{ {source} }}"),
    )
    .unwrap();
    fs::write(dir.0.join("sample/Main.java"), HARNESS).unwrap();
    let home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
    let tool = |name: &str| {
        home.as_ref()
            .map(|p| p.join("bin").join(name))
            .unwrap_or_else(|| name.into())
    };
    let compile = Command::new(tool("javac"))
        .arg("-d")
        .arg(&dir.0)
        .arg(dir.0.join("sample/Entry.java"))
        .arg(dir.0.join("sample/Main.java"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(tool("java"))
        .arg("-Xverify:all")
        .arg("-cp")
        .arg(&dir.0)
        .arg("sample.Main")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&run.stdout));
}
const HARNESS: &str = r#"package sample;
class Effects { static final RuntimeException R=new RuntimeException();static final Error E=new Error();static Throwable token;static StringBuilder trace;static int fault,calls,state,mode;
static void reset(int f,int m,Throwable t){fault=f;mode=m;token=t;calls=0;state=0;trace=new StringBuilder();}
static void effect(String name,int v){trace.append(name).append(v).append(';');state=state*31+v; if(++calls==fault){if(token instanceof Error)throw (Error)token;throw (RuntimeException)token;}}
static void header(int v){effect("h",v);}static int body(int v){effect("b",v);return (v+mode)%3==0?0:1;}
static int tail(int a,int seed,int counter){effect("t",a);effect("s",seed);effect("c",counter);return a+seed+counter+state;}
static int after(int a){effect("a",a);return a+state;}}
public class Main {static final int[] W={0x0012,0x5101,0x0339,13,0x1071,0,0,0x4035,18,0x1071,1,0,0x020a,0x0239,7,0x3071,2,0x0051,0x020a,0x020f,0x00d8,0x0100,0x0190,0x0001,0xec28,0x1071,3,1,0x020a,0x020f};
static int oracle(int entry,int limit,int seed){int[]r=new int[6];r[3]=entry;r[4]=limit;r[5]=seed;int pc=0,pending=0,budget=1000;while(--budget>0){int w=W[pc],a=w>>>8;switch(w&255){
case 0x12:r[a&15]=(a>>>4)>=8?(a>>>4)-16:a>>>4;pc++;break;
case 0x01:r[a&15]=r[a>>>4];pc++;break;
case 0x39:pc+=r[a]!=0?(short)W[pc+1]:2;break;
case 0x35:pc+=r[a&15]>=r[a>>>4]?(short)W[pc+1]:2;break;
case 0x71:{int x=W[pc+2];switch(W[pc+1]){case 0:Effects.header(r[x&15]);break;case 1:pending=Effects.body(r[x&15]);break;case 2:pending=Effects.tail(r[x&15],r[(x>>>4)&15],r[(x>>>8)&15]);break;case 3:pending=Effects.after(r[x&15]);break;default:throw new AssertionError();}pc+=3;break;}
case 0x0a:r[a]=pending;pc++;break;case 0x0f:return r[a];
case 0xd8:{int x=W[pc+1];r[a]=r[x&255]+(byte)(x>>>8);pc+=2;break;}
case 0x90:{int x=W[pc+1];r[a]=r[x&255]+r[x>>>8];pc+=2;break;}
case 0x28:pc+=(byte)a;break;default:throw new AssertionError();}}throw new AssertionError("budget");}
public static void main(String[]args){int n=0;for(int entry:new int[]{0,1,-1})for(int limit:new int[]{-1,0,1,2,5,20})for(int seed:new int[]{0,1,-99,Integer.MIN_VALUE,Integer.MAX_VALUE})for(int mode=0;mode<3;mode++)for(int fault=0;fault<28;fault++)for(Throwable token:new Throwable[]{Effects.R,Effects.E}){Effects.reset(fault,mode,token);int a=0;Throwable x=null;try{a=Entry.run(entry,limit,seed);}catch(Throwable t){x=t;}String trace=Effects.trace.toString();int state=Effects.state;Effects.reset(fault,mode,token);int b=0;Throwable y=null;try{b=oracle(entry,limit,seed);}catch(Throwable t){y=t;}if(a!=b||x!=y||state!=Effects.state||!trace.equals(Effects.trace.toString()))throw new AssertionError(entry+":"+limit+":"+seed+":"+mode+":"+fault+":"+trace+":"+Effects.trace);n++;}System.out.println(n+" raw DEX terminal-tail comparisons");}}
"#;

fn wide_fixture() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.protos = vec![
        ("V".into(), vec!["I".into()]),
        ("I".into(), vec!["I".into()]),
        ("J".into(), vec!["J".into(), "J".into(), "I".into()]),
        ("J".into(), vec!["J".into()]),
    ];
    symbols.methods[3] = (0, 3, 3);
    let m = &mut c.methods[0];
    m.return_type = "J".into();
    m.parameters = vec!["I".into(), "I".into(), "J".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 8;
    code.ins = 4;
    code.outs = 5;
    code.instructions = vec![
        0x0012, 0x6104, 0x0439, 13, 0x1071, 0, 0, 0x5035, 18, 0x1071, 1, 0, 0x030a, 0x0339, 7,
        0x5071, 2, 0x7621, 0x010b, 0x0110, 0x00d8, 0x0100, 0x019b, 0x0601, 0xec28, 0x2071, 3,
        0x0021, 0x010b, 0x0110,
    ];
    c
}
#[test]
fn complete_wide_tail_pairs_are_required() {
    let c = wide_fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = plan::select(code).unwrap();
    assert!(!p.terminal_copy_ssa_reads.is_empty());
    assert!(native_java::render_method("sample.Entry", &c, &c.methods[0]).is_ok());
    let mut bad = wide_fixture();
    let code = bad.methods[0].code.as_mut().unwrap();
    code.instructions[1] = 0x0212;
    assert!(plan::select(code).is_none());
}
#[test]
fn cyclic_foreign_entry_and_nonterminal_tail_never_gain_terminal_certificate() {
    for mutation in 0..2 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match mutation {
            0 => code.instructions[3] = 7,
            1 => code.instructions[19] = 0xf128,
            _ => unreachable!(),
        };
        if let Some(p) = plan::select(code) {
            assert!(p.terminal_copy_ssa_reads.is_empty());
        }
    }
}

#[test]
#[ignore = "requires javac/java; emitted source compiled unchanged"]
fn unchanged_wide_java_preserves_both_raw_words_and_tail_timing() {
    let c = wide_fixture();
    let source = native_java::render_method("sample.Entry", &c, &c.methods[0])
        .unwrap()
        .source;
    let dir = Temp(std::env::temp_dir().join(format!("rdx-terminal-wide-{}", std::process::id())));
    fs::create_dir_all(dir.0.join("sample")).unwrap();
    fs::write(
        dir.0.join("sample/Entry.java"),
        format!("package sample; public class Entry {{ {source} }}"),
    )
    .unwrap();
    fs::write(dir.0.join("sample/Main.java"), WIDE_HARNESS).unwrap();
    let home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
    let tool = |name: &str| {
        home.as_ref()
            .map(|p| p.join("bin").join(name))
            .unwrap_or_else(|| name.into())
    };
    let compile = Command::new(tool("javac"))
        .arg("-d")
        .arg(&dir.0)
        .arg(dir.0.join("sample/Entry.java"))
        .arg(dir.0.join("sample/Main.java"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(tool("java"))
        .arg("-Xverify:all")
        .arg("-cp")
        .arg(&dir.0)
        .arg("sample.Main")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&run.stdout));
}

const WIDE_HARNESS: &str = r#"package sample;
class Effects{static final RuntimeException R=new RuntimeException();static final Error E=new Error();static Throwable token;static StringBuilder trace;static int fault,calls,state,mode;static void reset(int f,int m,Throwable t){fault=f;mode=m;token=t;calls=0;state=0;trace=new StringBuilder();}static void effect(String name,long v){trace.append(name).append(v).append(';');state=state*31+(int)v;if(++calls==fault){if(token instanceof Error)throw (Error)token;throw (RuntimeException)token;}}static void header(int v){effect("h",v);}static int body(int v){effect("b",v);return (v+mode)%3==0?0:1;}static long tail(long a,long seed,int c){effect("t",a);effect("s",seed);effect("c",c);return a+seed+c+state;}static long after(long a){effect("a",a);return a+state;}}
public class Main{static final int[]W={0x0012,0x6104,0x0439,13,0x1071,0,0,0x5035,18,0x1071,1,0,0x030a,0x0339,7,0x5071,2,0x7621,0x010b,0x0110,0x00d8,0x0100,0x019b,0x0601,0xec28,0x2071,3,0x0021,0x010b,0x0110};static long wide(int[]r,int a){return Integer.toUnsignedLong(r[a])|((long)r[a+1]<<32);}static void put(int[]r,int a,long v){r[a]=(int)v;r[a+1]=(int)(v>>>32);}
static long oracle(int entry,int limit,long seed){int[]r=new int[8];r[4]=entry;r[5]=limit;put(r,6,seed);int pc=0,budget=1000;long pending=0;while(--budget>0){int w=W[pc],a=w>>>8;switch(w&255){case 0x12:r[a&15]=a>>>4;pc++;break;case 0x04:put(r,a&15,wide(r,a>>>4));pc++;break;case 0x39:pc+=r[a]!=0?(short)W[pc+1]:2;break;case 0x35:pc+=r[a&15]>=r[a>>>4]?(short)W[pc+1]:2;break;case 0x71:{int x=W[pc+2];switch(W[pc+1]){case 0:Effects.header(r[x&15]);break;case 1:pending=Effects.body(r[x&15]);break;case 2:pending=Effects.tail(wide(r,x&15),wide(r,(x>>>8)&15),r[(w>>>8)&15]);break;case 3:pending=Effects.after(wide(r,x&15));break;default:throw new AssertionError();}pc+=3;break;}case 0x0a:r[a]=(int)pending;pc++;break;case 0x0b:put(r,a,pending);pc++;break;case 0x10:return wide(r,a);case 0xd8:{int x=W[pc+1];r[a]=r[x&255]+(byte)(x>>>8);pc+=2;break;}case 0x9b:{int x=W[pc+1];put(r,a,wide(r,x&255)+wide(r,x>>>8));pc+=2;break;}case 0x28:pc+=(byte)a;break;default:throw new AssertionError();}}throw new AssertionError("budget");}
public static void main(String[]args){int n=0;for(int entry:new int[]{0,1,-1})for(int limit:new int[]{-1,0,1,2,5,20})for(long seed:new long[]{0,1,-99,Long.MIN_VALUE,Long.MAX_VALUE,0x123456789abcdef0L})for(int mode=0;mode<3;mode++)for(int fault=0;fault<28;fault++)for(Throwable token:new Throwable[]{Effects.R,Effects.E}){Effects.reset(fault,mode,token);long a=0;Throwable x=null;try{a=Entry.run(entry,limit,seed);}catch(Throwable t){x=t;}String trace=Effects.trace.toString();int state=Effects.state;Effects.reset(fault,mode,token);long b=0;Throwable y=null;try{b=oracle(entry,limit,seed);}catch(Throwable t){y=t;}if(a!=b||x!=y||state!=Effects.state||!trace.equals(Effects.trace.toString()))throw new AssertionError(entry+":"+limit+":"+seed+":"+mode+":"+fault+":"+a+":"+b+":"+trace+":"+Effects.trace);n++;}System.out.println(n+" exact wide-word terminal comparisons");}}
"#;

fn reference_fixture() -> DexClass {
    let mut c = fixture();
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.strings.push("select".into());
    symbols.protos = vec![
        ("V".into(), vec!["I".into()]),
        ("I".into(), vec!["I".into()]),
        (
            "Ljava/lang/Object;".into(),
            vec![
                "Ljava/lang/Object;".into(),
                "Ljava/lang/Object;".into(),
                "I".into(),
            ],
        ),
        (
            "Ljava/lang/Object;".into(),
            vec!["Ljava/lang/Object;".into()],
        ),
    ];
    symbols.methods[3] = (0, 3, 3);
    symbols.methods.push((0, 2, 4));
    let m = &mut c.methods[0];
    m.return_type = "Ljava/lang/Object;".into();
    m.parameters[2] = "Ljava/lang/Object;".into();
    let code = m.code.as_mut().unwrap();
    code.instructions = vec![
        0x0012, 0x5107, 0x0339, 13, 0x1071, 0, 0, 0x4035, 20, 0x1071, 1, 0, 0x020a, 0x0239, 7,
        0x3071, 2, 0x0051, 0x020c, 0x0211, 0x3071, 4, 0x0051, 0x010c, 0x00d8, 0x0100, 0xea28,
        0x1071, 3, 1, 0x020c, 0x0211,
    ];
    c
}
#[test]
fn reference_alias_frames_and_mixed_copy_paths_keep_original_ownership() {
    let c = reference_fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = plan::select(code).unwrap();
    assert!(!p.terminal_copy_ssa_reads.is_empty());
    assert!(native_java::render_method("sample.Entry", &c, &c.methods[0]).is_ok());
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[3] = 7;
    if let Some(p) = plan::select(code) {
        assert!(p.terminal_copy_ssa_reads.is_empty());
    }
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[15] = 0x051d;
    assert!(plan::select(code).is_none());
}

#[test]
#[ignore = "requires javac/java; emitted source compiled unchanged"]
fn unchanged_reference_java_preserves_aliases_nulls_and_argument_frames() {
    let c = reference_fixture();
    let source = native_java::render_method("sample.Entry", &c, &c.methods[0])
        .unwrap()
        .source;
    let dir =
        Temp(std::env::temp_dir().join(format!("rdx-terminal-reference-{}", std::process::id())));
    fs::create_dir_all(dir.0.join("sample")).unwrap();
    fs::write(
        dir.0.join("sample/Entry.java"),
        format!("package sample; public class Entry {{ {source} }}"),
    )
    .unwrap();
    fs::write(dir.0.join("sample/Main.java"), REFERENCE_HARNESS).unwrap();
    let home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
    let tool = |name: &str| {
        home.as_ref()
            .map(|p| p.join("bin").join(name))
            .unwrap_or_else(|| name.into())
    };
    let compile = Command::new(tool("javac"))
        .arg("-d")
        .arg(&dir.0)
        .arg(dir.0.join("sample/Entry.java"))
        .arg(dir.0.join("sample/Main.java"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(tool("java"))
        .arg("-Xverify:all")
        .arg("-cp")
        .arg(&dir.0)
        .arg("sample.Main")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&run.stdout));
}

const REFERENCE_HARNESS: &str = r#"package sample;
class Effects{static final RuntimeException R=new RuntimeException();static final Error E=new Error();static final Object A=new Object(),B=new Object();static Throwable token;static StringBuilder trace;static int fault,calls,state,mode;static int id(Object v){return v==null?0:v==A?1:v==B?2:3;}static void reset(int f,int m,Throwable t){fault=f;mode=m;token=t;calls=0;state=0;trace=new StringBuilder();}static void effect(String name,int v){trace.append(name).append(v).append(';');state=state*31+v;if(++calls==fault){if(token instanceof Error)throw (Error)token;throw (RuntimeException)token;}}static void header(int v){effect("h",v);}static int body(int v){effect("b",v);return (v+mode)%4==0?0:1;}static Object select(Object a,Object seed,int c){effect("x",id(a));effect("y",id(seed));return (c+mode)%3==0?null:(c+mode)%3==1?seed:B;}static Object tail(Object a,Object seed,int c){effect("t",id(a));effect("s",id(seed));effect("c",c);return a;}static Object after(Object a){effect("a",id(a));return a;}}
public class Main{static final int[]W={0x0012,0x5107,0x0339,13,0x1071,0,0,0x4035,20,0x1071,1,0,0x020a,0x0239,7,0x3071,2,0x0051,0x020c,0x0211,0x3071,4,0x0051,0x010c,0x00d8,0x0100,0xea28,0x1071,3,1,0x020c,0x0211};static Object oracle(int entry,int limit,Object seed){Object[]r=new Object[6];r[3]=entry;r[4]=limit;r[5]=seed;int pc=0,budget=1000;Object pending=null;while(--budget>0){int w=W[pc],a=w>>>8;switch(w&255){case 0x12:r[a&15]=a>>>4;pc++;break;case 0x07:r[a&15]=r[a>>>4];pc++;break;case 0x39:pc+=(Integer)r[a]!=0?(short)W[pc+1]:2;break;case 0x35:pc+=(Integer)r[a&15]>=(Integer)r[a>>>4]?(short)W[pc+1]:2;break;case 0x71:{int x=W[pc+2];switch(W[pc+1]){case 0:Effects.header((Integer)r[x&15]);break;case 1:pending=Effects.body((Integer)r[x&15]);break;case 2:pending=Effects.tail(r[x&15],r[(x>>>4)&15],(Integer)r[(x>>>8)&15]);break;case 3:pending=Effects.after(r[x&15]);break;case 4:pending=Effects.select(r[x&15],r[(x>>>4)&15],(Integer)r[(x>>>8)&15]);break;default:throw new AssertionError();}pc+=3;break;}case 0x0a:case 0x0c:r[a]=pending;pc++;break;case 0x11:return r[a];case 0xd8:{int x=W[pc+1];r[a]=(Integer)r[x&255]+(byte)(x>>>8);pc+=2;break;}case 0x28:pc+=(byte)a;break;default:throw new AssertionError();}}throw new AssertionError("budget");}
public static void main(String[]args){int n=0;for(int entry:new int[]{0,1,-1})for(int limit:new int[]{-1,0,1,2,5,20})for(Object seed:new Object[]{null,Effects.A,Effects.B})for(int mode=0;mode<4;mode++)for(int fault=0;fault<28;fault++)for(Throwable token:new Throwable[]{Effects.R,Effects.E}){Effects.reset(fault,mode,token);Object a=null;Throwable x=null;try{a=Entry.run(entry,limit,seed);}catch(Throwable t){x=t;}String trace=Effects.trace.toString();int state=Effects.state;Effects.reset(fault,mode,token);Object b=null;Throwable y=null;try{b=oracle(entry,limit,seed);}catch(Throwable t){y=t;}if(a!=b||x!=y||state!=Effects.state||!trace.equals(Effects.trace.toString()))throw new AssertionError(entry+":"+limit+":"+Effects.id(seed)+":"+mode+":"+fault+":"+trace+":"+Effects.trace);n++;}System.out.println(n+" exact reference/alias terminal comparisons");}}
"#;

#[test]
fn mixed_header_and_terminal_external_entries_stay_rejected() {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.splice(2..2, [0x0539, 9]);
    assert!(plan::select(code).is_none());
}

#[test]
fn narrow_overwrite_of_initialized_wide_high_word_is_rejected() {
    let mut c = wide_fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.insert(2, 0x0212);
    assert!(native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err());
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x0012, 0x6104, 0x0212, 0x0439, 13, 0x1071, 0, 0, 0x5035, 19, 0x1071, 1, 0, 0x030a, 0x0339,
        8, 0x1104, 0x5071, 2, 0x7621, 0x010b, 0x0110, 0x00d8, 0x0100, 0x019b, 0x0601, 0xeb28,
        0x2071, 3, 0x0021, 0x010b, 0x0110,
    ];
    assert!(plan::select(code).is_none());
}

#[test]
fn copied_tail_navigation_points_to_each_emitted_call() {
    let c = fixture();
    let rendered = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
    let links: Vec<_> = rendered
        .links
        .iter()
        .filter(|link| link.label == "sample.Effects.tail(III)I")
        .collect();
    assert_eq!(links.len(), 2);
    for link in links {
        let text: String = rendered
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect();
        assert_eq!(text, "tail");
    }
}

fn readless_fixture() -> DexClass {
    let mut c = fixture();
    let m = &mut c.methods[0];
    m.return_type = "V".into();
    let code = m.code.as_mut().unwrap();
    code.instructions[15..20].copy_from_slice(&[0, 0, 0, 0, 0x000e]);
    code.instructions[29] = 0x000e;
    c
}
#[test]
fn readless_terminal_copy_has_owned_plan_with_empty_read_certificate() {
    let c = readless_fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = plan::select(code).expect("readless terminal-only plan");
    assert!(p.copies.iter().all(|copy| copy.header.is_none()));
    assert!(p.terminal_copy_ssa_reads.is_empty());
    assert!(plan::validate(code, &p));
    let source = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
    assert!(
        source.source.contains("break;"),
        "raw CFG route stays available: {}",
        source.source
    );
    assert!(source.source.contains("while"));
    let mut foreign = p.clone();
    foreign.copies[0].header = Some(p.header);
    assert!(!plan::validate(code, &foreign));
    let mut c = c;
    c.methods[0].code.as_mut().unwrap().instructions[19] = 0x020f;
    assert!(plan::select(c.methods[0].code.as_ref().unwrap()).is_none());
}

#[test]
#[ignore = "requires javac/java; emitted source compiled unchanged"]
fn readless_terminal_copy_jvm_preserves_effects_and_exact_faults() {
    let c = readless_fixture();
    let source = native_java::render_method("sample.Entry", &c, &c.methods[0])
        .unwrap()
        .source;
    let code = c.methods[0].code.as_ref().unwrap();
    let start = HARNESS.find("static final int[] W=").unwrap();
    let end = start + HARNESS[start..].find(';').unwrap() + 1;
    let mut harness = HARNESS.to_owned();
    harness.replace_range(
        start..end,
        &format!(
            "static final int[] W={{{}}};",
            code.instructions
                .iter()
                .map(|w| w.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ),
    );
    harness = harness
        .replace(
            "a=Entry.run(entry,limit,seed)",
            "Entry.run(entry,limit,seed)",
        )
        .replace(
            "case 0x12:",
            "case 0:pc++;break;case 0x0e:return 0;case 0x12:",
        );
    let dir = Temp(std::env::temp_dir().join(format!("rdx-readless-entry-{}", std::process::id())));
    fs::create_dir_all(dir.0.join("sample")).unwrap();
    fs::write(
        dir.0.join("sample/Entry.java"),
        format!("package sample; public class Entry {{ {source} }}"),
    )
    .unwrap();
    fs::write(dir.0.join("sample/Main.java"), harness).unwrap();
    let home = std::env::var_os("RDX_JAVA25_HOME")
        .map(std::path::PathBuf::from)
        .expect("pinned JDK required");
    let compile = Command::new(home.join("bin/javac"))
        .arg("-d")
        .arg(&dir.0)
        .arg(dir.0.join("sample/Entry.java"))
        .arg(dir.0.join("sample/Main.java"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(home.join("bin/java"))
        .arg("-Xverify:all")
        .arg("-cp")
        .arg(&dir.0)
        .arg("sample.Main")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&run.stdout));
}
