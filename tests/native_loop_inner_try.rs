//! A typed try wholly inside a loop must retain per-iteration catch ownership.
use rdx::{
    native_dex::{self, DexClass, DexSymbols, DexTryRegion},
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture() -> DexClass {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.methods.retain(|m| m.name.as_ref() == "answer");
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["touch".into(), "caught".into()],
        types: vec!["Lsample/InnerTry;".into()],
        protos: vec![
            ("V".into(), vec!["I".into()]),
            ("V".into(), vec!["Ljava/lang/RuntimeException;".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 1)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "run".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into()];
    method.return_type = "V".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    code.outs = 1;
    code.tries = 1;
    code.instructions = vec![
        0x2001, 0x003d, 14, 0x1071, 0, 0, 0x0628, 0x010d, 0x1071, 1, 1, 0x000e, 0x00d8, 0xff00,
        0xf328, 0x000e,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 3,
        end: 6,
        catches: vec![(Some(Arc::from("Ljava/lang/RuntimeException;")), 7)].into(),
    }];
    class
}
fn continuing_fixture() -> DexClass {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x2001, 0x003d, 13, 0x1071, 0, 0, 0x0528, 0x010d, 0x1071, 1, 1, 0x00d8, 0xff00, 0xf428,
        0x000e,
    ];
    class
}
fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.InnerTry", class, &class.methods[0])?.source)
}
#[test]
fn typed_try_is_nested_in_loop_and_emitted_once() {
    let java = source(&fixture()).unwrap();
    assert_eq!(java.matches("while (true)").count(), 1, "{java}");
    assert_eq!(java.matches("try {").count(), 1, "{java}");
    assert_eq!(java.matches(".touch(").count(), 1, "{java}");
    assert_eq!(java.matches(".caught(").count(), 1, "{java}");
    assert!(
        java.find("while (true)").unwrap() < java.find("try {").unwrap(),
        "{java}"
    );
}
#[test]
fn partial_protected_overlap_stays_rejected() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().try_regions[0].start = 0;
    assert!(source(&class).is_err());
}
#[test]
fn catch_backedge_continues_enclosing_loop() {
    let java = source(&continuing_fixture()).unwrap();
    assert_eq!(java.matches("while (true)").count(), 1, "{java}");
    assert_eq!(java.matches("try {").count(), 1, "{java}");
    assert!(
        java.find(".caught(").unwrap() < java.find(" + (-1)").unwrap(),
        "{java}"
    );
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn inner_try_jvm_preserves_effect_count_and_exception_identity() {
    for continuing in [false, true] {
        let dir =
            std::env::temp_dir().join(format!("rdx-inner-try-{}-{continuing}", std::process::id()));
        fs::create_dir_all(dir.join("sample")).unwrap();
        let java = format!(
            r#"package sample;
public class InnerTry {{
 static int calls,catches,fail,limit; static boolean error;
 static final RuntimeException sentinel=new RuntimeException();
 static final Error fatal=new Error();
 static void touch(int n) {{
   if(n != limit-calls) throw new AssertionError("order");
   int index=calls++;
   if(index==fail) {{ if(error) throw fatal; throw sentinel; }}
 }}
 static void caught(RuntimeException e) {{
   if(e != sentinel) throw new AssertionError("identity",e);
   catches++;
 }}
 {}
 public static void main(String[] args) {{
   for(limit=0;limit<=5;limit++) for(fail=-1;fail<=limit;fail++)
     for(int mode=0;mode<2;mode++) {{
       calls=0;catches=0;error=mode==1;
       try {{ run(limit); if(error && fail>=0 && fail<limit) throw new AssertionError("missed Error"); }}
       catch(Error e) {{ if(e != fatal || !error || fail<0 || fail>=limit) throw new AssertionError("wrong Error",e); }}
       int expected=fail>=0 && fail<limit && (error || !{}) ? fail+1 : limit;
       if(calls!=expected || catches!=(!error && fail>=0 && fail<limit ? 1 : 0))
         throw new AssertionError("effects");
     }}
 }}
}}"#,
            if continuing {
                source(&continuing_fixture()).unwrap()
            } else {
                source(&fixture()).unwrap()
            },
            continuing
        );
        fs::write(dir.join("sample/InnerTry.java"), &java).unwrap();
        for (program, argument) in [
            ("javac", "sample/InnerTry.java"),
            ("java", "sample.InnerTry"),
        ] {
            let result = Command::new(program)
                .arg(argument)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{program}: {}\n{java}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
