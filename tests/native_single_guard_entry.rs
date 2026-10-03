//! Bounded terminal-guard node splitting: proof plus unchanged-source JVM checks.
use rdx::{
    native_cfg,
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_dominators, native_ir, native_java,
};
use std::{fs, process::Command, sync::Arc};
#[path = "../src/native_java/single_guard_entry.rs"]
mod single_guard_entry;

fn fixture() -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class
        .methods
        .retain(|method| method.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["<init>".into(), "header".into(), "step".into()],
        types: vec![
            "Lsample/Box;".into(),
            "Lsample/Effects;".into(),
            "Ljava/lang/Object;".into(),
        ],
        protos: vec![
            (
                "V".into(),
                vec!["J".into(), "Ljava/lang/Object;".into(), "I".into()],
            ),
            (
                "V".into(),
                vec!["I".into(), "J".into(), "Ljava/lang/Object;".into()],
            ),
            ("I".into(), vec!["I".into(), "Ljava/lang/Object;".into()]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1), (1, 2, 2)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "decode".into();
    method.access_flags = 9;
    method.parameters = vec![
        "I".into(),
        "I".into(),
        "J".into(),
        "Ljava/lang/Object;".into(),
    ];
    method.return_type = "Lsample/Box;".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 10;
    code.ins = 5;
    code.outs = 5;
    code.tries = 0;
    code.try_regions.clear();
    code.instructions = vec![
        0x0012, 0x7104, 0x9307, 0x0538, 4, 0x7412, 0x0f28, 0x00d8, 0x0100, 0x4071, 1, 0x3210,
        0x6034, 8, 0x0422, 0, 0x5070, 0, 0x3214, 0x0411, 0x1412, 0x2071, 2, 0x0034, 0x040a, 0x019b,
        0x0701, 0xec28,
    ];
    class
}

#[test]
fn exact_paths_and_complete_edge_ownership_are_reproved() {
    let class = fixture();
    let code = class.methods[0].code.as_ref().unwrap();
    let plan = single_guard_entry::select(code).unwrap();
    assert!(single_guard_entry::validate(code, &plan));
    assert_eq!(plan.header_path, [7, 9, 12]);
    assert_eq!(plan.continuation_path, [20, 21, 24, 25, 27]);
    assert_eq!(plan.terminal_path, [14, 16, 19]);
    assert_eq!(plan.copy, [21, 24, 25, 27]);
    for mutation in 0..15 {
        let mut bad = plan.clone();
        match mutation {
            0 => bad.header += 1,
            1 => bad.guard += 1,
            2 => bad.latch -= 1,
            3 => bad.continuation += 1,
            4 => bad.terminal += 1,
            5 => bad.copy_from += 1,
            6 => bad.copy_target += 1,
            7 => {
                bad.copy.pop();
            }
            8 => {
                bad.header_path.pop();
            }
            9 => {
                bad.continuation_path.pop();
            }
            10 => {
                bad.terminal_path.pop();
            }
            11 => {
                bad.edges.pop();
            }
            12 => bad.targets[0].1 += 1,
            13 => bad.words[8] ^= 0x100,
            14 => bad.registers += 1,
            _ => unreachable!(),
        }
        assert!(
            !single_guard_entry::validate(code, &bad),
            "metadata mutation {mutation}"
        );
    }
}

#[test]
fn cycles_interior_results_allocations_and_handler_entries_stay_rejected() {
    for mutation in 0..9 {
        let mut class = fixture();
        let code = class.methods[0].code.as_mut().unwrap();
        match mutation {
            0 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 7,
                    end: 12,
                    catches: vec![(None, 14)].into(),
                });
            }
            1 => code.instructions[13] = 0xfffb, // backward conditional reentry
            2 => code.instructions[6] = 0x0828,  // external constructor entry
            3 => code.instructions[6] = 0x1228,  // pending move-result entry
            4 => code.instructions[7] = 0x0022,  // allocating selected header
            5 => code.instructions[27] = 0xfd28, // suffix's private interior cycle
            6 => code.instructions[13] = 4,      // branch into constructor invoke operands
            7 => code.instructions = vec![0x002a, 0xffff], // truncated raw goto/32
            8 => code.instructions = vec![0x0013, 0xff28, 0x000f], // operand resembles backward goto
            _ => unreachable!(),
        }
        assert!(
            single_guard_entry::select(code).is_none(),
            "raw mutation {mutation}"
        );
    }
}

#[test]
fn ordinary_loop_and_entry_copy_keep_original_effects_and_constructor_arm() {
    let class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("while (true)"));
    assert!(source.contains("new sample.Box"));
    assert!(!source.contains("switch ("));
    assert!(!source.contains("__pc"));
}

#[test]
#[ignore = "requires javac/java; emitted source is compiled unchanged"]
fn unchanged_java_matches_encoded_register_oracle_on_both_entries_and_faults() {
    let class = fixture();
    let source = native_java::render_method("sample.Hello", &class, &class.methods[0])
        .unwrap()
        .source;
    let root = std::env::temp_dir().join(format!("rdx-single-guard-{}", std::process::id()));
    let package = root.join("sample");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("Hello.java"),
        format!("package sample; public class Hello {{ {source} }}"),
    )
    .unwrap();
    fs::write(package.join("Main.java"), HARNESS).unwrap();
    let java_home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
    let javac = java_home
        .as_ref()
        .map(|p| p.join("bin/javac"))
        .unwrap_or_else(|| "javac".into());
    let java = java_home
        .as_ref()
        .map(|p| p.join("bin/java"))
        .unwrap_or_else(|| "java".into());
    let version = Command::new(&javac).arg("-version").output().unwrap();
    assert!(version.status.success());
    eprintln!(
        "{}{}",
        String::from_utf8_lossy(&version.stdout),
        String::from_utf8_lossy(&version.stderr)
    );
    let compile = Command::new(&javac)
        .args(["-d"])
        .arg(&root)
        .arg(package.join("Hello.java"))
        .arg(package.join("Main.java"))
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compile.stderr)
    );
    for mode in ["matrix", "init-fault"] {
        let run = Command::new(&java)
            .args(["-Xverify:all", "-cp"])
            .arg(&root)
            .args(["sample.Main", mode])
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
}

const HARNESS: &str = r#"
package sample;
class Box {
  static { Effects.init(); }
  final long sum; final Object reference; final int count;
  Box(long sum, Object reference, int count) {
    Effects.constructor(sum, reference, count);
    this.sum=sum; this.reference=reference; this.count=count;
  }
}
class Effects {
  static final RuntimeException STEP=new RuntimeException("step"), HEADER=new RuntimeException("header"), CTOR=new RuntimeException("ctor"), INIT=new RuntimeException("init");
  static final Object REF=new Object();
  static StringBuilder trace; static int failStep,failHeader,stepCount; static boolean failCtor,failInit;
  static String identity(Object o) { return o==null?"null":o==REF?"ref":"wrong"; }
  static void reset(int step, int header, boolean ctor) { trace=new StringBuilder(); stepCount=0;failStep=step;failHeader=header;failCtor=ctor; }
  static void init() { trace.append("init;"); if(failInit)throw INIT; }
  static void header(int count,long sum,Object o) { trace.append("h:").append(count).append(':').append(sum).append(':').append(identity(o)).append(';');if(count==failHeader)throw HEADER; }
  static int step(int seed,Object o) { trace.append("s:").append(seed).append(':').append(identity(o)).append(';');if(++stepCount==failStep)throw STEP;return seed; }
  static void constructor(long sum,Object o,int count) { trace.append("c:").append(count).append(':').append(sum).append(':').append(identity(o)).append(';');if(failCtor)throw CTOR; }
}
public class Main {
  static boolean oracleInitialized;
  static final int[] WORDS={0x0012,0x7104,0x9307,0x0538,4,0x7412,0x0f28,0x00d8,0x0100,0x4071,1,0x3210,0x6034,8,0x0422,0,0x5070,0,0x3214,0x0411,0x1412,0x2071,2,0x0034,0x040a,0x019b,0x0701,0xec28};
  static class Allocated { long sum;Object reference;int count; }
  // Decode original DEX registers, widths, branch offsets, and invoke operands.
  // No Java control structure from the reconstructed source is copied here.
  static Allocated oracle(int mode,int limit,long initial,Object reference) {
    Object[] r=new Object[10];r[5]=mode;r[6]=limit;r[7]=initial;r[8]=initial;r[9]=reference;
    Object pending=null;int pc=0,budget=10000;
    while(--budget>0) {
      int w=WORDS[pc],op=w&255,a=w>>>8;
      switch(op) {
        case 0x12: { int v=(a>>>4);if(v>=8)v-=16;r[a&15]=v;pc++;break; }
        case 0x04:r[a&15]=r[a>>>4];r[(a&15)+1]=r[(a>>>4)+1];pc++;break;
        case 0x07:r[a&15]=r[a>>>4];pc++;break;
        case 0x38:pc=((Integer)r[a])==0?pc+(short)WORDS[pc+1]:pc+2;break;
        case 0x34:pc=(Integer)r[a&15]<(Integer)r[a>>>4]?pc+(short)WORDS[pc+1]:pc+2;break;
        case 0x28:pc+=(byte)a;break;
        case 0xd8:{ int args=WORDS[pc+1];r[a]=(Integer)r[args&255]+(byte)(args>>>8);pc+=2;break; }
        case 0x9b:{ int args=WORDS[pc+1];long v=(Long)r[args&255]+(Long)r[args>>>8];r[a]=v;r[a+1]=v;pc+=2;break; }
        case 0x22:
          if(!oracleInitialized) { oracleInitialized=true;try{Effects.init();}catch(RuntimeException x){throw new ExceptionInInitializerError(x);} }
          r[a]=new Allocated();pc+=2;break;
        case 0x70:case 0x71:{
          int args=WORDS[pc+2],c=args&15,d=(args>>>4)&15,e=(args>>>8)&15,f=(args>>>12)&15,g=a&15;
          switch(WORDS[pc+1]) {
            case 0: { Allocated x=(Allocated)r[c];Effects.constructor((Long)r[d],r[f],(Integer)r[g]);x.sum=(Long)r[d];x.reference=r[f];x.count=(Integer)r[g];break; }
            case 1:Effects.header((Integer)r[c],(Long)r[d],r[f]);break;
            case 2:pending=Effects.step((Integer)r[c],r[d]);break;
            default:throw new AssertionError();
          }pc+=3;break;
        }
        case 0x0a:r[a]=pending;pending=null;pc++;break;
        case 0x11:return (Allocated)r[a];
        default:throw new AssertionError("opcode "+op);
      }
    }throw new AssertionError("oracle budget");
  }
  static String throwable(Throwable t) {
    if(t==null)return "none";
    if(t==Effects.STEP)return "step";if(t==Effects.HEADER)return "header";if(t==Effects.CTOR)return "ctor";
    if(t instanceof ExceptionInInitializerError && t.getCause()==Effects.INIT)return "init";
    return "unexpected:"+t;
  }
  static void check(int mode,int limit,long initial,Object reference,int step,int header,boolean ctor) {
    Effects.reset(step,header,ctor);Box actual=null;Throwable actualError=null;
    try{actual=Hello.decode(mode,limit,initial,reference);}catch(Throwable t){actualError=t;}
    String actualTrace=Effects.trace.toString();
    Effects.reset(step,header,ctor);Allocated expected=null;Throwable expectedError=null;
    try{expected=oracle(mode,limit,initial,reference);}catch(Throwable t){expectedError=t;}
    if(!actualTrace.equals(Effects.trace.toString()) || !throwable(actualError).equals(throwable(expectedError)))
      throw new AssertionError("mode="+mode+" limit="+limit+" faults="+step+","+header+","+ctor+" actual="+actualTrace+" "+throwable(actualError)+" expected="+Effects.trace+" "+throwable(expectedError));
    if(actualError==null && (actual.sum!=expected.sum || actual.reference!=expected.reference || actual.count!=expected.count))throw new AssertionError("value identity");
  }
  public static void main(String[] args) {
    if(args[0].equals("init-fault")) { Effects.failInit=true;check(1,2,9L,Effects.REF,0,0,false);System.out.println("init-fault: 1 comparison");return; }
    int count=0;
    for(int mode:new int[]{0,1})for(int limit:new int[]{0,1,2,3,5})for(long initial:new long[]{0L,1L,-1L,Long.MIN_VALUE,Long.MAX_VALUE})for(Object reference:new Object[]{null,Effects.REF})
      for(int step:new int[]{0,1,2,4})for(int header:new int[]{0,1,2,4})for(boolean ctor:new boolean[]{false,true}){
        check(mode,limit,initial,reference,step,header,ctor);count++;
      }
    System.out.println("matrix: "+count+" comparisons");
  }
}
"#;
