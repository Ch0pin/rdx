use rdx::{
    native_dex::{self, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn fixture() -> native_dex::DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols::default());
    let m = &mut c.methods[0];
    m.name = "switchFeedback".into();
    m.access_flags = 9;
    m.parameters.clear();
    m.return_type = "I".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 0;
    code.outs = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x0012, 0x4112, 0x0212, 0x1035, 20, 0x002b, 19, 0, 0x02d8, 0x0102, 0x00d8, 0x0100, 0xf728,
        0x02d8, 0x0302, 0x00d8, 0x0100, 0xf228, 0x02d8, 0x0502, 0x00d8, 0x0100, 0xed28, 0x020f,
        0x0100, 2, 0, 0, 8, 0, 13, 0,
    ];
    c
}
#[test]
fn distinct_switch_feedback_keeps_enclosing_loop_frame() {
    let c = fixture();
    let s = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    assert_eq!(s.matches("switch (").count(), 1, "{s}");
    assert_eq!(s.matches("continue;").count(), 2, "{s}");
    assert!(s.contains("while (true)"), "{s}");
    println!("{s}");
}
pub use rdx::{
    native_call_values, native_calls, native_cfg, native_dominators, native_ir, native_ssa,
    native_types,
};
#[path = "../src/native_java/switch_feedback.rs"]
mod proof;
#[test]
fn exact_raw_proof_rejects_foreign_entries_cycles_protection_and_missing_inputs() {
    let c = fixture();
    let plan = proof::prove(&c, &c.methods[0], 3, 5, 22).unwrap();
    assert_eq!(plan.edges, vec![(12, 3), (17, 3), (22, 3)]);
    assert!(!plan.phi_inputs.is_empty());
    for mode in 0..4 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match mode {
            0 => code.instructions[0] = 0x0d28,
            1 => code.instructions[12] = 0x0028,
            2 => code.try_regions.push(native_dex::DexTryRegion {
                start: 5,
                end: 23,
                catches: Arc::from([(None, 23)]),
            }),
            3 => code.instructions[2] = 0,
            _ => unreachable!(),
        };
        assert!(
            proof::prove(&c, &c.methods[0], 3, 5, 22).is_none(),
            "negative{mode}"
        );
    }
}
fn effects_fixture(shared: bool, early: bool) -> native_dex::DexClass {
    let mut c = fixture();
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.parameters = vec!["I".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 5;
    code.ins = 1;
    code.outs = 3;
    c.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: vec!["header".into(), "step".into()],
        protos: vec![
            ("I".into(), vec!["I".into()]),
            ("I".into(), vec!["I".into(), "I".into(), "I".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let mut w = vec![0x0012, 0x0112];
    let h = w.len();
    w.extend([0x1071, 0, 0, 0x020a]);
    let guard = w.len();
    w.extend([0x4035, 0]);
    let sw = w.len();
    w.extend([0x022b, 0, 0]);
    let mut starts = Vec::new();
    let mut gotos = Vec::new();
    for arm in 0..3 {
        starts.push(w.len());
        w.push(0x0312 | ((arm as u16) << 12));
        w.extend([0x3071, 1, 0x0103, 0x010a, 0x00d8, 0x0100]);
        if early && arm == 1 {
            w.extend([0x0038, 3, 0x010f]);
        }
        gotos.push(w.len());
        w.extend([0x0029, 0]);
    }
    let exit = w.len();
    w.push(0x010f);
    if !w.len().is_multiple_of(2) {
        w.push(0)
    }
    let payload = w.len();
    w.extend([0x0100, 2, 0, 0]);
    for s in &starts[1..] {
        let d = (*s as i32 - sw as i32) as u32;
        w.extend([d as u16, (d >> 16) as u16]);
    }
    w[guard + 1] = (exit as isize - guard as isize) as u16;
    let d = (payload - sw) as u32;
    w[sw + 1] = d as u16;
    w[sw + 2] = (d >> 16) as u16;
    for (i, p) in gotos.iter().enumerate() {
        w[*p + 1] = ((if shared && i == 0 { starts[1] } else { h }) as isize - *p as isize) as u16;
    }
    code.instructions = w;
    c
}
#[test]
fn effectful_header_and_shared_tail_remain_per_iteration() {
    for shared in [false, true] {
        for early in [false, true] {
            let c = effects_fixture(shared, early);
            let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
                .unwrap()
                .source;
            assert!(source.contains(".header("), "{source}");
            assert!(source.contains(".step("), "{source}");
            assert!(source.contains("continue;"), "{source}");
        }
    }
}
struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME; own raw-word equivalence oracle"]
fn switch_feedback_jvm_preserves_header_arm_shared_tail_terminal_and_fault_identity() {
    use std::{fs, process::Command};
    let home = std::env::var_os("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir =
        Temp(std::env::temp_dir().join(format!("rdx-switch-feedback-{}", std::process::id())));
    fs::create_dir_all(dir.0.join("sample")).unwrap();
    let mut methods = String::new();
    let mut arrays = Vec::new();
    for (n, (shared, early)) in [(false, false), (false, true), (true, false), (true, true)]
        .into_iter()
        .enumerate()
    {
        let c = effects_fixture(shared, early);
        let s = native_java::render_method("sample.Hello", &c, &c.methods[0])
            .unwrap()
            .source;
        methods.push_str(&s.replace(" run(", &format!(" run{n}(")));
        arrays.push(format!(
            "{{{}}}",
            c.methods[0]
                .code
                .as_ref()
                .unwrap()
                .instructions
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    for n in 4..6 {
        let c = nested_fixture();
        let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
            .unwrap()
            .source;
        methods.push_str(&source.replace(" run(", &format!(" run{n}(")));
        arrays.push(format!(
            "{{{}}}",
            c.methods[0]
                .code
                .as_ref()
                .unwrap()
                .instructions
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    let java = ORACLE
        .replace("@METHODS@", &methods)
        .replace("@WORDS@", &arrays.join(","));
    fs::write(dir.0.join("sample/Hello.java"), &java).unwrap();
    for (tool, args) in [
        ("javac", vec!["sample/Hello.java"]),
        ("java", vec!["-cp", ".", "sample.Hello"]),
    ] {
        let out = Command::new(std::path::PathBuf::from(&home).join("bin").join(tool))
            .args(args)
            .current_dir(&dir.0)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{tool}: {}\n{java}",
            String::from_utf8_lossy(&out.stderr)
        );
        if tool == "java" {
            eprintln!("{}", String::from_utf8_lossy(&out.stdout));
        }
    }
}
const ORACLE: &str = r#"package sample;
public class Hello{
@METHODS@
static final int[][]WORDS={@WORDS@};
static int dex(int variant,int limit){int[]r=new int[6];r[4]=limit;r[5]=variant==4?0:3;int pending=0;int[]w=WORDS[variant];for(int pc=0,work=0;;){if(++work>2000)throw new AssertionError("work");int a=w[pc],op=a&255;switch(op){case 0x12:r[(a>>>8)&15]=((short)(a&0xf000))>>12;pc++;break;case 0x71:int words=w[pc+2];pending=w[pc+1]==0?Effects.header(r[words&15]):Effects.step(r[words&15],r[(words>>>4)&15],r[(words>>>8)&15]);pc+=3;break;case 0x0a:r[a>>>8]=pending;pc++;break;case 0x35:pc+=r[(a>>>8)&15]>=r[a>>>12]?(short)w[pc+1]:2;break;case 0x38:pc+=r[a>>>8]==0?(short)w[pc+1]:2;break;case 0x2b:int payload=pc+(w[pc+1]|w[pc+2]<<16),size=w[payload+1],first=w[payload+2]|w[payload+3]<<16,key=r[a>>>8]-first;pc+=key>=0&&key<size?(w[payload+4+2*key]|w[payload+5+2*key]<<16):3;break;case 0xd8:int v=w[pc+1];r[a>>>8]=r[v&255]+(byte)(v>>>8);pc+=2;break;case 0x29:pc+=(short)w[pc+1];break;case 0x0f:return r[a>>>8];default:throw new AssertionError("op"+op);}}}
static String outcome(boolean model,int variant,int limit,int salt,boolean mutate,int fault,boolean error){Effects.reset(salt,mutate,fault,error);try{int result=model?dex(variant,limit):switch(variant){case 0->run0(limit);case 1->run1(limit);case 2->run2(limit);case 3->run3(limit);case 4->run4(limit,0);default->run5(limit,3);};return "value"+result+"|"+Effects.trace+"|"+Effects.state;}catch(Throwable t){return (t==Effects.FAULT||t==Effects.ERROR?"identity":t.getClass().getName())+"|"+Effects.trace+"|"+Effects.state;}}
public static void main(String[]a){int count=0;for(int v=0;v<6;v++)for(int l=0;l<9;l++)for(int salt=0;salt<5;salt++)for(boolean mutate:new boolean[]{false,true})for(int f=0;f<41;f++)for(boolean error:new boolean[]{false,true}){String x=outcome(true,v,l,salt,mutate,f,error),y=outcome(false,v,l,salt,mutate,f,error);if(!x.equals(y))throw new AssertionError(v+","+l+","+salt+","+f+": "+x+" != "+y);count++;}System.out.println(count+" original-word callback/fault comparisons");}}
class Effects{static final RuntimeException FAULT=new RuntimeException("sentinel");static final Error ERROR=new Error("sentinel");static int state,salt,fault,event;static boolean mutate,error;static StringBuilder trace;static void reset(int s,boolean m,int f,boolean e){state=0;salt=s;fault=f;event=0;mutate=m;error=e;trace=new StringBuilder();}static void hit(String value){trace.append(value).append(';');if(++event==fault){if(error)throw ERROR;throw FAULT;}if(mutate)state++;}static int header(int i){hit("H"+i+":"+state);return(i+salt+state)%5-1;}static int step(int arm,int i,int sum){hit("S"+arm+":"+i+":"+sum+":"+state);return sum+arm*5+i+state+1;}}
"#;
fn rich_fixture() -> native_dex::DexClass {
    let mut c = fixture();
    let m = &mut c.methods[0];
    m.name = "rich".into();
    m.parameters = vec![
        "I".into(),
        "Ljava/lang/Object;".into(),
        "Ljava/lang/Object;".into(),
        "J".into(),
        "J".into(),
    ];
    m.return_type = "Ljava/lang/Object;".into();
    let code = m.code.as_mut().unwrap();
    code.registers = 27;
    code.ins = 7;
    code.outs = 7;
    c.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into()],
        strings: vec![
            "header".into(),
            "step".into(),
            "observe".into(),
            "finish".into(),
        ],
        protos: vec![
            ("I".into(), vec!["I".into()]),
            ("I".into(), vec!["I".into(), "I".into(), "I".into()]),
            (
                "V".into(),
                vec![
                    "Ljava/lang/Object;".into(),
                    "Ljava/lang/Object;".into(),
                    "J".into(),
                    "J".into(),
                ],
            ),
            (
                "Ljava/lang/Object;".into(),
                vec![
                    "I".into(),
                    "Ljava/lang/Object;".into(),
                    "Ljava/lang/Object;".into(),
                    "J".into(),
                    "J".into(),
                ],
            ),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1), (0, 2, 2), (0, 3, 3)],
        ..Default::default()
    });
    let mut w = vec![
        0x0012, 0x0112, 0x0302, 20, 0x0408, 21, 0x0508, 22, 0x0605, 23, 0x0805, 25,
    ];
    let h = w.len();
    w.extend([0x1071, 0, 0, 0x020a]);
    let guard = w.len();
    w.extend([0x3035, 0]);
    let sw = w.len();
    w.extend([0x022b, 0, 0]);
    let mut starts = Vec::new();
    let mut gotos = Vec::new();
    for arm in 0..3 {
        starts.push(w.len());
        if arm == 1 {
            w.extend([0x4a07, 0x5407, 0xa507, 0x6b04, 0x8604, 0xb804]);
        }
        if arm == 2 {
            w.push(0x0412);
        }
        w.extend([
            0x0677,
            2,
            4,
            0x0d12 | ((arm as u16) << 12),
            0x3071,
            1,
            0x010d,
            0x010a,
            0x00d8,
            0x0100,
        ]);
        gotos.push(w.len());
        w.extend([0x0029, 0]);
    }
    let exit = w.len();
    w.extend([0x1301, 0x0777, 3, 3, 0x020c, 0x0211]);
    if !w.len().is_multiple_of(2) {
        w.push(0)
    }
    let payload = w.len();
    w.extend([0x0100, 2, 0, 0]);
    for s in &starts[1..] {
        let d = (*s as i32 - sw as i32) as u32;
        w.extend([d as u16, (d >> 16) as u16]);
    }
    w[guard + 1] = (exit as isize - guard as isize) as u16;
    let d = (payload - sw) as u32;
    w[sw + 1] = d as u16;
    w[sw + 2] = (d >> 16) as u16;
    for p in gotos {
        w[p + 1] = (h as isize - p as isize) as u16
    }
    code.instructions = w;
    c
}
#[test]
fn wide_reference_phis_keep_parallel_swaps_and_null_arm() {
    let c = rich_fixture();
    let s = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(s.contains("long "), "{s}");
    assert!(s.contains(".observe("), "{s}");
    assert!(s.contains("continue;"), "{s}");
}
#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME; original wide-word/reference model"]
fn wide_reference_feedback_jvm_keeps_aliases_parallel_pairs_and_fault_order() {
    use std::{fs, process::Command};
    let home = std::env::var_os("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let dir = Temp(std::env::temp_dir().join(format!("rdx-switch-rich-{}", std::process::id())));
    fs::create_dir_all(dir.0.join("sample")).unwrap();
    let c = rich_fixture();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    let words = c.methods[0]
        .code
        .as_ref()
        .unwrap()
        .instructions
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let java = RICH_ORACLE
        .replace("@METHOD@", &source)
        .replace("@WORDS@", &words);
    fs::write(dir.0.join("sample/Hello.java"), &java).unwrap();
    for (tool, args) in [
        ("javac", vec!["sample/Hello.java"]),
        ("java", vec!["-cp", ".", "sample.Hello"]),
    ] {
        let o = Command::new(std::path::PathBuf::from(&home).join("bin").join(tool))
            .args(args)
            .current_dir(&dir.0)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{tool}: {}\n{java}",
            String::from_utf8_lossy(&o.stderr)
        );
        if tool == "java" {
            eprintln!("{}", String::from_utf8_lossy(&o.stdout));
        }
    }
}
const RICH_ORACLE: &str = r#"package sample;
public class Hello{
@METHOD@
static final int[]W={@WORDS@};static final Object TAIL=new Object();static long wide(Object[]r,int n){if(r[n+1]!=TAIL)throw new AssertionError("wide pair");return((Number)r[n]).longValue();}static Object dex(int limit,Object a,Object b,long x,long y){Object[]r=new Object[27];r[20]=limit;r[21]=a;r[22]=b;r[23]=x;r[24]=TAIL;r[25]=y;r[26]=TAIL;Object pending=null;for(int pc=0,work=0;;){if(++work>2000)throw new AssertionError("work");int w=W[pc],op=w&255,d=w>>>8;switch(op){case 0x12:if((d&15)==4)r[4]=null;else r[d&15]=(d>>>4)==0?0:(d>>>4);pc++;break;case 0x01:case 0x07:r[d&15]=r[w>>>12];pc++;break;case 0x04:int from=w>>>12,to=d&15;r[to]=wide(r,from);r[to+1]=TAIL;pc++;break;case 0x02:case 0x08:r[d]=r[W[pc+1]];pc+=2;break;case 0x05:r[d]=wide(r,W[pc+1]);r[d+1]=TAIL;pc+=2;break;case 0x71:int args=W[pc+2];pending=W[pc+1]==0?Effects.header(((Number)r[args&15]).intValue()):Effects.step(((Number)r[args&15]).intValue(),((Number)r[(args>>>4)&15]).intValue(),((Number)r[(args>>>8)&15]).intValue());pc+=3;break;case 0x77:int n=W[pc+2];if(W[pc+1]==2)Effects.observe(r[n],r[n+1],wide(r,n+2),wide(r,n+4));else pending=Effects.finish(((Number)r[n]).intValue(),r[n+1],r[n+2],wide(r,n+3),wide(r,n+5));pc+=3;break;case 0x0a:case 0x0c:r[d]=pending;pc++;break;case 0x35:pc+=((Number)r[d&15]).intValue()>=((Number)r[w>>>12]).intValue()?(short)W[pc+1]:2;break;case 0x2b:int p=pc+(W[pc+1]|W[pc+2]<<16),size=W[p+1],first=W[p+2]|W[p+3]<<16,key=((Number)r[d]).intValue()-first;pc+=key>=0&&key<size?(W[p+4+key*2]|W[p+5+key*2]<<16):3;break;case 0xd8:int v=W[pc+1];r[d]=((Number)r[v&255]).intValue()+(byte)(v>>>8);pc+=2;break;case 0x29:pc+=(short)W[pc+1];break;case 0x11:return r[d];default:throw new AssertionError("op"+op);}}}
static String out(boolean model,int limit,Object a,Object b,long x,long y,int salt,boolean mutate,int fault,boolean error){Effects.reset(salt,mutate,fault,error);try{Object value=model?dex(limit,a,b,x,y):rich(limit,a,b,x,y);return value+"|"+Effects.trace+"|"+Effects.state;}catch(Throwable t){return(t==Effects.FAULT||t==Effects.ERROR?"identity":t.getClass().getName())+"|"+Effects.trace+"|"+Effects.state;}}
public static void main(String[]z){int count=0;Object a="A",b="B";for(int alias=0;alias<4;alias++)for(int limit=0;limit<8;limit++)for(int salt=0;salt<5;salt++)for(boolean mutate:new boolean[]{false,true})for(int f=0;f<36;f++)for(boolean error:new boolean[]{false,true}){Object left=alias==0?null:a,right=alias==1?null:alias==2?a:b;long x=0x1122334455667788L,y=-0x7766554433221101L;String u=out(true,limit,left,right,x,y,salt,mutate,f,error),v=out(false,limit,left,right,x,y,salt,mutate,f,error);if(!u.equals(v))throw new AssertionError(alias+","+limit+","+salt+","+f+": "+u+" != "+v);count++;}System.out.println(count+" original wide/reference frame comparisons");}}
class Effects{static final RuntimeException FAULT=new RuntimeException();static final Error ERROR=new Error();static int state,salt,fault,event;static boolean mutate,error;static StringBuilder trace;static String id(Object o){return o==null?"null":o.toString();}static void reset(int s,boolean m,int f,boolean e){state=0;salt=s;fault=f;event=0;mutate=m;error=e;trace=new StringBuilder();}static void hit(String s){trace.append(s).append(';');if(++event==fault){if(error)throw ERROR;throw FAULT;}if(mutate)state++;}static int header(int i){hit("H"+i+":"+state);return(i+salt+state)%5-1;}static int step(int arm,int i,int sum){hit("S"+arm+":"+i+":"+sum+":"+state);return sum+arm*5+i+state+1;}static void observe(Object a,Object b,long x,long y){hit("O"+id(a)+","+id(b)+","+x+","+y);}static Object finish(int sum,Object a,Object b,long x,long y){hit("F"+sum+":"+id(a)+","+id(b)+","+x+","+y);return sum+":"+id(a)+","+id(b)+","+x+","+y;}}
"#;
fn nested_fixture() -> native_dex::DexClass {
    let mut c = effects_fixture(false, false);
    let m = &mut c.methods[0];
    m.parameters.push("I".into());
    let code = m.code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 2;
    let mut w = vec![0x0112, 0x0312, 0x5335, 0, 0x0012];
    let h = w.len();
    w.extend([0x1071, 0, 0, 0x020a]);
    let guard = w.len();
    w.extend([0x4035, 0]);
    let sw = w.len();
    w.extend([0x022b, 0, 0]);
    let mut starts = Vec::new();
    let mut gotos = Vec::new();
    for arm in 0..3 {
        starts.push(w.len());
        w.push(0x0212 | ((arm as u16) << 12));
        w.extend([0x3071, 1, 0x0102, 0x010a, 0x00d8, 0x0100]);
        gotos.push(w.len());
        w.extend([0x0029, 0]);
    }
    let inner_exit = w.len();
    w.extend([0x03d8, 0x0103, 0x0029, 0]);
    let outer_latch = inner_exit + 2;
    let exit = w.len();
    w.push(0x010f);
    if !w.len().is_multiple_of(2) {
        w.push(0)
    }
    let payload = w.len();
    w.extend([0x0100, 2, 0, 0]);
    for s in &starts[1..] {
        let d = (*s as i32 - sw as i32) as u32;
        w.extend([d as u16, (d >> 16) as u16]);
    }
    w[3] = (exit - 2) as u16;
    w[guard + 1] = (inner_exit - guard) as u16;
    let d = (payload - sw) as u32;
    w[sw + 1] = d as u16;
    w[sw + 2] = (d >> 16) as u16;
    w[outer_latch + 1] = (2isize - outer_latch as isize) as u16;
    for p in gotos {
        w[p + 1] = (h as isize - p as isize) as u16
    }
    code.instructions = w;
    c
}
#[test]
fn inner_switch_feedback_preserves_exact_nested_loop_owner() {
    let c = nested_fixture();
    let s = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    assert_eq!(s.matches("while (true)").count(), 2, "{s}");
    assert_eq!(s.matches("continue;").count(), 2, "{s}");
}
fn insert_words(c: &mut native_dex::DexClass, at: usize, words: &[u16]) {
    let code = c.methods[0].code.as_mut().unwrap();
    let ir = native_ir::DecodedMethod::decode(code).unwrap();
    let old = code.instructions.clone();
    code.instructions.splice(at..at, words.iter().copied());
    let map = |pc: usize| pc + if pc >= at { words.len() } else { 0 };
    for i in &ir.instructions {
        let pc = map(i.pc);
        if let Some(target) = i.branch_target {
            let d = map(target) as isize - pc as isize;
            if i.opcode == 0x28 {
                code.instructions[pc] =
                    (code.instructions[pc] & 255) | ((d as i8 as u8 as u16) << 8);
            } else {
                code.instructions[pc + 1] = d as u16;
            }
        }
        if let Some(payload) = i.payload_target {
            let p = map(payload);
            let d = (p as i32 - pc as i32) as u32;
            code.instructions[pc + 1] = d as u16;
            code.instructions[pc + 2] = (d >> 16) as u16;
            let n = old[payload + 1] as usize;
            for k in 0..n {
                let o = payload + 4 + k * 2;
                let t =
                    (i.pc as i32 + (old[o] as u32 | ((old[o + 1] as u32) << 16)) as i32) as usize;
                let delta = (map(t) as i32 - pc as i32) as u32;
                let dest = map(o);
                code.instructions[dest] = delta as u16;
                code.instructions[dest + 1] = (delta >> 16) as u16;
            }
        }
    }
}
#[test]
fn register_budget_extra_switch_child_cycle_and_partial_wide_stay_excluded() {
    let mut exact = fixture();
    exact.methods[0].code.as_mut().unwrap().registers = 256;
    assert!(proof::prove(&exact, &exact.methods[0], 3, 5, 22).is_some());
    for (regs, ins) in [(257, 0), (3, 4)] {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        code.registers = regs;
        code.ins = ins;
        assert!(proof::prove(&c, &c.methods[0], 3, 5, 22).is_none());
    }
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[12] = 0xfc28;
    assert!(proof::prove(&c, &c.methods[0], 3, 5, 22).is_none());
    let mut c = rich_fixture();
    insert_words(&mut c, 12, &[0x1712, 0]);
    let ir = native_ir::DecodedMethod::decode(c.methods[0].code.as_ref().unwrap()).unwrap();
    let switch = ir
        .instructions
        .iter()
        .find(|i| i.opcode == 0x2b)
        .unwrap()
        .pc;
    let latch = ir
        .instructions
        .iter()
        .rfind(|i| i.opcode == 0x29 && i.branch_target == Some(14))
        .unwrap()
        .pc;
    assert!(proof::prove(&c, &c.methods[0], 14, switch, latch).is_none());
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.splice(8..13, [0x002b, 24, 0, 0xf828, 0]);
    code.instructions.extend([0x0100, 1, 0, 0, 0xfffb, 0xffff]);
    assert!(native_cfg::ControlFlowGraph::build(code).is_ok());
    assert!(proof::prove(&c, &c.methods[0], 3, 5, 22).is_none());
}
#[test]
fn bounded_owned_instruction_certificate_and_navigation_stay_exact() {
    let mut c = effects_fixture(false, false);
    let ir = native_ir::DecodedMethod::decode(c.methods[0].code.as_ref().unwrap()).unwrap();
    let switch = ir
        .instructions
        .iter()
        .find(|i| i.opcode == 0x2b)
        .unwrap()
        .pc;
    let latch = ir
        .instructions
        .iter()
        .rfind(|i| i.opcode == 0x29 && i.branch_target == Some(2))
        .unwrap()
        .pc;
    let n = proof::prove(&c, &c.methods[0], 2, switch, latch)
        .unwrap()
        .owned
        .len();
    let extra = (256 - n) / 2 * 2;
    insert_words(&mut c, switch + 3, &vec![0; extra]);
    let p = proof::prove(&c, &c.methods[0], 2, switch, latch + extra).unwrap();
    assert_eq!(p.owned.len(), n + extra);
    insert_words(&mut c, switch + 3, &[0, 0]);
    assert!(proof::prove(&c, &c.methods[0], 2, switch, latch + extra + 2).is_none());
    let c = effects_fixture(true, true);
    let rendered = native_java::render_method("sample.Hello", &c, &c.methods[0]).unwrap();
    let links = format!("{:?}", rendered.links);
    assert!(links.contains("sample.Effects.header(I)I"), "{links}");
    assert!(links.contains("sample.Effects.step(III)I"), "{links}");
}
fn many_feedback_fixture(arms: usize) -> (native_dex::DexClass, usize, usize) {
    let mut c = fixture();
    let m = &mut c.methods[0];
    m.parameters = vec!["I".into()];
    let code = m.code.as_mut().unwrap();
    code.ins = 1;
    let mut w = vec![0x0012, 0x0112, 0x2035, 0, 0x002b, 0, 0];
    let mut starts = Vec::new();
    let mut gotos = Vec::new();
    for arm in 0..arms {
        starts.push(w.len());
        w.extend([
            0x01d8,
            (((arm + 1) as u16) << 8) | 1,
            0x00d8,
            0x0100,
            0x0029,
            0,
        ]);
        gotos.push(w.len() - 2);
    }
    let exit = w.len();
    w.push(0x010f);
    if !w.len().is_multiple_of(2) {
        w.push(0)
    }
    let payload = w.len();
    w.extend([0x0100, (arms - 1) as u16, 0, 0]);
    for pc in &starts[1..] {
        let d = (*pc - 4) as u32;
        w.extend([d as u16, (d >> 16) as u16]);
    }
    w[3] = (exit - 2) as u16;
    w[5] = (payload - 4) as u16;
    for pc in &gotos {
        w[pc + 1] = (2isize - *pc as isize) as u16
    }
    let latch = *gotos.last().unwrap();
    code.instructions = w;
    (c, 4, latch)
}
#[test]
fn exact_feedback_count_bound_accepts32_and_rejects33() {
    let (c, s, l) = many_feedback_fixture(32);
    let p = proof::prove(&c, &c.methods[0], 2, s, l).unwrap();
    assert_eq!(p.edges.len(), 32);
    let (c, s, l) = many_feedback_fixture(33);
    assert!(proof::prove(&c, &c.methods[0], 2, s, l).is_none());
}
