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
fn insert_words(code: &mut native_dex::DexCode, at: usize, extra: &[u16]) {
    let decoded = native_ir::DecodedMethod::decode(code).unwrap();
    let mut words = code.instructions[..at].to_vec();
    words.extend_from_slice(extra);
    words.extend_from_slice(&code.instructions[at..]);
    for instruction in decoded.instructions {
        let Some(target) = instruction.branch_target else {
            continue;
        };
        let from = instruction.pc + usize::from(instruction.pc >= at) * extra.len();
        let to = target + usize::from(target >= at) * extra.len();
        let delta = to as isize - from as isize;
        match instruction.opcode {
            0x28 => {
                assert!((-128..=127).contains(&delta));
                words[from] = ((delta as i8 as u8 as u16) << 8) | 0x28;
            }
            0x32..=0x3d => words[from + 1] = delta as i16 as u16,
            opcode => panic!("unexpected branch {opcode}"),
        }
    }
    code.instructions = words;
}

fn field_fixture(mode: usize, opcode: u16, descriptor: &str) -> DexClass {
    let mut c = variant(if mode == 3 { 5 } else { 0 });
    let mut symbols = DexSymbols {
        types: c.symbols.types.clone(),
        strings: c.symbols.strings.clone(),
        protos: c.symbols.protos.clone(),
        methods: c.symbols.methods.clone(),
        ..Default::default()
    };
    symbols
        .types
        .extend(["Lsample/Holder;".into(), descriptor.into()]);
    let name = symbols.strings.len() as u32;
    symbols.strings.push("limit".into());
    symbols.fields.push((3, 4, name));
    c.symbols = Arc::new(symbols);
    let method = &mut c.methods[0];
    if opcode == 0x55 && mode != 1 {
        method.parameters[1] = "Z".into();
    }
    method.parameters.push("Lsample/Holder;".into());
    let code = method.code.as_mut().unwrap();
    code.registers = 8;
    code.ins = 4;
    let old = sole_plan(code).unwrap();
    insert_words(
        code,
        old.child,
        &[if mode == 1 { 0x7400 } else { 0x7500 } | opcode, 0],
    );
    code.instructions[old.child_latch + 2] =
        (((old.child as isize - (old.child_latch + 2) as isize) as i8 as u8 as u16) << 8) | 0x28;
    let guard = old.child + 2;
    code.instructions[guard] = if opcode == 0x55 {
        0x0539
    } else if mode == 1 {
        0x4135
    } else {
        0x5135
    };
    if mode == 2 {
        method.parameters.push("Lsample/Holder;".into());
        code.registers = 9;
        code.ins = 5;
        insert_words(code, old.parent, &[0x8707]);
    }
    c
}
#[test]
fn exact_field_header_has_owned_guard_frame_and_raw_identity() {
    for mode in 0..4 {
        for (op, ty) in [
            (0x52, "I"),
            (0x55, "Z"),
            (0x56, "B"),
            (0x57, "C"),
            (0x58, "S"),
        ] {
            let c = field_fixture(mode, op, ty);
            let code = c.methods[0].code.as_ref().unwrap();
            if mode == 1 && op == 0x55 {
                continue;
            }
            let p = sole_plan(code).unwrap();
            assert_eq!(p.child_guard, p.child + 2);
            assert!(plan::validate(code, &p));
            assert!(plan::validate_all(code, std::slice::from_ref(&p)));
            let emitted = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
            assert!(emitted.source.contains(".limit"));
            let link = emitted
                .links
                .iter()
                .find(|link| link.label == format!("sample.Holder.limit:{ty}"))
                .unwrap();
            assert_eq!(
                emitted
                    .source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>(),
                "limit"
            );
            let mut poisoned = p.clone();
            poisoned.words[p.child + 1] ^= 1;
            assert!(!plan::validate(code, &poisoned));
        }
    }
}
#[test]
fn wide_object_extra_prefix_foreign_entry_and_undefined_field_receiver_refuse() {
    for mutation in 0..8 {
        let mut c = field_fixture(0, 0x52, "I");
        let code = c.methods[0].code.as_mut().unwrap();
        let p = sole_plan(code).unwrap();
        match mutation {
            0 => code.instructions[p.child] = (code.instructions[p.child] & 0xff00) | 0x53,
            1 => code.instructions[p.child] = (code.instructions[p.child] & 0xff00) | 0x54,
            2 => {
                insert_words(code, p.child, &[0]);
                code.instructions[p.child_latch + 1] =
                    (((p.child as isize - (p.child_latch + 1) as isize) as i8 as u8 as u16) << 8)
                        | 0x28;
            }
            3 => code.instructions[p.child_guard] = 0x3135,
            4 => {
                code.instructions[p.parent_guard] = 0x0032;
                code.instructions[p.parent_guard + 1] = (p.child_guard - p.parent_guard) as u16;
            }
            5 => {
                code.registers = 9;
                code.ins = 4;
            }
            6 => code.tries = 1,
            7 => {
                code.instructions[2] = 0;
                code.instructions[3] = 0;
                code.instructions[p.child] = 0x3552;
            }
            _ => unreachable!(),
        }
        assert!(plan::select_all(code).is_none(), "mutation{mutation}");
    }
}
#[test]
fn descriptor_width_and_reference_mismatches_refuse_typed_emission() {
    for descriptor in ["J", "D", "Ljava/lang/Object;", "F"] {
        let c = field_fixture(0, 0x52, descriptor);
        assert!(
            native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err(),
            "descriptor{descriptor}"
        );
    }
}
struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
const HARNESS: &str = r#"package sample;
class Effects {static final RuntimeException FAULT=new RuntimeException();static StringBuilder trace;static int calls,fault;static void reset(int f){trace=new StringBuilder();calls=0;fault=f;}static void effect(String label,int v){trace.append(label).append(v).append(';');if(++calls==fault)throw FAULT;}static void parent(int v){effect("p",v);}static void child(int v){effect("c",v);}static boolean guard(int c,int limit,int parent){effect("h"+limit+","+parent+",",c);return c>=limit;}}
class Box {Box(){Effects.effect("a",0);}}
public class Main {
 static final int[] W={@WORDS@};static final boolean WIDE=@WIDE@;static final Object TAIL=new Object();
 static int integer(Object o){return ((Number)o).intValue();}
 static long dex(int a,int b,int seed){Object[] r=new Object[@REGS@];int entry=r.length-@INS@;r[entry]=a;r[entry+1]=b;if(WIDE){r[entry+2]=(long)seed;r[entry+3]=TAIL;}else r[entry+2]=seed;
 Object pending=null;for(int pc=0,work=0;;){if(++work>10000)throw new AssertionError("unbounded model");int word=W[pc],op=word&255,d=word>>>8;switch(op){
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
 static String outcome(boolean model,int a,int b,int seed,int fault){Effects.reset(fault);try{long value=model?dex(a,b,seed):Entry.run(a,b,seed);return value+"|"+Effects.trace;}catch(Throwable t){return ((t==Effects.FAULT||t==Effects.ERROR)?"identity":t.getClass().getName())+"|"+Effects.trace;}}
 public static void main(String[] args){int count=0;for(int a=-1;a<=5;a++)for(int b=-1;b<=5;b++)for(int seed:new int[]{-4,0,1,7})for(int fault=0;fault<=30;fault++){String want=outcome(true,a,b,seed,fault),got=outcome(false,a,b,seed,fault);if(!want.equals(got))throw new AssertionError(a+","+b+","+seed+","+fault+" expected "+want+" got "+got);count++;}System.out.println(count+" exact DEX-register comparisons");}
}
"#;

fn field_harness(mode: usize, ty: &str, volatile: bool) -> String {
    let mut h = HARNESS.replace(
        "class Box {",
        &format!(
            "class Holder {{ {} {ty} limit; Holder(int b) {{limit={};}} }}\nclass Box {{",
            if volatile { "volatile" } else { "" },
            if ty == "boolean" {
                "b<=0".to_owned()
            } else if ty == "char" {
                "(char)Math.max(0,b)".to_owned()
            } else {
                "(".to_owned() + ty + ")b"
            }
        ),
    );
    h = h.replace(
        "static int calls,fault;",
        "static int calls,fault,mutate,b; static Holder holder;",
    );
    let value = if ty == "boolean" {
        "v>=b".to_owned()
    } else {
        format!("({ty})(mutate==1?Math.max(1,b-v):mutate==2?b+1:b)")
    };
    h = h.replace(
        "static void child(int v){effect(\"c\",v);}",
        &format!(
            "static void child(int v){{effect(\"c\",v);if(holder!=null)holder.limit={value};}}"
        ),
    );
    h = h.replace(
        "static long dex(int a,int b,int seed)",
        "static long dex(int a,int b,int seed,Holder holder,Holder alias)",
    );
    h = h.replace(
        "Object pending=null;",
        "r[entry+3]=holder;if(@INS@==5)r[entry+4]=alias;Object pending=null;",
    );
    h = h.replace("case 0x01:", "case 0x07:case 0x01:");
    let read = if ty == "boolean" {
        "((Holder)r[word>>>12]).limit?1:0"
    } else {
        "(int)((Holder)r[word>>>12]).limit"
    };
    h=h.replace("case 0x00:",&format!("case 0x52:case 0x55:case 0x56:case 0x57:case 0x58:r[(word>>>8)&15]={read};pc+=2;break;\ncase 0x00:"));
    h=h.replace("static final RuntimeException FAULT=new RuntimeException();","static final RuntimeException FAULT=new RuntimeException();static final Error ERROR=new Error();static boolean error;");
    h = h.replace(
        "if(++calls==fault)throw FAULT;",
        "if(++calls==fault){if(error)throw ERROR;throw FAULT;}",
    );
    let begin = h.find(" static String outcome(").unwrap();
    h.replace_range(begin.., &format!(r#" static String outcome(boolean model,int a,int b,int seed,int fault,int mutation,boolean nil){{Effects.reset(fault);Effects.mutate=mutation;Effects.b=b;Holder holder=nil?null:new Holder(b);Holder alias=nil?null:new Holder(b);Effects.holder={};try{{long value=model?dex(a,b,seed,holder,alias):Entry.run(a,{},seed,holder{});return value+"|"+Effects.trace+"|"+(Effects.holder==null?"null":Effects.holder.limit);}}catch(Throwable t){{return ((t==Effects.FAULT||t==Effects.ERROR)?"identity":t.getClass().getName())+"|"+Effects.trace+"|"+(Effects.holder==null?"null":Effects.holder.limit);}}}}
 public static void main(String[]args){{int count=0;for(int a=-1;a<=5;a++)for(int b=-1;b<=5;b++)for(int seed:new int[]{{-4,0,1,7}})for(int fault=0;fault<=30;fault++)for(int mutation=0;mutation<3;mutation++)for(boolean nil:new boolean[]{{false,true}})for(boolean error:new boolean[]{{false,true}}){{Effects.error=error;String want=outcome(true,a,b,seed,fault,mutation,nil),got=outcome(false,a,b,seed,fault,mutation,nil);if(!want.equals(got))throw new AssertionError(a+","+b+","+seed+","+fault+","+mutation+","+nil+" expected "+want+" got "+got);count++;}}System.out.println(count+" exact DEX-register comparisons");}}
}}
"#,if mode==2 {"alias"}else{"holder"},if ty=="boolean"{"b<=0"}else{"b"},if mode==2{",alias"}else{""}));
    if mode == 2 {
        h = h.replace(
            "Holder holder=nil?null:new Holder(b);",
            "Holder holder=nil?new Holder(b+2):null;",
        );
    }
    h
}
#[test]
#[ignore = "requires JDK25; unchanged field-loop source versus raw register model"]
fn per_iteration_field_reads_preserve_aliases_volatile_values_null_faults_and_terminals() {
    let home = std::env::var_os("RDX_JAVA25_HOME").map(std::path::PathBuf::from);
    let tool = |name: &str| {
        home.as_ref()
            .map(|p| p.join("bin").join(name))
            .unwrap_or_else(|| name.into())
    };
    for (kind, (mode, op, ty, descriptor, volatile)) in [
        (0, 0x52, "int", "I", false),
        (0, 0x52, "int", "I", true),
        (1, 0x52, "int", "I", false),
        (2, 0x52, "int", "I", true),
        (3, 0x52, "int", "I", false),
        (0, 0x55, "boolean", "Z", true),
        (0, 0x56, "byte", "B", false),
        (0, 0x57, "char", "C", false),
        (0, 0x58, "short", "S", true),
    ]
    .into_iter()
    .enumerate()
    {
        let c = field_fixture(mode, op, descriptor);
        let code = c.methods[0].code.as_ref().unwrap();
        let source = native_java::render_method("sample.Entry", &c, &c.methods[0])
            .unwrap()
            .source;
        let dir = Temp(
            std::env::temp_dir().join(format!("rdx-field-guard-{}-{kind}", std::process::id())),
        );
        fs::create_dir_all(dir.0.join("sample")).unwrap();
        fs::write(
            dir.0.join("sample/Entry.java"),
            format!("package sample;public class Entry{{{source}}}"),
        )
        .unwrap();
        let h = field_harness(mode, ty, volatile)
            .replace(
                "@WORDS@",
                &code
                    .instructions
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .replace("@REGS@", &code.registers.to_string())
            .replace("@INS@", &code.ins.to_string())
            .replace("@WIDE@", "false");
        fs::write(dir.0.join("sample/Main.java"), h).unwrap();
        let output = Command::new(tool("javac"))
            .arg("-d")
            .arg(&dir.0)
            .arg(dir.0.join("sample/Entry.java"))
            .arg(dir.0.join("sample/Main.java"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "kind{kind}: {}\n{source}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = Command::new(tool("java"))
            .arg("-Xverify:all")
            .arg("-cp")
            .arg(&dir.0)
            .arg("sample.Main")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "kind{kind}: {}\n{}\n{source}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        eprintln!("kind{kind}: {}", String::from_utf8_lossy(&output.stdout));
    }
}
