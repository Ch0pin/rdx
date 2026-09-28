use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    DexClass {
        descriptor: "Lsample/Guarded;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["inside".into(), "outside".into()],
            types: vec!["Lsample/Guarded;".into()],
            protos: vec![("I".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 0, 1)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Guarded;".into(),
            name: "test".into(),
            return_type: "I".into(),
            parameters: vec!["Z".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 0,
                tries: 1,
                offset: 0,
                instructions: vec![
                    0x0012, 0x0239, 3, 0x0928, 0x0071, 0, 0, 0x000a, 0x0071, 1, 0, 0x000a, 0x000f,
                    0x010d, 0xf012, 0x000f,
                ],
                try_regions: vec![DexTryRegion {
                    start: 4,
                    end: 8,
                    catches: vec![(Some(Arc::from("Ljava/lang/RuntimeException;")), 13)].into(),
                }],
            }),
        }],
    }
}

#[test]
fn handler_after_enclosing_join_remains_detached() {
    let class = fixture();
    let source = native_java::render_method("sample.Guarded", &class, &class.methods[0])
        .unwrap()
        .source;
    assert_eq!(source.matches(".inside()").count(), 1, "{source}");
    assert_eq!(source.matches(".outside()").count(), 1, "{source}");
    assert!(
        source.find("catch (").unwrap() < source.find(".outside()").unwrap(),
        "{source}"
    );
}

#[test]
fn outside_handler_reentering_enclosing_join_is_rejected() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions[14] = 0xfe28;
    class.methods[0]
        .code
        .as_mut()
        .unwrap()
        .instructions
        .truncate(15);
    let error =
        native_java::render_method("sample.Guarded", &class, &class.methods[0]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("outside handler re-enters enclosing region"),
        "{error:#}"
    );
}

#[test]
#[ignore = "requires javac and java"]
fn guard_handler_and_outside_throw_preserve_effects_and_identity() {
    let class = fixture();
    let source = native_java::render_method("sample.Guarded", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-detached-handler-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Guarded.java"), format!(r#"package sample;
public class Guarded {{
 static int mode, insideCalls, outsideCalls;
 static final RuntimeException failure=new IllegalStateException("outside");
 static int inside(){{insideCalls++;if(mode==1)throw new IllegalArgumentException("inside");return 7;}}
 static int outside(){{outsideCalls++;if(mode==2)throw failure;return 42;}}
 {source}
 public static void main(String[] args){{for(mode=0;mode<3;mode++)for(boolean enabled:new boolean[]{{false,true}}){{
 insideCalls=outsideCalls=0;
 try{{int result=test(enabled);if(enabled&&mode==2||result!=(enabled?(mode==1?-1:42):0))throw new AssertionError("result");}}
 catch(RuntimeException actual){{if(!enabled||mode!=2||actual!=failure)throw new AssertionError("identity",actual);}}
 if(insideCalls!=(enabled?1:0)||outsideCalls!=(enabled&&mode!=1?1:0))throw new AssertionError("effects");
 }}}}
}}"#)).unwrap();
    for (command, args) in [
        ("javac", vec!["sample/Guarded.java"]),
        ("java", vec!["-cp", ".", "sample.Guarded"]),
    ] {
        let result = std::process::Command::new(command)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
