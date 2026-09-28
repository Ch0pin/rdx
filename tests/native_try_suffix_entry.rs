//! Two normal paths enter different offsets of one typed DEX try region.
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
        strings: vec!["first".into(), "second".into(), "caught".into()],
        types: vec!["Lsample/SuffixTry;".into()],
        protos: vec![
            ("I".into(), vec![]),
            ("I".into(), vec!["Ljava/lang/RuntimeException;".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2)],
        ..Default::default()
    });
    let method = &mut class.methods[0];
    method.name = "run".into();
    method.access_flags = 9;
    method.parameters = vec!["I".into()];
    method.return_type = "I".into();
    let code = method.code.as_mut().unwrap();
    code.registers = 3;
    code.ins = 1;
    code.outs = 1;
    code.tries = 1;
    code.instructions = vec![
        0x0012, 0x0238, 6, 0x0071, 0, 0, 0x000a, 0x0071, 1, 0, 0x010a, 0x0190, 0x0001, 0x010f,
        0x010d, 0x1071, 2, 1, 0x010a, 0x010f,
    ];
    code.try_regions = vec![DexTryRegion {
        start: 3,
        end: 11,
        catches: vec![(Some(Arc::from("Ljava/lang/RuntimeException;")), 14)].into(),
    }];
    class
}
fn source(class: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.SuffixTry", class, &class.methods[0])?.source)
}
#[test]
fn suffix_entry_keeps_both_exception_arms() {
    let java = source(&fixture()).unwrap();
    assert_eq!(java.matches(".first(").count(), 1, "{java}");
    assert_eq!(java.matches(".second(").count(), 2, "{java}");
    assert_eq!(java.matches(".caught(").count(), 2, "{java}");
}
#[test]
fn entry_at_move_result_without_invoke_is_rejected() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[2] = 9;
    assert!(source(&class).is_err());
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn suffix_entry_jvm_preserves_effects_catches_and_error_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-suffix-try-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class SuffixTry {{
 static int mode,a,b,c;
 static final RuntimeException sentinel=new RuntimeException();
 static final Error fatal=new Error();
 static int first(){{a++;if(mode==1)throw sentinel;if(mode==3)throw fatal;return 10;}}
 static int second(){{b++;if(mode==2)throw sentinel;if(mode==4)throw fatal;return 20;}}
 static int caught(RuntimeException e){{if(e!=sentinel)throw new AssertionError("identity",e);c++;return -9;}}
 {}
 public static void main(String[]args){{
   for(int prefix=0;prefix<2;prefix++)for(mode=0;mode<5;mode++){{
     a=b=c=0;boolean first=prefix!=0;
     try{{int result=run(prefix);if(mode==3&&first||mode==4||result!=(mode==1&&first||mode==2?-9:(first?30:20)))throw new AssertionError("result");}}
     catch(Error e){{if(e!=fatal||!(mode==3&&first||mode==4&&mode!=1))throw new AssertionError("Error identity",e);}}
     int expectedB=(mode==1&&first||mode==3&&first)?0:1;
     if(a!=(first?1:0)||b!=expectedB||c!=(mode==1&&first||mode==2?1:0))throw new AssertionError("effects");
   }}
 }}
}}"#,
        source(&fixture()).unwrap()
    );
    fs::write(dir.join("sample/SuffixTry.java"), &java).unwrap();
    for (program, arg) in [
        ("javac", "sample/SuffixTry.java"),
        ("java", "sample.SuffixTry"),
    ] {
        let result = Command::new(program)
            .arg(arg)
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
