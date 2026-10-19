use rdx::{
    native_dex::{self, DexSymbols},
    native_java,
};
use std::{process::Command, sync::Arc};
fn fixture() -> native_dex::DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["exit".into(), "after".into(), "log".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![("I".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 0, 2)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.return_type = "I".into();
    m.parameters = vec!["I".into(), "I".into()];
    m.access_flags = 9;
    let code = m.code.as_mut().unwrap();
    code.registers = 4;
    code.ins = 2;
    code.outs = 1;
    code.instructions = vec![
        0x0012, 0x1112, 0x0239, 7, 0x033d, 19, 0x1332, 3, 0x0c28, 0x1071, 0, 3, 0x000a, 0x0038, 10,
        0x1071, 2, 0, 0x000a, 0x0428, 0x03d8, 0xff03, 0xee28, 0x1071, 1, 0, 0x000a, 0x000f,
    ];
    c
}
#[test]
fn shared_exit_jvm() {
    let c = fixture();
    let source = native_java::render_method("sample.Outer", &c, &c.methods[0])
        .unwrap()
        .source;
    let d = std::env::temp_dir().join(format!("rdx-effectful-exit-{}", std::process::id()));
    std::fs::create_dir_all(d.join("sample")).unwrap();
    std::fs::write(d.join("sample/Outer.java"),format!("package sample;public class Outer{{{source} public static void main(String[]a){{for(int b:new int[]{{0,1}})for(int n:new int[]{{-1,0,1,4}}){{Effects.trace=\"\";try{{System.out.println(b+\":\"+n+\":\"+run(b,n)+\":\"+Effects.trace);}}catch(RuntimeException e){{System.out.println(b+\":\"+n+\":fault=\"+(e==Effects.fault)+\":\"+Effects.trace);}}}}}}}}")).unwrap();
    std::fs::write(d.join("sample/Effects.java"),"package sample;public class Effects{static String trace=\"\";static RuntimeException fault=new RuntimeException();public static int exit(int x){trace+=\"exit\"+x+\",\";if(x<0)throw fault;return x;}public static int log(int x){trace+=\"log\"+x+\",\";return x;}public static int after(int x){trace+=\"after\"+x+\",\";return x+1;}}").unwrap();
    let r = Command::new("javac")
        .args(["sample/Outer.java", "sample/Effects.java"])
        .current_dir(&d)
        .output()
        .unwrap();
    assert!(
        r.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&r.stderr)
    );
    let r = Command::new("java")
        .arg("sample.Outer")
        .current_dir(&d)
        .output()
        .unwrap();
    assert!(r.status.success());
    assert_eq!(
        String::from_utf8_lossy(&r.stdout).replace("\r\n", "\n"),
        "0:-1:1:after0,\n0:0:1:after0,\n0:1:2:exit1,log1,after1,\n0:4:2:exit1,log1,after1,\n1:-1:fault=true:exit-1,\n1:0:1:exit0,after0,\n1:1:2:exit1,log1,after1,\n1:4:5:exit4,log4,after4,\n"
    );
}
#[test]
fn external_entry_to_cyclic_body_rejects() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[3] = 18;
    assert!(native_java::render_method("sample.Outer", &c, &c.methods[0]).is_err());
}

pub use rdx::{native_cfg, native_dominators, native_ir, native_ssa};
#[path = "../src/native_java/shared_effectful_exit.rs"]
mod shared_effectful_exit;
#[test]
fn canonical_plan_records_original_dag_edges_and_phi_live_in() {
    let c = fixture();
    let code = c.methods[0].code.as_ref().unwrap();
    let p = shared_effectful_exit::prove(code, 2, 9, 4, 22, 23)
        .expect("canonical effectful shared DAG");
    assert!(shared_effectful_exit::validate(code, &p));
    assert_eq!(p.live_in_registers, vec![3]);
    assert_eq!(p.incoming_edges, vec![(2, 9), (6, 9)]);
    assert!(p.edges.contains(&(13, 23)) && p.edges.contains(&(13, 15)));
    assert!(!p.loop_members.contains(&9));
    let mut poisoned = p.clone();
    poisoned.edges.pop();
    assert!(!shared_effectful_exit::validate(code, &poisoned));
    let mut poisoned = p.clone();
    poisoned.live_in_registers.clear();
    assert!(!shared_effectful_exit::validate(code, &poisoned));
    let mut poisoned = p.clone();
    poisoned.raw_words[0] ^= 0x1000;
    assert!(!shared_effectful_exit::validate(code, &poisoned));
}
#[test]
fn two_external_callers_and_loop_carried_entry_keep_exact_original_edges() {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions = vec![
        0x0012, 0x1112, 0x023c, 9, 0x023a, 7, 0x033d, 19, 0x1332, 3, 0x0c28, 0x1071, 0, 3, 0x000a,
        0x0038, 10, 0x1071, 2, 0, 0x000a, 0x0428, 0x03d8, 0xff03, 0xee28, 0x1071, 1, 0, 0x000a,
        0x000f,
    ];
    for from in [2, 4] {
        let p = shared_effectful_exit::prove(code, from, 11, 6, 24, 25).unwrap();
        assert_eq!(p.incoming_edges, vec![(2, 11), (4, 11), (8, 11)]);
        assert_eq!(p.live_in_registers, vec![3]);
        assert!(shared_effectful_exit::validate(code, &p));
    }
}
#[test]
fn protected_effects_undefined_inputs_and_invalid_wide_words_decline() {
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions.extend([0x010d, 0x030f]);
    code.tries = 1;
    code.try_regions = vec![native_dex::DexTryRegion {
        start: 9,
        end: 12,
        catches: Arc::from(vec![(None, 28)]),
    }];
    assert!(shared_effectful_exit::prove(code, 2, 9, 4, 22, 23).is_none());
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[1] = 0;
    code.instructions[11] = 1;
    assert!(shared_effectful_exit::prove(code, 2, 9, 4, 22, 23).is_none());
    let mut c = fixture();
    let code = c.methods[0].code.as_mut().unwrap();
    code.instructions[9] = 0x0005;
    code.instructions[10] = 3;
    code.instructions[11] = 0;
    assert!(shared_effectful_exit::prove(code, 2, 9, 4, 22, 23).is_none());
}
