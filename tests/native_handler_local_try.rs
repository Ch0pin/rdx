//! A forward normal continuation can jump over a handler-local try block.
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
    c.methods.retain(|m| m.name.as_ref() == "answer");
    c.symbols = Arc::new(DexSymbols {
        strings: vec!["first".into(), "second".into(), "after".into()],
        types: vec!["Lsample/HandlerTry;".into()],
        protos: vec![("I".into(), vec![]), ("I".into(), vec!["I".into()])],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.parameters.clear();
    m.return_type = "I".into();
    m.access_flags = 9;
    let b = m.code.as_mut().unwrap();
    b.registers = 2;
    b.ins = 0;
    b.outs = 1;
    b.tries = 2;
    b.instructions = vec![
        0x0071, 0, 0, 0x000a, 0x0f28, 0x010d, 0x0071, 1, 0, 0x000a, 0x0428, 0x010d, 0xe012, 0x0128,
        0x1071, 2, 0, 0x000a, 0x0128, 0x000f,
    ];
    b.try_regions = vec![
        DexTryRegion {
            start: 0,
            end: 4,
            catches: vec![(Some(Arc::from("Ljava/lang/RuntimeException;")), 5)].into(),
        },
        DexTryRegion {
            start: 6,
            end: 10,
            catches: vec![(Some(Arc::from("Ljava/lang/IllegalArgumentException;")), 11)].into(),
        },
    ];
    c
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.HandlerTry", c, &c.methods[0])?.source)
}
fn sibling_handlers_join_before_normal_tail() -> DexClass {
    let mut c = fixture();
    let m = &mut c.methods[0];
    let code = m.code.as_mut().unwrap();
    code.tries = 1;
    code.instructions = vec![
        0x0071, 0, 0, 0x000a, 0x0c28, 0x010d, 0x1012, 0x0428, 0x010d, 0x2012, 0x0128, 0x1071, 2, 0,
        0x000a, 0x0128, 0x000f,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 0,
        end: 4,
        catches: vec![
            (Some(Arc::from("Ljava/lang/IllegalArgumentException;")), 5),
            (Some(Arc::from("Ljava/lang/RuntimeException;")), 8),
        ]
        .into(),
    }];
    c
}
#[test]
fn sibling_handlers_shared_effect_precedes_normal_entry() {
    let s = source(&sibling_handlers_join_before_normal_tail()).unwrap();
    assert!(
        s.contains("catch (java.lang.IllegalArgumentException"),
        "{s}"
    );
    assert!(s.contains("catch (java.lang.RuntimeException"), "{s}");
}
#[test]
#[ignore = "requires javac and java"]
fn sibling_handlers_preserve_effect_count_and_outside_throw() {
    let dir = std::env::temp_dir().join(format!("rdx-sibling-handler-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class HandlerTry {{
 static int mode,firstCalls,afterCalls;
 static final RuntimeException outside=new IllegalStateException("outside");
 static int first(){{firstCalls++;if(mode==1)throw new IllegalArgumentException();if(mode>=2)throw new RuntimeException();return 11;}}
 static int second(){{throw new AssertionError("unused");}}
 static int after(int value){{afterCalls++;if(mode==3)throw outside;return value+100;}}
 {}
 public static void main(String[]args){{for(mode=0;mode<4;mode++){{firstCalls=afterCalls=0;
 try{{int result=run();if(mode==3||result!=(mode==0?11:mode==1?101:102))throw new AssertionError("result");}}
 catch(RuntimeException actual){{if(mode!=3||actual!=outside)throw new AssertionError("identity",actual);}}
 if(firstCalls!=1||afterCalls!=(mode==0?0:1))throw new AssertionError("effects");
 }}}}
}}"#,
        source(&sibling_handlers_join_before_normal_tail()).unwrap()
    );
    fs::write(dir.join("sample/HandlerTry.java"), &java).unwrap();
    for (p, args) in [
        ("javac", vec!["sample/HandlerTry.java"]),
        ("java", vec!["-cp", ".", "sample.HandlerTry"]),
    ] {
        let r = Command::new(p)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{p}: {}\n{java}",
            String::from_utf8_lossy(&r.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn handler_local_try_is_not_on_outer_normal_path() {
    let s = source(&fixture()).unwrap();
    assert_eq!(s.matches("try {").count(), 2, "{s}");
    for name in ["first", "second", "after"] {
        assert_eq!(s.matches(&format!(".{name}(")).count(), 1, "{s}");
    }
}
#[test]
fn normal_path_entering_next_protected_region_at_its_start_is_valid() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[4] = 0x0228;
    assert!(source(&c).is_ok());
}
#[test]
fn normal_path_entering_a_handler_stays_rejected() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[4] = 0x0728;
    assert!(source(&c).is_err());
}
#[test]
#[ignore = "requires javac and java"]
fn nested_handler_java_preserves_dispatch_effects_and_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-handler-local-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
 public class HandlerTry {{
 static int mode,a,b,c; static final RuntimeException outer=new RuntimeException(),other=new IllegalStateException(),last=new RuntimeException();static final IllegalArgumentException inner=new IllegalArgumentException();
 static int first(){{a++;if(mode>0)throw outer;return 11;}}
 static int second(){{b++;if(mode==2)throw inner;if(mode==3)throw other;return 22;}}
 static int after(int x){{c++;if(mode==4)throw last;return x+100;}}
 {}
 public static void main(String[]args){{for(mode=0;mode<5;mode++){{a=b=c=0;try{{int n=run();if(mode>=3||n!=(mode==0?11:mode==1?122:98))throw new AssertionError("result");}}catch(RuntimeException e){{if(e!=(mode==3?other:mode==4?last:null))throw new AssertionError("identity",e);}}if(a!=1||b!=(mode>0?1:0)||c!=((mode==1||mode==2||mode==4)?1:0))throw new AssertionError("effects");}}}}
 }}"#,
        source(&fixture()).unwrap()
    );
    fs::write(dir.join("sample/HandlerTry.java"), &java).unwrap();
    for (p, a) in [
        ("javac", "sample/HandlerTry.java"),
        ("java", "sample.HandlerTry"),
    ] {
        let r = Command::new(p).arg(a).current_dir(&dir).output().unwrap();
        assert!(
            r.status.success(),
            "{p}: {}\n{java}",
            String::from_utf8_lossy(&r.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
