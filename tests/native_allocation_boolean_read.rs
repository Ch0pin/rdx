pub use rdx::{native_dex, native_hierarchy, native_ir, native_java, native_ssa, native_types};
#[allow(dead_code)]
#[path = "../src/native_java/literal_read_proof.rs"]
mod proof;
use native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols};
use std::sync::Arc;
fn fixture(other: u16, later: u16) -> Vec<DexClass> {
    let sy = Arc::new(DexSymbols {
        types: vec![
            "Lsample/Target;".into(),
            "Lsample/Effects;".into(),
            "Ljava/lang/Object;".into(),
            "Z".into(),
        ],
        strings: vec![
            "<init>".into(),
            "ping".into(),
            "flag".into(),
            "observe".into(),
        ],
        protos: vec![
            ("V".into(), vec!["Z".into()]),
            ("V".into(), vec![]),
            ("V".into(), vec!["I".into()]),
        ],
        methods: vec![(0, 0, 0), (1, 1, 1), (2, 1, 0), (1, 2, 3)],
        fields: vec![(0, 3, 2)],
        ..Default::default()
    });
    let method =
        |owner: &str, name: &str, ret: &str, args: Vec<Arc<str>>, flags, regs, ins, words| {
            DexMethod {
                declaring_type: owner.into(),
                name: name.into(),
                parameters: args,
                return_type: ret.into(),
                thrown_types: vec![],
                access_flags: flags,
                code: Some(DexCode {
                    registers: regs,
                    ins,
                    outs: 2,
                    tries: 0,
                    try_regions: vec![],
                    instructions: words,
                    offset: 0,
                }),
            }
        };
    let class = |desc: &str, fields, methods| DexClass {
        descriptor: desc.into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields,
        methods,
        symbols: sy.clone(),
    };
    let mut words = vec![
        0x0022, 0, 0x0071, 1, 0, 0x0338, 4, 0x0112, 0x0228, other, 0x2070, 0, 0x0010, 0x1071, 3, 1,
        later, 0x0011,
    ];
    let target = class(
        "Lsample/Target;",
        vec![DexField {
            name: "flag".into(),
            field_type: "Z".into(),
            access_flags: 17,
            declaring_type: "Lsample/Target;".into(),
            is_static: false,
        }],
        vec![method(
            "Lsample/Target;",
            "<init>",
            "V",
            vec!["Z".into()],
            0x10001,
            2,
            2,
            vec![0x1070, 2, 0, 0x015c, 0, 0x000e],
        )],
    );
    let caller = class(
        "Lsample/Caller;",
        vec![],
        vec![method(
            "Lsample/Caller;",
            "run",
            "Lsample/Target;",
            vec!["I".into()],
            9,
            4,
            1,
            std::mem::take(&mut words),
        )],
    );
    let cs = vec![target, caller];
    let h = Arc::new(native_hierarchy::TypeHierarchy::from_classes(cs.iter()).unwrap());
    sy.hierarchy.set(h).unwrap();
    cs
}
#[test]
fn boolean_phi_original_pc_and_later_reuse() {
    let c = fixture(0x1112, 0x2112);
    let out = native_java::render_method("sample.Caller", &c[1], &c[1].methods[0]).unwrap();
    println!("{}", out.source);
    assert!(out.source.contains("observe((") && out.source.contains("? 1 : 0"));
}
#[test]
fn arbitrary_integer_phi_is_rejected() {
    let c = fixture(0x2112, 0x2112);
    assert!(native_java::render_method("sample.Caller", &c[1], &c[1].methods[0]).is_err());
}

#[test]
fn loop_carried_boolean_uses_exact_original_read() {
    let mut c = fixture(0x1112, 0x2112);
    c[1].methods[0].code.as_mut().unwrap().instructions = vec![
        0x0022, 0, 0x0112, 0x0212, 0x0071, 1, 0, 0x3235, 6, 0x1112, 0x02d8, 0x0102, 0xfb28, 0x2070,
        0, 0x0010, 0x1071, 3, 1, 0x2112, 0x0011,
    ];
    let out = native_java::render_method("sample.Caller", &c[1], &c[1].methods[0]).unwrap();
    println!("{}", out.source);
    assert!(out.source.contains("!= 0"));
    assert!(out.source.contains(".observe("));
}

fn loop_fixture() -> Vec<DexClass> {
    let mut c = fixture(0x1112, 0x2112);
    c[1].methods[0].code.as_mut().unwrap().instructions = vec![
        0x0022, 0, 0x0112, 0x0212, 0x0071, 1, 0, 0x3235, 6, 0x1112, 0x02d8, 0x0102, 0xfb28, 0x2070,
        0, 0x0010, 0x1071, 3, 1, 0x2112, 0x0011,
    ];
    c
}
#[test]
fn original_read_proof_rejects_work_bound_and_incomplete_types() {
    let c = loop_fixture();
    let a = rdx::native_method::MethodAnalysis::build(&c[1], &c[1].methods[0]).unwrap();
    let mut types = a.infer_types().unwrap();
    assert!(proof::proves_read(
        a.instructions(),
        a.ssa(),
        &types,
        13,
        1,
        proof::Domain::Boolean
    ));
    let id = a
        .ssa()
        .instructions
        .iter()
        .find(|i| i.pc == 13)
        .unwrap()
        .reads
        .iter()
        .find(|r| r.register == 1)
        .unwrap()
        .words[0];
    types.values[id].bounds_truncated = true;
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &types,
        13,
        1,
        proof::Domain::Boolean
    ));
    types.values[id].bounds_truncated = false;
    let mut oversized = a.instructions().clone();
    oversized.instructions = vec![oversized.instructions[0].clone(); 8193];
    assert!(!proof::proves_read(
        &oversized,
        a.ssa(),
        &types,
        13,
        1,
        proof::Domain::Boolean
    ));
    assert!(!proof::proves_read(
        a.instructions(),
        a.ssa(),
        &types,
        14,
        1,
        proof::Domain::Boolean
    ));
}
#[test]
fn reused_arbitrary_parameter_and_undefined_path_are_rejected() {
    for parameter in [false, true] {
        let mut c = loop_fixture();
        let w = &mut c[1].methods[0].code.as_mut().unwrap().instructions;
        if parameter {
            w[9] = 0x3101;
        } else {
            w[2] = 0;
            w[9] = 0;
        }
        assert!(native_java::render_method("sample.Caller", &c[1], &c[1].methods[0]).is_err());
    }
}
#[test]
fn original_constructor_and_observer_links_remain_exact() {
    let c = loop_fixture();
    let o = native_java::render_method("sample.Caller", &c[1], &c[1].methods[0]).unwrap();
    let s = format!("{:?}", o.links);
    assert!(s.contains("sample.Target.<init>(Z)V"), "{s}");
    assert!(s.contains("sample.Effects.observe(I)V"), "{s}");
}
#[test]
#[ignore = "requires Java25"]
fn fresh_loader_boolean_allocation_argument_ctor_and_reuse_fault_order() {
    use std::{fs, process::Command};
    let home = std::env::var("RDX_JAVA25_HOME").expect("RDX_JAVA25_HOME");
    let c = loop_fixture();
    let out = native_java::render_method("sample.Caller", &c[1], &c[1].methods[0]).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-boolean-allocation-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/Caller.java"),format!(r#"package sample; public class Caller {{ {}
 public static Target original(int p0) {{int flag;Target result=new Target(switch(0){{default->{{Effects.ping();flag=0;int counter=0;while(counter<p0){{flag=1;counter++;}}yield flag!=0;}}}});Effects.observe(flag);return result;}}
 }}"#,out.source)).unwrap();
    fs::write(dir.join("sample/State.java"),r#"package sample;public class State {public static String trace="";public static int last=-1;public static Throwable marker=System.getProperty("kind").equals("E")?new AssertionError("marker"):new RuntimeException("marker");public static void hit(String s,int n){trace+=s;if(Integer.parseInt(System.getProperty("mode"))==n){if(marker instanceof Error e)throw e;throw (RuntimeException)marker;}}}"#).unwrap();
    fs::write(dir.join("sample/Target.java"),r#"package sample;public class Target {static{State.hit("N",1);}public final boolean flag;public Target(boolean f){State.hit("T"+f,4);flag=f;}}"#).unwrap();
    fs::write(dir.join("sample/Effects.java"),r#"package sample;public class Effects {static{State.hit("E",2);}public static void ping(){State.hit("P",3);}public static void observe(int f){State.last=f;State.hit("O"+f,5);}}"#).unwrap();
    fs::write(dir.join("Driver.java"),r#"import java.net.*;import java.nio.file.*;import java.lang.reflect.*;public class Driver {
 static String run(String m,int count,int mode,String kind)throws Exception{System.setProperty("mode",""+mode);System.setProperty("kind",kind);try(URLClassLoader l=new URLClassLoader(new URL[]{Path.of(".").toUri().toURL()},null)){Class<?> state=Class.forName("sample.State",true,l);Object marker=state.getField("marker").get(null);String outcome;try{Object value=Class.forName("sample.Caller",true,l).getMethod(m,int.class).invoke(null,count);outcome="value:"+value.getClass().getField("flag").getBoolean(value);}catch(InvocationTargetException e){Throwable t=e.getCause();if(t!=marker && (!(t instanceof ExceptionInInitializerError)||t.getCause()!=marker))throw new AssertionError("identity",t);outcome="throw:"+t.getClass().getName();}return outcome+"|"+state.getField("trace").get(null)+"|"+state.getField("last").getInt(null);}}
 public static void main(String[]args)throws Exception{int n=0;for(int count:new int[]{Integer.MIN_VALUE,-3,-1,0,1,2,7,17})for(int mode=0;mode<6;mode++)for(String kind:new String[]{"R","E"}){String a=run("run",count,mode,kind),b=run("original",count,mode,kind);if(!a.equals(b))throw new AssertionError(a+" != "+b);n++;}System.out.println("checked="+n);}}
 "#).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Caller.java", "Driver.java"]),
        ("java", vec!["-cp", ".", "Driver"]),
    ] {
        let r = Command::new(format!("{home}/bin/{program}"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        if program == "java" {
            assert!(String::from_utf8_lossy(&r.stdout).contains("checked=96"));
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nested_constructor_projection_keeps_original_boolean_read() {
    let mut c = loop_fixture();
    let words = &mut c[1].methods[0].code.as_mut().unwrap().instructions;
    words.splice(2..2, [0x0112, 0x0222, 0, 0x0071, 1, 0, 0x2070, 0, 0x0012]);
    let out = native_java::render_method("sample.Caller", &c[1], &c[1].methods[0]).unwrap();
    assert_eq!(
        out.source.matches("new sample.Target(").count(),
        2,
        "{}",
        out.source
    );
    assert!(out.source.contains("!= 0"));
    assert!(out.source.contains(".observe("));
}
#[test]
fn canonical_boolean_parameter_is_preserved() {
    let mut c = fixture(0x3101, 0x2112);
    c[1].methods[0].parameters[0] = "Z".into();
    let out = native_java::render_method("sample.Caller", &c[1], &c[1].methods[0]).unwrap();
    assert!(out.source.contains("boolean p0"));
    assert!(out.source.contains("sample.Effects.observe("));
}
