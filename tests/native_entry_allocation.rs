pub use rdx::{native_cfg, native_dominators, native_ir, native_ssa};
#[path = "../src/native_java/forward_iteration_entry.rs"]
mod plan;
use rdx::{
    native_dex::{self, DexClass, DexSymbols},
    native_java,
};
use std::sync::Arc;
fn fixture() -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.descriptor = "Lsample/Entry;".into();
    c.superclass = Some("Ljava/lang/Object;".into());
    c.fields.clear();
    c.interfaces.clear();
    c.methods.retain(|m| m.name.as_ref() == "answer");
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.declaring_type = c.descriptor.clone();
    m.return_type = "I".into();
    m.thrown_types = vec!["Lsample/Fault;".into()];
    m.parameters = vec!["I".into()];
    m.access_flags = 9;
    c.symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Effects;".into(), "Lsample/Fault;".into()],
        strings: vec!["tick".into(), "<init>".into()],
        protos: vec![("V".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (1, 0, 1)],
        ..Default::default()
    });
    let code = m.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 1;
    code.outs = 2;
    code.instructions = vec![
        0x0012, 0x0428, 0x1071, 0, 0, 0x3034, 8, 0x0222, 1, 0x2070, 1, 2, 0x0227, 0x00d8, 0x0100,
        0xf328,
    ];
    let mut fault = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    fault.descriptor = "Lsample/Fault;".into();
    fault.superclass = Some("Ljava/lang/Exception;".into());
    fault.interfaces.clear();
    fault.methods.clear();
    fault.fields.clear();
    c.symbols
        .hierarchy
        .set(Arc::new(
            rdx::native_hierarchy::TypeHierarchy::from_classes([&c, &fault]).unwrap(),
        ))
        .unwrap();
    c
}
fn constant_fixture(variant: usize) -> DexClass {
    let mut c = fixture();
    let constant = match variant {
        0 => vec![],
        1 => vec![0x7112],
        2 => vec![0x011a, 2],
        3 => vec![0x011b, 2, 0],
        _ => unreachable!(),
    };
    let len = constant.len();
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    words.splice(9..9, constant);
    words[6] += len as u16;
    words[15 + len] = (((-13i16 - len as i16) as i8 as u8 as u16) << 8) | 0x28;
    if variant != 0 {
        words[11 + len] = 0x12;
    }
    if variant >= 2 {
        let symbols = Arc::get_mut(&mut c.symbols).unwrap();
        symbols.strings.push("argument".into());
        symbols
            .protos
            .push(("V".into(), vec!["Ljava/lang/String;".into()]));
        symbols.methods[1].1 = 1;
    }
    c
}

fn retargeted_fixture(variant: usize, negative: usize) -> DexClass {
    let mut c = constant_fixture(variant);
    let args = {
        let s = Arc::get_mut(&mut c.symbols).unwrap();
        s.hierarchy = Default::default();
        s.types.push("Lsample/BaseFault;".into());
        s.methods[1].0 = 2;
        s.protos[s.methods[1].1 as usize].1.clone()
    };
    let empty = |descriptor: &str, parent: &str| {
        let mut x = native_dex::parse(include_bytes!("fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        x.descriptor = descriptor.into();
        x.superclass = Some(parent.into());
        x.fields.clear();
        x.methods.clear();
        x.interfaces.clear();
        x.access_flags = 1;
        x
    };
    let mut fault = empty("Lsample/Fault;", "Lsample/BaseFault;");
    let mut parent = empty("Lsample/BaseFault;", "Ljava/lang/RuntimeException;");
    parent.methods.push(native_dex::DexMethod {
        declaring_type: parent.descriptor.clone(),
        name: "<init>".into(),
        parameters: args.clone(),
        return_type: "V".into(),
        thrown_types: vec![],
        access_flags: if negative == 1 { 2 } else { 1 },
        code: Some(native_dex::DexCode {
            registers: 2,
            ins: 2,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            instructions: vec![0x1070, 0, 0, 0x000e],
            offset: 0,
        }),
    });
    parent.symbols = Arc::new(DexSymbols {
        types: vec!["Ljava/lang/RuntimeException;".into()],
        strings: vec!["<init>".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    if negative == 2 {
        parent.methods[0].parameters = vec!["J".into()];
    }
    if negative == 3 {
        fault.methods.push(native_dex::DexMethod {
            declaring_type: fault.descriptor.clone(),
            name: "<init>".into(),
            parameters: args.clone(),
            return_type: "V".into(),
            thrown_types: vec![],
            access_flags: 1,
            code: None,
        });
    }
    let hierarchy = if negative == 4 {
        rdx::native_hierarchy::TypeHierarchy::from_classes([&c, &fault]).unwrap()
    } else if negative == 5 {
        rdx::native_hierarchy::TypeHierarchy::from_classes([&c, &fault, &parent, &parent]).unwrap()
    } else {
        rdx::native_hierarchy::TypeHierarchy::from_classes([&c, &fault, &parent]).unwrap()
    };
    assert_eq!(
        hierarchy.equivalent_constructor("Lsample/Fault;", "Lsample/BaseFault;", &args),
        negative == 0,
        "variant {variant} negative {negative}"
    );
    c.symbols.hierarchy.set(Arc::new(hierarchy)).unwrap();
    c
}
#[test]
fn entry_island_allocation_identity_accepts_exact_recovered_forwarder() {
    for variant in 0..=3 {
        let c = retargeted_fixture(variant, 0);
        let code = c.methods[0].code.as_ref().unwrap();
        let p = plan::select(code).unwrap();
        assert_eq!(p.allocation_islands.len(), 1);
        let rendered = native_java::render_method("sample.Entry", &c, &c.methods[0])
            .unwrap_or_else(|e| panic!("variant {variant}: {e}"));
        assert!(rendered.source.contains("new sample.Fault("));
        assert!(format!("{:?}", rendered.links).contains("sample.BaseFault.<init>"));
    }
}
#[test]
fn entry_forwarder_requires_access_exact_proto_and_unambiguous_loaded_metadata() {
    for variant in 0..=3 {
        for negative in 1..=5 {
            let c = retargeted_fixture(variant, negative);
            assert!(
                native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err(),
                "variant {variant} negative {negative}"
            );
        }
    }
}
#[test]
fn numeric_string_and_jumbo_constants_keep_constructor_navigation() {
    for variant in 1..=3 {
        let c = constant_fixture(variant);
        assert!(plan::select(c.methods[0].code.as_ref().unwrap()).is_some());
        let r = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
        let descriptor = if variant == 1 {
            "I"
        } else {
            "Ljava/lang/String;"
        };
        assert!(format!("{:?}", r.links).contains(&format!("sample.Fault.<init>({descriptor})V")));
    }
}
#[test]
fn island_constant_budget_and_class_resolution_are_guarded() {
    let mut c = fixture();
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    words.splice(9..9, [0x1112; 5]);
    words[6] = 13;
    words[20] = 0xee28;
    assert!(rdx::native_method::MethodAnalysis::build(&c, &c.methods[0]).is_ok());
    assert!(plan::select(c.methods[0].code.as_ref().unwrap()).is_none());
    let mut c = constant_fixture(2);
    let symbols = Arc::get_mut(&mut c.symbols).unwrap();
    symbols.protos[1].1[0] = "Ljava/lang/Class;".into();
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    words[9] = 0x011c;
    words[10] = 0;
    assert!(rdx::native_method::MethodAnalysis::build(&c, &c.methods[0]).is_ok());
    assert!(plan::select(c.methods[0].code.as_ref().unwrap()).is_none());
}
#[test]
fn closed_constructor_copy_has_raw_identity_and_exact_navigation() {
    let c = fixture();
    let p = plan::select(c.methods[0].code.as_ref().unwrap()).unwrap();
    assert_eq!(p.allocation_islands.len(), 1);
    assert_eq!(
        (
            p.allocation_islands[0].allocation,
            p.allocation_islands[0].initialize
        ),
        (7, 9)
    );
    assert!(plan::validate(c.methods[0].code.as_ref().unwrap(), &p));
    let r = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
    println!("{}", r.source);
    assert!(format!("{:?}", r.links).contains("sample.Fault.<init>(I)V"));
}
#[test]
fn islands_reject_uninitialized_uses_foreign_entry_and_wrong_constructor() {
    for mutation in 0..5 {
        let mut c = fixture();
        let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
        match mutation {
            0 => words[9] = 0x2071,
            1 => words[11] = 0x22,
            2 => words[1] = 0x0828,
            3 => Arc::get_mut(&mut c.symbols).unwrap().strings[1] = "other".into(),
            _ => words[11] = 0x12,
        };
        assert!(
            native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn raw_island_plan_rejects_identity_or_ownership_mutation() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = plan::select(code).unwrap();
    for mutation in 0..4 {
        let mut bad = p.clone();
        let island = &mut bad.allocation_islands[0];
        match mutation {
            0 => island.receiver += 1,
            1 => island.allocation += 1,
            2 => island.initialize += 1,
            _ => {
                island.instructions.pop();
            }
        }
        assert!(!plan::validate(code, &bad));
    }
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.tries = 1;
    code.try_regions = vec![native_dex::DexTryRegion {
        start: 7,
        end: 12,
        catches: Arc::from(vec![(None, 12)]),
    }];
    assert!(plan::select(code).is_none());
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[7] = 0x0223;
    assert!(plan::select(c.methods[0].code.as_ref().unwrap()).is_none());
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[0] = 0x1001;
    assert!(rdx::native_method::MethodAnalysis::build(&c, &c.methods[0]).is_ok());
    assert!(plan::select(c.methods[0].code.as_ref().unwrap()).is_none());
    let mut c = fixture();
    let words = &mut c.methods[0].code.as_mut().unwrap().instructions;
    words.splice(9..9, [0x0118, 0, 0, 0, 0]);
    words[6] = 13;
    words[20] = 0xee28;
    assert!(rdx::native_method::MethodAnalysis::build(&c, &c.methods[0]).is_ok());
    assert!(plan::select(c.methods[0].code.as_ref().unwrap()).is_none());
    let mut c = fixture();
    Arc::get_mut(&mut c.symbols).unwrap().methods[1].0 = 0;
    assert!(native_java::render_method("sample.Entry", &c, &c.methods[0]).is_err());
}

#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn copied_constructor_jvm_keeps_initial_entry_effects_and_fault_identity() {
    use std::{fs, process::Command};
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    for variant in 0..=3 {
        let c = constant_fixture(variant);
        let r = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
        let dir = std::env::temp_dir().join(format!("rdx-entry-allocation-{}", std::process::id()));
        let _cleanup = Cleanup(dir.clone());
        fs::create_dir_all(dir.join("sample")).unwrap();
        let java = r#"package sample;
class Effects {static String trace;static int calls,failAt;static Fault last;static final RuntimeException fault=new RuntimeException();static void record(String s){trace+=s+",";if(++calls==failAt)throw fault;}static void tick(int value){record("tick"+value);}}
class Fault extends Exception {static{Effects.record("clinit");}final int value;Fault(int value){Effects.record("ctor"+value);this.value=value;Effects.last=this;}Fault(String value){Effects.record("ctorstring"+value);this.value=value.length();Effects.last=this;}}
public class Entry {
METHOD
static int oracle(int n)throws Fault{int value=0;while(value<n){value++;Effects.tick(value);}throw new Fault(ORACLEARG);}
static Throwable evaluate(int n,boolean generated){Effects.calls=0;Effects.trace="";Effects.last=null;try{if(generated)run(n);else oracle(n);throw new AssertionError("missing throw");}catch(Fault x){if(x!=Effects.last)throw new AssertionError("allocated identity");return x;}catch(ExceptionInInitializerError x){if(x.getException()!=Effects.fault)throw new AssertionError("initializer cause identity");return x;}catch(RuntimeException x){if(x!=Effects.fault)throw new AssertionError("callback identity");return x;}}
public static void main(String[]args)throws Exception{if(args.length>0){Effects.failAt=Integer.parseInt(args[1]);Throwable x=evaluate(0,args[0].equals("actual"));System.out.println(Effects.trace+Effects.calls+":"+x.getClass().getName()+":"+(x instanceof Fault?((Fault)x).value:-1));return;}Class.forName("sample.Fault");for(int n=-2;n<=6;n++)for(int fail=0;fail<=9;fail++){Effects.failAt=fail;Throwable a=evaluate(n,true);String trace=Effects.trace;int calls=Effects.calls;Throwable b=evaluate(n,false);if(a.getClass()!=b.getClass()||a instanceof Fault&&((Fault)a).value!=((Fault)b).value||calls!=Effects.calls||!trace.equals(Effects.trace))throw new AssertionError(trace+" != "+Effects.trace);}}
}"#;
        fs::write(
            dir.join("sample/Entry.java"),
            java.replace("METHOD", &r.source).replace(
                "ORACLEARG",
                match variant {
                    0 => "value",
                    1 => "7",
                    _ => "\"argument\"",
                },
            ),
        )
        .unwrap();
        let home = std::env::var("RDX_JAVA25_HOME").unwrap();
        for (program, args) in [
            ("javac", vec!["sample/Entry.java"]),
            ("java", vec!["-cp", ".", "sample.Entry"]),
        ] {
            let out = Command::new(format!("{home}/bin/{program}"))
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stderr),
                r.source
            );
        }
        if variant > 0 {
            let bytecode = Command::new(format!("{home}/bin/javap"))
                .args(["-c", "-p", "-classpath", ".", "sample.Entry"])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(bytecode.status.success());
            let text = String::from_utf8(bytecode.stdout).unwrap();
            let method = text
                .split("public static int run")
                .nth(1)
                .unwrap()
                .split("static int oracle")
                .next()
                .unwrap();
            let new = method.find("class sample/Fault").unwrap();
            let constant = method
                .find(if variant == 1 {
                    "bipush"
                } else {
                    "String argument"
                })
                .unwrap();
            let constructor = method
                .find(if variant == 1 {
                    "(I)V"
                } else {
                    "(Ljava/lang/String;)V"
                })
                .unwrap();
            assert!(new < constant && constant < constructor, "{method}");
        }
        for fail in ["0", "1"] {
            let cold: Vec<_> = ["actual", "oracle"]
                .into_iter()
                .map(|mode| {
                    let out = Command::new(format!("{home}/bin/java"))
                        .args(["-cp", ".", "sample.Entry", mode, fail])
                        .current_dir(&dir)
                        .output()
                        .unwrap();
                    assert!(
                        out.status.success(),
                        "{}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    out.stdout
                })
                .collect();
            assert_eq!(cold[0], cold[1]);
            assert!(
                String::from_utf8_lossy(&cold[0]).starts_with(if fail == "0" {
                    if variant == 0 {
                        "clinit,ctor0,"
                    } else if variant == 1 {
                        "clinit,ctor7,"
                    } else {
                        "clinit,ctorstringargument,"
                    }
                } else {
                    "clinit,1:java.lang.ExceptionInInitializerError"
                })
            );
        }
    }
}

#[test]
#[ignore = "requires JDK25 via RDX_JAVA25_HOME"]
fn recovered_copy_constructor_jvm_keeps_cold_initialization_bytecode_order_and_fault_identity() {
    use std::{fs, process::Command};
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    for variant in 0..=3 {
        let c = retargeted_fixture(variant, 0);
        let r = native_java::render_method("sample.Entry", &c, &c.methods[0]).unwrap();
        let dir = std::env::temp_dir().join(format!("rdx-entry-forwarder-{}", std::process::id()));
        let _cleanup = Cleanup(dir.clone());
        fs::create_dir_all(dir.join("sample")).unwrap();
        let java = r#"package sample;
class Effects {static String trace;static int calls,failAt;static Fault last;static final RuntimeException fault=new RuntimeException();static void record(String s){trace+=s+",";if(++calls==failAt)throw fault;}static void tick(int value){record("tick"+value);}}
class BaseFault extends RuntimeException {static{Effects.record("baseclinit");}int value;static void initialize(BaseFault self,int value){Effects.record("ctor"+value);self.value=value;Effects.last=(Fault)self;}static void initialize(BaseFault self,String value){Effects.record("ctorstring"+value);self.value=value.length();Effects.last=(Fault)self;}BaseFault(int value){initialize(this,value);}BaseFault(String value){initialize(this,value);}}
class Fault extends BaseFault {static{Effects.record("clinit");}Fault(int value){super(value);}Fault(String value){super(value);}}
public class Entry {
METHOD
static sun.misc.Unsafe allocator(){try{java.lang.reflect.Field f=sun.misc.Unsafe.class.getDeclaredField("theUnsafe");f.setAccessible(true);return(sun.misc.Unsafe)f.get(null);}catch(Exception e){throw new AssertionError(e);}}
static final sun.misc.Unsafe U=allocator();static final int[]W={RAWWORDS};
static int oracle(int n){int[]r=new int[4];Object[]o=new Object[4];r[3]=n;int pc=0,budget=100000;while(--budget>0){int w=W[pc],a=w>>>8;switch(w&255){
case 0x12:r[a&15]=(a>>>4)>=8?(a>>>4)-16:a>>>4;pc++;break;case 0x28:pc+=(byte)a;break;case 0x34:pc+=r[a&15]<r[a>>>4]?(short)W[pc+1]:2;break;
case 0x71:Effects.tick(r[W[pc+2]&15]);pc+=3;break;
case 0x22:try{o[a]=U.allocateInstance(Fault.class);}catch(InstantiationException x){throw new AssertionError(x);}pc+=2;break;
case 0x1a:o[a]="argument";pc+=2;break;case 0x1b:o[a]="argument";pc+=3;break;
case 0x70:{int x=W[pc+2],receiver=x&15,arg=(x>>>4)&15; if(o[arg] instanceof String)BaseFault.initialize((BaseFault)o[receiver],(String)o[arg]);else BaseFault.initialize((BaseFault)o[receiver],r[arg]);pc+=3;break;}
case 0x27:throw(Fault)o[a];case 0xd8:r[a]=r[W[pc+1]&255]+(byte)(W[pc+1]>>>8);pc+=2;break;default:throw new AssertionError("opcode "+w+" at "+pc);}}throw new AssertionError("budget");}
static Throwable evaluate(int n,boolean generated){Effects.calls=0;Effects.trace="";Effects.last=null;try{if(generated)run(n);else oracle(n);throw new AssertionError("missing throw");}catch(Fault x){if(x!=Effects.last)throw new AssertionError("allocated identity");return x;}catch(ExceptionInInitializerError x){if(x.getException()!=Effects.fault)throw new AssertionError("initializer cause identity");return x;}catch(RuntimeException x){if(x!=Effects.fault)throw new AssertionError("callback identity");return x;}}
public static void main(String[]args)throws Exception{if(args.length>0){Effects.failAt=Integer.parseInt(args[1]);Throwable x=evaluate(0,args[0].equals("actual"));System.out.println(Effects.trace+Effects.calls+":"+x.getClass().getName()+":"+(x instanceof Fault?((Fault)x).value:-1));return;}Class.forName("sample.Fault");for(int n=-2;n<=40;n++)for(int fail=0;fail<=45;fail++){Effects.failAt=fail;Throwable a=evaluate(n,true);String trace=Effects.trace;int calls=Effects.calls;Throwable b=evaluate(n,false);if(a.getClass()!=b.getClass()||a instanceof Fault&&((Fault)a).value!=((Fault)b).value||calls!=Effects.calls||!trace.equals(Effects.trace))throw new AssertionError(trace+" != "+Effects.trace);}}
}"#;
        fs::write(
            dir.join("sample/Entry.java"),
            java.replace("METHOD", &r.source)
                .replace(
                    "RAWWORDS",
                    &c.methods[0]
                        .code
                        .as_ref()
                        .unwrap()
                        .instructions
                        .iter()
                        .map(|w| w.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                )
                .replace(
                    "ORACLEARG",
                    match variant {
                        0 => "value",
                        1 => "7",
                        _ => "\"argument\"",
                    },
                ),
        )
        .unwrap();
        let home = std::env::var("RDX_JAVA25_HOME").unwrap();
        for (program, args) in [
            ("javac", vec!["sample/Entry.java"]),
            ("java", vec!["-cp", ".", "sample.Entry"]),
        ] {
            let out = Command::new(format!("{home}/bin/{program}"))
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stderr),
                r.source
            );
        }
        if variant > 0 {
            let bytecode = Command::new(format!("{home}/bin/javap"))
                .args(["-c", "-p", "-classpath", ".", "sample.Entry"])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(bytecode.status.success());
            let text = String::from_utf8(bytecode.stdout).unwrap();
            let method = text
                .split("public static int run")
                .nth(1)
                .unwrap()
                .split("static int oracle")
                .next()
                .unwrap();
            let new = method.find("class sample/Fault").unwrap();
            let constant = method
                .find(if variant == 1 {
                    "bipush"
                } else {
                    "String argument"
                })
                .unwrap();
            let constructor = method
                .find(if variant == 1 {
                    "(I)V"
                } else {
                    "(Ljava/lang/String;)V"
                })
                .unwrap();
            assert!(new < constant && constant < constructor, "{method}");
        }
        for fail in ["0", "1", "2"] {
            let cold: Vec<_> = ["actual", "oracle"]
                .into_iter()
                .map(|mode| {
                    let out = Command::new(format!("{home}/bin/java"))
                        .args(["-cp", ".", "sample.Entry", mode, fail])
                        .current_dir(&dir)
                        .output()
                        .unwrap();
                    assert!(
                        out.status.success(),
                        "{}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    out.stdout
                })
                .collect();
            assert_eq!(cold[0], cold[1]);
            assert!(
                String::from_utf8_lossy(&cold[0]).starts_with(if fail == "0" {
                    if variant == 0 {
                        "baseclinit,clinit,ctor0,"
                    } else if variant == 1 {
                        "baseclinit,clinit,ctor7,"
                    } else {
                        "baseclinit,clinit,ctorstringargument,"
                    }
                } else if fail == "1" {
                    "baseclinit,1:java.lang.ExceptionInInitializerError"
                } else {
                    "baseclinit,clinit,2:java.lang.ExceptionInInitializerError"
                })
            );
        }
    }
}
