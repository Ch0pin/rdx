pub use rdx::{native_cfg, native_dominators, native_ir, native_ssa};
#[allow(dead_code)]
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod forward_iteration_entry;
#[path = "../src/native_java/parent_continue.rs"]
mod plan;
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::{fs, process::Command, sync::Arc};
fn sole_plan(code: &native_dex::DexCode) -> Option<plan::Plan> {
    let plans = plan::select_all(code)?;
    (plans.len() == 1).then(|| plans[0].clone())
}
fn variant(kind: usize) -> DexClass {
    let wide = kind == 3 || kind == 4;
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Entry;".into();
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Effects;".into(),
            "[I".into(),
            "Lsample/Box;".into(),
        ],
        strings: vec!["parent".into(), "child".into(), "<init>".into()],
        protos: vec![("V".into(), vec!["I".into()]), ("V".into(), vec![])],
        methods: vec![(0, 0, 0), (0, 0, 1), (2, 1, 2)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.return_type = if wide { "J" } else { "I" }.into();
    m.parameters = vec!["I".into(), "I".into(), if wide { "J" } else { "I" }.into()];
    m.declaring_type = c.descriptor.clone();
    let code = m.code.as_mut().unwrap();
    code.registers = if wide { 11 } else { 7 };
    code.ins = if wide { 4 } else { 3 };
    code.outs = 1;
    let mut w = vec![
        0x0012,
        if wide { 0x9204 } else { 0x6201 },
        if wide { 0x7423 } else { 0x4323 },
        1,
    ];
    let parent = w.len();
    w.extend([0x1071, 0, 0]);
    let parent_guard = w.len();
    w.extend([
        if wide { 0x7035 } else { 0x4035 },
        0,
        0x00d8,
        0x0100,
        0x0112,
    ]);
    let child = w.len();
    w.extend([
        if wide { 0x8135 } else { 0x5135 },
        (parent as isize - child as isize) as i16 as u16,
    ]);
    w.extend([
        if wide { 0x0422 } else { 0x0322 },
        2,
        0x1070,
        2,
        if wide { 4 } else { 3 },
        0x1071,
        1,
        1,
    ]);
    if kind == 1 {
        w.extend([0x2301, 0x6201, 0x3601]);
    }
    if kind == 4 {
        w.extend([0x2504, 0x9204, 0x5904]);
    }
    if kind == 2 {
        w.extend([0x04d8, 0xff04, 0x00d8, 0x0100, 0x05d8, 0xff05]);
    }
    if kind == 5 {
        w.extend([0x0639, 3, 0x020f]);
    }
    w.extend(if wide {
        vec![0x029b, 0x0902]
    } else {
        vec![0x0290, 0x0002]
    });
    w.extend([0x01d8, 0x0101]);
    let latch = w.len();
    w.push(((child as isize - latch as isize) as i8 as u8 as u16) << 8 | 0x28);
    let exit = w.len();
    w[parent_guard + 1] = (exit - parent_guard) as u16;
    w.push(if wide { 0x0210 } else { 0x020f });
    code.instructions = w;
    c
}
fn dispatch_fixture(sparse: bool, wide: bool) -> DexClass {
    let mut class = variant(if wide { 4 } else { 1 });
    let code = class.methods[0].code.as_mut().unwrap();
    let original = code.instructions.clone();
    let mut words = if wide {
        vec![0x082b, 0, 0, 0x1071, 0, 8, 0x0910, 0x1071, 0, 8]
    } else {
        vec![
            0x052b, 0, 0, 0x1071, 0, 5, 0x060f, 0x1071, 0, 5, 0x5612, 0x0928, 0x1071, 0, 5, 0xe612,
            0x1071, 0, 5, 0,
        ]
    };
    if sparse {
        words[0] = (words[0] & 0xff00) | 0x2c;
    }
    words.extend(original);
    if !words.len().is_multiple_of(2) {
        words.push(0);
    }
    let payload = words.len();
    words[1] = payload as u16;
    let targets = if wide { [7, 7, 7] } else { [16, 7, 12] };
    if sparse {
        words.extend([0x0200, 3, 0, 0, 2, 0, 4, 0]);
    } else {
        words.extend([0x0100, 3, 0, 0]);
    }
    for target in targets {
        words.extend([target as u16, 0]);
    }
    code.instructions = words;
    class
}

fn reference_dispatch_fixture(sparse: bool) -> DexClass {
    let mut class = variant(0);
    class.methods[0].parameters[2] = "Ljava/lang/Object;".into();
    class.methods[0].return_type = "Ljava/lang/Object;".into();
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[1] = 0x6207;
    let body = code
        .instructions
        .windows(2)
        .position(|w| w == [0x0290, 0x0002])
        .unwrap();
    code.instructions[body] = 0x3207;
    code.instructions[body + 1] = 0;
    *code.instructions.last_mut().unwrap() = 0x0211;
    let mut words = vec![
        if sparse { 0x052c } else { 0x052b },
        0,
        0,
        0x0611,
        0x0612,
        0x0128,
    ];
    words.extend_from_slice(&code.instructions);
    if !words.len().is_multiple_of(2) {
        words.push(0);
    }
    words[1] = words.len() as u16;
    if sparse {
        words.extend([0x0200, 3, 0, 0, 2, 0, 4, 0]);
    } else {
        words.extend([0x0100, 3, 0, 0]);
    }
    words.extend([6, 0, 4, 0, 6, 0]);
    code.instructions = words;
    class
}

#[test]
fn reference_and_null_dispatch_phi_roots_remain_exact() {
    for sparse in [false, true] {
        let class = reference_dispatch_fixture(sparse);
        let code = class.methods[0].code.as_ref().unwrap();
        let selected = sole_plan(code).unwrap();
        assert!(plan::validate(code, &selected));
        let source = native_java::render_method("sample.Entry", &class, &class.methods[0])
            .unwrap()
            .source;
        assert!(source.contains("switch ("), "{source}");
    }
}

#[test]
fn dispatch_arms_keep_exact_pair_and_payload_ownership() {
    for sparse in [false, true] {
        for wide in [false, true] {
            let class = dispatch_fixture(sparse, wide);
            let code = class.methods[0].code.as_ref().unwrap();
            let selected = sole_plan(code).unwrap();
            assert!(plan::validate_all(code, std::slice::from_ref(&selected)));
            assert!(plan::validate(code, &selected));
            assert!(
                selected
                    .parent_nodes
                    .iter()
                    .all(|pc| *pc >= selected.parent)
            );
            assert!(!selected.parent_nodes.contains(&0));
            let source = native_java::render_method("sample.Entry", &class, &class.methods[0])
                .unwrap()
                .source;
            assert!(source.contains("switch ("), "{source}");
            assert!(source.contains("continue;"), "{source}");
        }
    }
}

#[test]
fn dispatch_into_pair_interior_and_payload_is_rejected() {
    for child_entry in [false, true] {
        let mut class = dispatch_fixture(false, false);
        let code = class.methods[0].code.as_mut().unwrap();
        let selected = sole_plan(code).unwrap();
        let payload = usize::from(code.instructions[1]);
        code.instructions[payload + 4] = if child_entry {
            selected.child as u16
        } else {
            (selected.parent + 3) as u16
        };
        assert!(sole_plan(code).is_none());
    }
    let mut class = dispatch_fixture(false, false);
    let code = class.methods[0].code.as_mut().unwrap();
    let payload = usize::from(code.instructions[1]);
    code.instructions[payload + 4] = payload as u16;
    assert!(sole_plan(code).is_none());
}

#[test]
fn malformed_or_unreferenced_payload_and_reachable_alignment_are_rejected() {
    for kind in 0..4 {
        let mut class = dispatch_fixture(false, false);
        let code = class.methods[0].code.as_mut().unwrap();
        let payload = usize::from(code.instructions[1]);
        match kind {
            0 => {
                code.instructions[payload] = 0x0200;
            }
            1 => {
                code.instructions[1] += 1;
            }
            2 => {
                code.instructions[0] = 0;
            }
            _ => {
                let padding = payload - 1;
                assert_eq!(code.instructions[padding], 0);
                code.instructions[payload + 4] = padding as u16;
            }
        }
        assert!(sole_plan(code).is_none(), "kind {kind}");
    }
}

#[test]
fn owned_switch_and_unreachable_effectful_instruction_are_rejected() {
    let mut class = dispatch_fixture(false, false);
    let code = class.methods[0].code.as_mut().unwrap();
    let selected = sole_plan(code).unwrap();
    let payload = code.instructions.len();
    assert!(payload.is_multiple_of(2));
    code.instructions[selected.parent] = 0x052b;
    code.instructions[selected.parent + 1] = (payload - selected.parent) as u16;
    code.instructions[selected.parent + 2] = 0;
    code.instructions.extend([0x0100, 1, 0, 0, 3, 0]);
    native_ir::DecodedMethod::decode(code).unwrap();
    native_cfg::ControlFlowGraph::build(code).unwrap();
    assert!(sole_plan(code).is_none());

    let mut class = dispatch_fixture(false, false);
    let code = class.methods[0].code.as_mut().unwrap();
    let payload = usize::from(code.instructions[1]);
    assert_eq!(code.instructions[payload - 1], 0);
    code.instructions[payload - 1] = 0x0012;
    native_ir::DecodedMethod::decode(code).unwrap();
    native_cfg::ControlFlowGraph::build(code).unwrap();
    assert!(sole_plan(code).is_none());
}

#[test]
fn duplicate_aliased_unreferenced_and_truncated_payloads_are_rejected() {
    let mut class = dispatch_fixture(false, false);
    let code = class.methods[0].code.as_mut().unwrap();
    let old_payload = usize::from(code.instructions[1]);
    let mut words = code.instructions[..3].to_vec();
    words.extend([0x052b, (old_payload + 1) as u16, 0, 0]);
    words.extend_from_slice(&code.instructions[3..]);
    let payload = old_payload + 4;
    words[1] = payload as u16;
    for offset in [4, 6, 8] {
        words[payload + offset] += 4;
    }
    code.instructions = words;
    native_ir::DecodedMethod::decode(code).unwrap();
    native_cfg::ControlFlowGraph::build(code).unwrap();
    assert!(sole_plan(code).is_none());

    let mut class = dispatch_fixture(false, false);
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.extend([0x0100, 0, 0, 0]);
    native_ir::DecodedMethod::decode(code).unwrap();
    native_cfg::ControlFlowGraph::build(code).unwrap();
    assert!(sole_plan(code).is_none());

    let mut class = dispatch_fixture(false, false);
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions.pop();
    assert!(native_ir::DecodedMethod::decode(code).is_err());
    assert!(sole_plan(code).is_none());
}

#[test]
#[ignore = "requires RDX_JAVA25_HOME; emitted source compiled unchanged"]
fn raw_dispatch_model_preserves_phi_wide_effects_and_faults() {
    for sparse in [false, true] {
        for (wide, reference) in [(false, false), (true, false), (false, true)] {
            let class = if reference {
                reference_dispatch_fixture(sparse)
            } else {
                dispatch_fixture(sparse, wide)
            };
            let harness = if reference {
                reference_harness()
            } else {
                HARNESS.to_owned()
            };
            let code = class.methods[0].code.as_ref().unwrap();
            let source = native_java::render_method("sample.Entry", &class, &class.methods[0])
                .unwrap()
                .source;
            let dir = Temp(std::env::temp_dir().join(format!(
                "rdx-parent-switch-{}-{sparse}-{wide}-{reference}",
                std::process::id()
            )));
            fs::create_dir_all(dir.0.join("sample")).unwrap();
            fs::write(
                dir.0.join("sample/Entry.java"),
                format!("package sample; public class Entry {{{source}}}"),
            )
            .unwrap();
            let words = code
                .instructions
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(",");
            fs::write(
                dir.0.join("sample/Main.java"),
                harness
                    .replace("@WORDS@", &words)
                    .replace("@REGS@", &code.registers.to_string())
                    .replace("@INS@", &code.ins.to_string())
                    .replace("@WIDE@", if wide { "true" } else { "false" }),
            )
            .unwrap();
            let home =
                std::path::PathBuf::from(std::env::var_os("RDX_JAVA25_HOME").expect("JDK25 home"));
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
                "{}",
                String::from_utf8_lossy(&run.stderr)
            );
            println!(
                "{sparse}/{wide}/{reference}: {}",
                String::from_utf8_lossy(&run.stdout)
            );
        }
    }
}
struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
const HARNESS: &str = r#"package sample;
class Effects {static Throwable FAULT;static boolean errors;static StringBuilder trace;static int calls,fault;static void reset(int f){FAULT=errors?new Error():new RuntimeException();trace=new StringBuilder();calls=0;fault=f;}static void effect(String label,int v){trace.append(label).append(v).append(';');if(++calls==fault){if(FAULT instanceof Error)throw (Error)FAULT;throw (RuntimeException)FAULT;}}static void parent(int v){effect("p",v);}static void child(int v){effect("c",v);}static boolean guard(int c,int limit,int parent){effect("h"+limit+","+parent+",",c);return c>=limit;}}
class Box {Box(){Effects.effect("a",0);}}
public class Main {
 static final int[] W={@WORDS@};static final boolean WIDE=@WIDE@;static final Object TAIL=new Object();
 static int integer(Object o){return ((Number)o).intValue();}
 static long dex(int a,int b,int seed){Object[] r=new Object[@REGS@];int entry=r.length-@INS@;r[entry]=a;r[entry+1]=b;if(WIDE){r[entry+2]=(long)seed;r[entry+3]=TAIL;}else r[entry+2]=seed;
 Object pending=null;for(int pc=0,work=0;;){if(++work>10000)throw new AssertionError("unbounded model");int word=W[pc],op=word&255,d=word>>>8;switch(op){
 case 0x2b:case 0x2c:{int table=pc+(W[pc+1]|(W[pc+2]<<16)),n=W[table+1],value=integer(r[d]),target=3;if(op==0x2b){int first=W[table+2]|(W[table+3]<<16),index=value-first;if(index>=0&&index<n)target=W[table+4+index*2]|(W[table+5+index*2]<<16);}else{for(int i=0;i<n;i++)if(value==(W[table+2+i*2]|(W[table+3+i*2]<<16)))target=W[table+2+n*2+i*2]|(W[table+3+n*2+i*2]<<16);}pc+=target;break;}
 case 0x00:pc++;break;
 case 0x12:r[(word>>>8)&15]=(int)((short)word>>12);pc++;break;
 case 0x01:r[(word>>>8)&15]=r[word>>>12];pc++;break;
 case 0x04:r[(word>>>8)&15]=r[word>>>12];r[((word>>>8)&15)+1]=TAIL;pc++;break;
 case 0x23:r[(word>>>8)&15]=new int[integer(r[word>>>12])];pc+=2;break;
 case 0x22:r[d]=new Object();pc+=2;break;
 case 0x70:r[W[pc+2]&15]=new Box();pc+=3;break;
 case 0x0a:r[d]=pending;pending=null;pc++;break;
 case 0x71:if(W[pc+1]==3){int args=W[pc+2];pending=Effects.guard(integer(r[args&15]),integer(r[(args>>>4)&15]),integer(r[(args>>>8)&15]))?1:0;}else if(W[pc+1]==0)Effects.parent(integer(r[W[pc+2]&15]));else Effects.child(integer(r[W[pc+2]&15]));pc+=3;break;
 case 0x33:pc+=integer(r[(word>>>8)&15])!=integer(r[word>>>12])?(short)W[pc+1]:2;break;
 case 0x35:pc+=integer(r[(word>>>8)&15])>=integer(r[word>>>12])?(short)W[pc+1]:2;break;
 case 0x39:pc+=integer(r[d])!=0?(short)W[pc+1]:2;break;
 case 0x90:r[d]=integer(r[W[pc+1]&255])+integer(r[W[pc+1]>>>8]);pc+=2;break;
 case 0x9b:r[d]=((Number)r[W[pc+1]&255]).longValue()+((Number)r[W[pc+1]>>>8]).longValue();r[d+1]=TAIL;pc+=2;break;
 case 0xd8:r[d]=integer(r[W[pc+1]&255])+(byte)(W[pc+1]>>>8);pc+=2;break;
 case 0x28:pc+=(byte)(word>>>8);break;
 case 0x0f:case 0x10:return ((Number)r[d]).longValue();
 default:throw new AssertionError("opcode "+op+" pc"+pc);}}}
 static String outcome(boolean model,int a,int b,int seed,int fault){Effects.reset(fault);try{long value=model?dex(a,b,seed):Entry.run(a,b,seed);return value+"|"+Effects.trace;}catch(Throwable t){return (t==Effects.FAULT?"identity":t.getClass().getName())+"|"+Effects.trace;}}
 public static void main(String[] args){int count=0;for(int a=-1;a<=5;a++)for(int b=-1;b<=5;b++)for(int seed:new int[]{-4,0,1,7})for(int fault=0;fault<=30;fault++)for(boolean error:new boolean[]{false,true}){Effects.errors=error;String want=outcome(true,a,b,seed,fault),got=outcome(false,a,b,seed,fault);if(!want.equals(got))throw new AssertionError(a+","+b+","+seed+","+fault+" expected "+want+" got "+got);count++;}System.out.println(count+" exact DEX-register comparisons");}
}
"#;
fn reference_harness() -> String {
    let mut harness = HARNESS
        .replace(
            "static long dex(int a,int b,int seed)",
            "static Object dex(int a,int b,Object seed)",
        )
        .replace(
            "class Box {Box(){Effects.effect(\"a\",0);}}",
            "class Box {final int id;Box(){id=Effects.calls;Effects.effect(\"a\",0);}}",
        )
        .replace("case 0x01:", "case 0x07:case 0x01:")
        .replace(
            "case 0x0f:case 0x10:",
            "case 0x11:return r[d];case 0x0f:case 0x10:",
        );
    harness = harness.replace(
        "case 0x12:r[",
        "case 0x12:if(pc==4){r[6]=null;pc++;break;}r[",
    );
    let start = harness.find(" static String outcome(").unwrap();
    let end = harness.find(" public static void main(").unwrap();
    harness.replace_range(start..end, r#" static String outcome(boolean model,int a,int b,int seed,int fault){Effects.reset(fault);Object token=new Object();try{Object value=model?dex(a,b,token):Entry.run(a,b,token);return (value==null?"null":value==token?"seed"+seed:"box"+((Box)value).id)+"|"+Effects.trace;}catch(Throwable t){return (t==Effects.FAULT?"identity":t.getClass().getName())+"|"+Effects.trace;}}
"#);
    harness
}
