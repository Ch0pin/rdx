//! A catchall handler can jump past the normal continuation to cleanup+throw.
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
        strings: vec!["touch".into(), "cleanup".into()],
        types: vec!["Lsample/CleanupTry;".into()],
        protos: vec![("V".into(), vec![])],
        methods: vec![(0, 0, 0), (0, 0, 1)],
        ..Default::default()
    });
    let m = &mut c.methods[0];
    m.name = "run".into();
    m.access_flags = 9;
    m.parameters.clear();
    m.return_type = "V".into();
    let b = m.code.as_mut().unwrap();
    b.registers = 2;
    b.ins = 0;
    b.outs = 0;
    b.tries = 1;
    b.instructions = vec![
        0x0071, 0, 0, 0x0428, 0x010d, 0x1007, 0x0528, 0x0071, 1, 0, 0x000e, 0x0071, 1, 0, 0x0027,
    ];
    b.try_regions = vec![DexTryRegion {
        start: 0,
        end: 3,
        catches: vec![(None, 4)].into(),
    }];
    c
}
fn source(c: &DexClass) -> anyhow::Result<String> {
    Ok(native_java::render_method("sample.CleanupTry", c, &c.methods[0])?.source)
}
#[test]
fn detached_cleanup_renders_two_exclusive_paths() {
    let s = source(&fixture()).unwrap();
    assert_eq!(s.matches(".touch(").count(), 1, "{s}");
    assert_eq!(s.matches(".cleanup(").count(), 2, "{s}");
    assert!(s.contains("throw "), "{s}");
}
#[test]
#[ignore = "requires javac and java on PATH"]
fn detached_cleanup_jvm_preserves_effects_and_exception_identity() {
    let dir = std::env::temp_dir().join(format!("rdx-detached-cleanup-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class CleanupTry{{
 static int mode,touches,cleans;
 static final RuntimeException sentinel=new RuntimeException(),cleanupFailure=new RuntimeException();
 static void touch(){{touches++;if(mode==1||mode==3)throw sentinel;}}
 static void cleanup(){{cleans++;if(mode==2||mode==3)throw cleanupFailure;}}
 {}
 public static void main(String[]args){{for(mode=0;mode<4;mode++){{touches=cleans=0;
  try{{run();if(mode!=0)throw new AssertionError("missing failure");}}
  catch(RuntimeException e){{if(e!=(mode==1?sentinel:cleanupFailure))throw new AssertionError("identity",e);}}
  if(touches!=1||cleans!=1)throw new AssertionError("effects");
 }}}}
}}"#,
        source(&fixture()).unwrap()
    );
    fs::write(dir.join("sample/CleanupTry.java"), &java).unwrap();
    for (p, a) in [
        ("javac", "sample/CleanupTry.java"),
        ("java", "sample.CleanupTry"),
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
