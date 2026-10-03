//! Exact forward iteration DAG and terminal edge copies, encoded DEX oracle.
use rdx::{
    native_cfg,
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_dominators, native_ir, native_java,
};
use std::{fs, process::Command, sync::Arc};
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod forward_iteration_entry;
fn fixture() -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec![
            "<init>".into(),
            "header".into(),
            "step".into(),
            "tail".into(),
            "extra".into(),
            "terminal".into(),
        ],
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
        methods: vec![
            (0, 0, 0),
            (1, 1, 1),
            (1, 2, 2),
            (1, 2, 3),
            (1, 2, 4),
            (1, 2, 5),
        ],
        ..Default::default()
    });
    let m = &mut class.methods[0];
    m.name = "decode".into();
    m.access_flags = 9;
    m.parameters = vec![
        "I".into(),
        "I".into(),
        "J".into(),
        "Ljava/lang/Object;".into(),
    ];
    m.return_type = "Ljava/lang/Object;".into();
    let c = m.code.as_mut().unwrap();
    c.registers = 10;
    c.ins = 5;
    c.outs = 5;
    c.tries = 0;
    c.try_regions.clear();
    c.instructions = vec![
        0x12, 0x7104, 0x9307, 0x2412, 0x4532, 0x2a, 0x538, 0x4, 0x7412, 0x1728, 0xd8, 0x100,
        0x4071, 0x1, 0x3210, 0x6034, 0x8, 0x422, 0x0, 0x5070, 0x0, 0x3214, 0x411, 0x938, 0x17,
        0x1412, 0x2071, 0x2, 0x34, 0x40a, 0x439, 0x9, 0x2071, 0x3, 0x34, 0x40a, 0x19b, 0x701,
        0xe428, 0x2071, 0x4, 0x34, 0x40a, 0x439, 0xffdf, 0x311, 0x2071, 0x5, 0x34, 0x40a, 0x311,
    ];
    class
}
#[test]
fn exact_edge_ownership_and_poison_reproof() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let plan = forward_iteration_entry::select(code).unwrap();
    assert!(forward_iteration_entry::validate(code, &plan));
    assert_eq!(plan.continues.len(), 2);
    assert_eq!(plan.copies.len(), 2);
    assert!(plan.copies.iter().any(|c| c.header.is_none()));
    assert!(
        plan.copies
            .iter()
            .any(|c| c.header.is_some() && *c.instructions.last().unwrap() < plan.latch)
    );
    for mutation in 0..12 {
        let mut bad = plan.clone();
        match mutation {
            0 => bad.header += 1,
            1 => bad.latch -= 1,
            2 => {
                bad.members.pop();
            }
            3 => {
                bad.iteration_nodes.pop();
            }
            4 => bad.copies[0].target += 1,
            5 => {
                bad.copies[0].instructions.pop();
            }
            6 => {
                bad.continues.pop();
            }
            7 => {
                bad.terminal_edges.pop();
            }
            8 => {
                bad.edges.pop();
            }
            9 => bad.targets[0].1 += 1,
            10 => bad.words[0] ^= 0x100,
            11 => bad.registers += 1,
            _ => unreachable!(),
        }
        assert!(
            !forward_iteration_entry::validate(code, &bad),
            "mutation {mutation}"
        );
    }
}
#[test]
fn protected_cycles_pending_results_and_allocating_copies_reject() {
    for mutation in 0..12 {
        let mut c = fixture();
        let code = c.methods[0].code.as_mut().unwrap();
        match mutation {
            0 => {
                code.tries = 1;
                code.try_regions.push(DexTryRegion {
                    start: 10,
                    end: 12,
                    catches: vec![(None, 46)].into(),
                });
            }
            1 => code.instructions[44] = 0xfffe, // new interior backedge
            2 => code.instructions[9] = 0x1a28,  // external entry into move-result at35
            3 => code.instructions[32] = 0x0422, // allocation in copied suffix
            4 => code.instructions[5] = 29,      // branch into invocation operand
            5 => code.instructions[38] = 0xff28, // suffix cycle
            6 => code.instructions[46] = 0x0422, // terminal copy allocation
            7 => code.instructions = vec![0x002a, 0xffff],
            8 => code.instructions = vec![0x0013, 0xff28, 0x000f],
            9 => code.instructions[50] = 0xd828, // terminal reentry toheader10
            10 => code.instructions[5] = ((49) - 4) as u16, // terminal move-result sideentry
            11 => {
                code.instructions[50] = 0x0012;
                code.instructions.extend(std::iter::repeat_n(0, 33));
                code.instructions.push(0x0311);
            } // terminal overbudget
            _ => unreachable!(),
        }
        assert!(
            forward_iteration_entry::select(code).is_none(),
            "mutation {mutation}"
        );
    }
}
#[test]
fn real_renderer_keeps_original_continues_terminal_effects_and_types() {
    let c = fixture();
    let source = native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("while (true)"));
    assert!(source.matches("continue;").count() >= 2);
    assert!(source.contains("sample.Effects.terminal"));
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
    let root = std::env::temp_dir().join(format!("rdx-forward-iteration-{}", std::process::id()));
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
  static StringBuilder trace; static int failStep,failHeader,stepCount,tailCount,extraCount; static boolean failCtor,failInit;
  static String identity(Object o) { return o==null?"null":o==REF?"ref":"wrong"; }
  static void reset(int step, int header, boolean ctor) { trace=new StringBuilder(); stepCount=0;tailCount=0;extraCount=0;failStep=step;failHeader=header;failCtor=ctor; }
  static void init() { trace.append("init;"); if(failInit)throw INIT; }
  static void header(int count,long sum,Object o) { trace.append("h:").append(count).append(':').append(sum).append(':').append(identity(o)).append(';');if(count==failHeader)throw HEADER; }
  static int step(int seed,Object o) { trace.append("s:").append(seed).append(':').append(identity(o)).append(';');if(++stepCount==failStep)throw STEP;return stepCount%3==0?0:1; }
  static int tail(int seed,Object o) {trace.append("t:").append(seed).append(':').append(identity(o)).append(';');if(++stepCount==failStep)throw STEP;tailCount++;return seed;}
  static int extra(int seed,Object o) {trace.append("e:").append(seed).append(':').append(identity(o)).append(';');if(++stepCount==failStep)throw STEP;return ++extraCount<2?1:0;}
  static int terminal(int seed,Object o) {trace.append("z:").append(seed).append(':').append(identity(o)).append(';');if(++stepCount==failStep)throw STEP;return seed;}
  static void constructor(long sum,Object o,int count) { trace.append("c:").append(count).append(':').append(sum).append(':').append(identity(o)).append(';');if(failCtor)throw CTOR; }
}
public class Main {
  static boolean oracleInitialized;
  static final int[] WORDS={0x12, 0x7104, 0x9307, 0x2412, 0x4532, 0x2a, 0x538, 0x4, 0x7412, 0x1728, 0xd8, 0x100, 0x4071, 0x1, 0x3210, 0x6034, 0x8, 0x422, 0x0, 0x5070, 0x0, 0x3214, 0x411, 0x938, 0x17, 0x1412, 0x2071, 0x2, 0x34, 0x40a, 0x439, 0x9, 0x2071, 0x3, 0x34, 0x40a, 0x19b, 0x701, 0xe428, 0x2071, 0x4, 0x34, 0x40a, 0x439, 0xffdf, 0x311, 0x2071, 0x5, 0x34, 0x40a, 0x311};
  static class Allocated { long sum;Object reference;int count; }
  // Decode original DEX registers, widths, branch offsets, and invoke operands.
  // No Java control structure from the reconstructed source is copied here.
  static Object oracle(int mode,int limit,long initial,Object reference) {
    Object[] r=new Object[10];r[5]=mode;r[6]=limit;r[7]=initial;r[8]=initial;r[9]=reference;
    Object pending=null;int pc=0,budget=10000;
    while(--budget>0) {
      int w=WORDS[pc],op=w&255,a=w>>>8;
      switch(op) {
        case 0x12: { int v=(a>>>4);if(v>=8)v-=16;r[a&15]=v;pc++;break; }
        case 0x04:r[a&15]=r[a>>>4];r[(a&15)+1]=r[(a>>>4)+1];pc++;break;
        case 0x07:r[a&15]=r[a>>>4];pc++;break;
        case 0x32:pc=((Integer)r[a&15]).equals((Integer)r[a>>>4])?pc+(short)WORDS[pc+1]:pc+2;break;
        case 0x39:pc=((Integer)r[a])!=0?pc+(short)WORDS[pc+1]:pc+2;break;
        case 0x38:pc=(r[a]==null || r[a] instanceof Integer && ((Integer)r[a])==0)?pc+(short)WORDS[pc+1]:pc+2;break;
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
            case 3:pending=Effects.tail((Integer)r[c],r[d]);break;
            case 4:pending=Effects.extra((Integer)r[c],r[d]);break;
            case 5:pending=Effects.terminal((Integer)r[c],r[d]);break;
            default:throw new AssertionError();
          }pc+=3;break;
        }
        case 0x0a:r[a]=pending;pending=null;pc++;break;
        case 0x11:return r[a];
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
    Effects.reset(step,header,ctor);Object actual=null;Throwable actualError=null;
    try{actual=Hello.decode(mode,limit,initial,reference);}catch(Throwable t){actualError=t;}
    String actualTrace=Effects.trace.toString();
    Effects.reset(step,header,ctor);Object expected=null;Throwable expectedError=null;
    try{expected=oracle(mode,limit,initial,reference);}catch(Throwable t){expectedError=t;}
    if(!actualTrace.equals(Effects.trace.toString()) || !throwable(actualError).equals(throwable(expectedError)))
      throw new AssertionError("mode="+mode+" limit="+limit+" faults="+step+","+header+","+ctor+" actual="+actualTrace+" "+throwable(actualError)+" expected="+Effects.trace+" "+throwable(expectedError));
    if(actualError==null) {
      if(actual instanceof Box a && expected instanceof Allocated e) {if(a.sum!=e.sum || a.reference!=e.reference || a.count!=e.count)throw new AssertionError("box value identity");}
      else if(actual!=expected) throw new AssertionError("terminal reference identity");
    }
  }
  public static void main(String[] args) {
    if(args[0].equals("init-fault")) { Effects.failInit=true;check(1,2,9L,Effects.REF,0,0,false);System.out.println("init-fault: 1 comparison");return; }
    int count=0;
    for(int mode:new int[]{0,1,2})for(int limit:new int[]{0,1,2,3,5})for(long initial:new long[]{0L,1L,-1L,Long.MIN_VALUE,Long.MAX_VALUE})for(Object reference:new Object[]{null,Effects.REF})
      for(int step:new int[]{0,1,2,3,4,5,7})for(int header:new int[]{0,1,2,4})for(boolean ctor:new boolean[]{false,true}){
        check(mode,limit,initial,reference,step,header,ctor);count++;
      }
    System.out.println("matrix: "+count+" comparisons");
  }
}
"#;
