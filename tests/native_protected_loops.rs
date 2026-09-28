//! Wholly enclosed loops with invariant catch inputs.
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
        strings: vec!["touch".into(), "caught".into()],
        types: vec!["Lsample/Protected;".into()],
        protos: vec![
            ("V".into(), vec!["I".into(), "I".into()]),
            (
                "I".into(),
                vec!["Ljava/lang/RuntimeException;".into(), "I".into()],
            ),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "test".into();
    m.access_flags = 9;
    m.parameters = vec!["I".into(), "I".into()];
    m.return_type = "I".into();
    let b = m.code.as_mut().unwrap();
    b.registers = 6;
    b.ins = 2;
    b.outs = 2;
    b.tries = 1;
    b.instructions = vec![
        0x0012, 0x4035, 14, 0x0112, 0x4135, 8, 0x2071, 0, 0x0010, 0x01d8, 0x0101, 0xf928, 0x00d8,
        0x0100, 0xf328, 0x000f, 0x020d, 0x2071, 1, 0x0052, 0x020a, 0x020f,
    ];
    b.try_regions = vec![DexTryRegion {
        start: 1,
        end: 15,
        catches: vec![(Some(Arc::from("Ljava/lang/RuntimeException;")), 16)].into(),
    }];
    c
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.Protected", c, &c.methods[0])?.source)
}
#[test]
fn nested_loops_preserve_enclosing_catch() {
    let s = source(&fixture()).unwrap();
    assert_eq!(s.matches("while (true)").count(), 2, "{s}");
    assert!(s.contains("catch (java.lang.RuntimeException"), "{s}");
}
#[test]
fn changing_handler_input_is_rejected() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().instructions[12] = 0x05d8;
    assert!(source(&c).is_err());
}
#[test]
fn partial_protected_loop_is_rejected() {
    let mut c = fixture();
    c.methods[0].code.as_mut().unwrap().try_regions[0].end = 11;
    assert!(source(&c).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn protected_loop_jvm_preserves_effects_and_exception_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-protected-loop-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Protected {{
 static int calls,fail,limit,catches; static boolean error;
 static final RuntimeException sentinel=new RuntimeException();static final Error fatal=new Error();
 static void touch(int i,int j){{if(i!=calls/limit||j!=calls%limit)throw new AssertionError("order");int n=calls++;if(n==fail){{if(error)throw fatal;throw sentinel;}}}}
 static int caught(RuntimeException e,int tag){{if(e!=sentinel)throw new AssertionError("identity");catches++;return tag;}}
 {}
 public static void main(String[] args){{for(limit=0;limit<5;limit++)for(fail=-1;fail<=limit*limit;fail++)for(int mode=0;mode<2;mode++){{error=mode==1;calls=0;catches=0;boolean hit=fail>=0&&fail<limit*limit;try{{int r=test(limit,73);if(hit&&error||r!=(hit?73:limit))throw new AssertionError("result");}}catch(Error e){{if(!hit||!error||e!=fatal)throw new AssertionError("unexpected",e);}}if(calls!=(hit?fail+1:limit*limit)||catches!=(hit&&!error?1:0))throw new AssertionError("effects");}}}}
}}
"#,
        source(&fixture()).unwrap()
    );
    fs::write(dir.join("sample/Protected.java"), &java).unwrap();
    for (p, a) in [
        ("javac", "sample/Protected.java"),
        ("java", "sample.Protected"),
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
