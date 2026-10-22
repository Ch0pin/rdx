//! Adjacent exception phases preserve equal liveouts and exact exception identity.
pub use rdx::{native_cfg, native_dex, native_ir, native_method, native_types};
#[path = "../src/native_java/equivalent_handler_regions.rs"]
mod certificate;
use native_dex::{DexClass, DexSymbols, DexTryRegion};
use std::sync::Arc;
fn fixture() -> DexClass {
    let mut c = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["first".into(), "second".into(), "caught".into()],
        types: vec!["Lsample/Effects;".into()],
        protos: vec![
            ("V".into(), vec![]),
            ("V".into(), vec!["Ljava/lang/Throwable;".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.return_type = "I".into();
    m.parameters = vec!["I".into()];
    let code = m.code.as_mut().unwrap();
    code.registers = 6;
    code.ins = 1;
    code.outs = 1;
    code.tries = 2;
    code.instructions = vec![
        0x0012, 0x1112, 0x0071, 0, 0, 0x5035, 11, 0x1201, 0x0101, 0x0071, 1, 0, 0x2101, 0x00d8,
        0x0100, 0xf328, 0x1201, 0x0a28, 0x040d, 0x0428, 0x040d, 0x1201, 0x0128, 0x1071, 2, 4,
        0x020f, 0x020f,
    ];
    code.try_regions = vec![
        DexTryRegion {
            start: 2,
            end: 7,
            catches: vec![(None, 20)].into(),
        },
        DexTryRegion {
            start: 9,
            end: 23,
            catches: vec![(None, 18)].into(),
        },
    ];
    c
}
#[test]
fn same_constant_liveout_certifies_only_the_original_handler_pair() {
    let c = fixture();
    let plan = certificate::prove(&c, &c.methods[0]).expect("constant-equivalent handlers");
    assert_eq!((plan.start, plan.end, plan.common), (2, 23, 23));
    assert_eq!(plan.constant_copies.len(), 1);
    assert_eq!(plan.constant_copies[0].literal, 1);
    assert!(certificate::validate(&c, &c.methods[0], &plan));
    let adjusted = certificate::apply(&c, &c.methods[0], &plan).unwrap();
    let original = c.methods[0].code.as_ref().unwrap();
    let view = adjusted.code.as_ref().unwrap();
    let fresh = certificate::adjust(&c, &c.methods[0]).unwrap();
    let fresh = fresh.code.as_ref().unwrap();
    assert_eq!(fresh.instructions, view.instructions);
    assert_eq!(fresh.try_regions, view.try_regions);
    assert_eq!(view.instructions.len(), original.instructions.len());
    assert_eq!(view.offset, original.offset);
    assert_eq!((view.tries, view.try_regions[0].end), (1, 23));
    assert_eq!(original.try_regions[0].end, 7);
    assert_eq!(original.instructions[21], 0x1201);
    assert_eq!(view.instructions[21], 0x1212);
    assert_eq!(original.instructions[18], 0x040d);
    assert_eq!(view.instructions[18], 0);
    assert_eq!(view.instructions[19], 0);
    assert_eq!(plan.retired_prefix, 18..20);
    let source = rdx::native_java::render_method("sample.Hello", &c, &c.methods[0])
        .expect("merged protected loop")
        .source;
    assert!(source.contains("while"), "{source}");
    assert_eq!(source.matches("catch (").count(), 1, "{source}");
}
#[test]
fn changed_constant_dispatch_or_gap_effect_cannot_reuse_authority() {
    let c = fixture();
    let plan = certificate::prove(&c, &c.methods[0]).unwrap();
    for (pc, word) in [
        (7, 0x0201),
        (21, 0x0201),
        (8, 0x0027),
        (18, 0x030d),
        (17, 0x0428),
    ] {
        let mut c = fixture();
        c.methods[0].code.as_mut().unwrap().instructions[pc] = word;
        assert!(!certificate::validate(&c, &c.methods[0], &plan), "pc={pc}");
        assert!(certificate::apply(&c, &c.methods[0], &plan).is_none());
    }
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().try_regions[1].catches = vec![(None, 19)].into();
    assert!(!certificate::validate(&c, &c.methods[0], &plan));
}

#[test]
#[ignore = "requires javac and java"]
fn merged_phases_preserve_results_fault_identity_and_unprotected_continuation() {
    let c = fixture();
    let source = rdx::native_java::render_method("sample.Hello", &c, &c.methods[0])
        .unwrap()
        .source;
    let java = format!(
        r#"package sample;
public class Check {{
 {source}
 public static void main(String[] args) {{
  int checks=0;
  for(int n=0;n<5;n++) for(int phase=0;phase<3;phase++)
   for(int fail=0;fail<7;fail++) for(boolean catchFails:new boolean[]{{false,true}}) {{
    Effects.n=n; Effects.phase=phase; Effects.fail=fail; Effects.catchFails=catchFails;
    Effects.first=0; Effects.second=0; Effects.caught=0; Effects.trace="";
    int result=-1; Throwable thrown=null;
    try {{ result=run(n); }} catch(Throwable e) {{ thrown=e; }}
    int f=0,s=0,caught=0; String trace=""; Throwable expected=null;
    boolean done=false;
    while(!done) {{
     ++f; trace+="f;";
     if(phase==1 && f==fail) {{ caught=1; done=true; }}
     else if(s>=n) done=true;
     else {{ ++s; trace+="s;"; if(phase==2 && s==fail) {{ caught=1; done=true; }} }}
    }}
    if(caught!=0) {{ trace+="c;"; if(catchFails) expected=Effects.continuation; }}
    if(thrown!=expected || (thrown==null && result!=1) || Effects.first!=f || Effects.second!=s
       || Effects.caught!=caught || !Effects.trace.equals(trace))
      throw new AssertionError(n+":"+phase+":"+fail+":"+catchFails+":"+Effects.trace+"/"+trace,thrown);
    checks++;
   }}
  System.out.println(checks);
 }}
}}
class Effects {{
 static int n,phase,fail,first,second,caught; static boolean catchFails; static String trace;
 static final AssertionError fault=new AssertionError("original"), continuation=new AssertionError("continuation");
 static void first() {{ ++first; trace+="f;"; if(phase==1 && first==fail) throw fault; }}
 static void second() {{ ++second; trace+="s;"; if(phase==2 && second==fail) throw fault; }}
 static void caught(Throwable e) {{ if(e!=fault) throw new AssertionError("identity"); ++caught; trace+="c;"; if(catchFails) throw continuation; }}
}}
"#
    );
    let dir = std::env::temp_dir().join(format!("rdx-equivalent-handler-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Check.java"), &java).unwrap();
    for (command, argument) in [("javac", "sample/Check.java"), ("java", "sample.Check")] {
        let result = std::process::Command::new(command)
            .arg(argument)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{java}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
