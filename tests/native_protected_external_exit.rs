//! A pure branch exits a DEX try before an effectful external throw.
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
        strings: vec![
            "touch".into(),
            "normal".into(),
            "external".into(),
            "done".into(),
            "caught".into(),
        ],
        types: vec!["Lsample/ExternalExit;".into()],
        protos: vec![
            ("V".into(), vec![]),
            ("Ljava/lang/RuntimeException;".into(), vec![]),
            ("I".into(), vec![]),
            ("I".into(), vec!["Ljava/lang/RuntimeException;".into()]),
        ],
        methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2), (0, 2, 3), (0, 3, 4)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.parameters = vec!["I".into()];
    m.return_type = "I".into();
    let b = m.code.as_mut().unwrap();
    b.registers = 3;
    b.ins = 1;
    b.outs = 1;
    b.tries = 1;
    b.instructions = vec![
        0x0071, 0, 0, 0x0238, 6, 0x0071, 1, 0, 0x0628, 0x0071, 2, 0, 0x000c, 0x0027, 0x0071, 3, 0,
        0x000a, 0x000f, 0x000d, 0x1071, 4, 0, 0x000a, 0x000f,
    ];
    b.try_regions = vec![DexTryRegion {
        start: 0,
        end: 9,
        catches: vec![(Some(Arc::from("Ljava/lang/RuntimeException;")), 19)].into(),
    }];
    c
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.ExternalExit", c, &c.methods[0])?.source)
}
#[test]
fn external_throw_stays_outside_original_catch() {
    let s = source(&fixture()).unwrap();
    assert_eq!(s.matches(".touch(").count(), 1, "{s}");
    assert_eq!(s.matches(".normal(").count(), 1, "{s}");
    assert_eq!(s.matches(".done(").count(), 1, "{s}");
    assert_eq!(s.matches(".external(").count(), 1, "{s}");
    assert_eq!(s.matches(".caught(").count(), 2, "{s}");
    assert!(
        s.find("catch (").unwrap() < s.find(".external(").unwrap(),
        "{s}"
    );
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn external_throw_jvm_keeps_original_exception_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-external-exit-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class ExternalExit{{
 static int mode,touches,normals,externals,dones,catches;
 static final RuntimeException protectedFailure=new RuntimeException(),externalFailure=new RuntimeException();
 static void touch(){{touches++;if(mode==1)throw protectedFailure;}}
 static void normal(){{normals++;if(mode==2)throw protectedFailure;}}
 static RuntimeException external(){{externals++;return externalFailure;}}
 static int done(){{dones++;if(mode==3)throw externalFailure;return 1;}}
 static int caught(RuntimeException e){{if(e!=protectedFailure)throw new AssertionError("caught wrong exception",e);catches++;return -9;}}
 {}
 public static void main(String[]args){{for(mode=0;mode<4;mode++)for(int take=0;take<2;take++){{touches=normals=externals=dones=catches=0;
   boolean protectedFailureExpected=mode==1||mode==2&&take!=0;
   boolean externalFailureExpected=!protectedFailureExpected&&(take==0||mode==3);
   try{{int result=run(take);if(externalFailureExpected||result!=(protectedFailureExpected?-9:1))throw new AssertionError("result");}}
   catch(RuntimeException e){{if(e!=externalFailure||!externalFailureExpected)throw new AssertionError("external identity",e);}}
   if(touches!=1||normals!=(mode==1||take==0?0:1)||externals!=(mode!=1&&take==0?1:0)||dones!=(mode==1||take==0||mode==2?0:1)||catches!=(protectedFailureExpected?1:0))throw new AssertionError("effects");
 }}}}
}}"#,
        source(&fixture()).unwrap()
    );
    fs::write(dir.join("sample/ExternalExit.java"), &java).unwrap();
    for (p, a) in [
        ("javac", "sample/ExternalExit.java"),
        ("java", "sample.ExternalExit"),
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
