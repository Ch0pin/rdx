use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;
fn fixture(
    words: Vec<u16>,
    catches: Vec<(Option<&str>, u32)>,
    start: u32,
    end: u32,
    ret: &str,
) -> DexClass {
    DexClass {
        descriptor: "Lsample/Effects;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["first".into(), "second".into(), "touch".into()],
            types: vec!["Lsample/Effects;".into()],
            protos: vec![("I".into(), vec![]), ("V".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 0, 1), (0, 1, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Effects;".into(),
            name: "test".into(),
            return_type: ret.into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 0,
                outs: 0,
                tries: 1,
                try_regions: vec![DexTryRegion {
                    start,
                    end,
                    catches: catches
                        .into_iter()
                        .map(|(ty, addr)| (ty.map(Arc::from), addr))
                        .collect::<Vec<_>>()
                        .into(),
                }],
                instructions: words,
                offset: 0,
            }),
        }],
    }
}
fn sample() -> DexClass {
    let mut class = fixture(
        vec![
            0x0071, 0, 0, 0x000a, // first result
            0x0038, 6, // zero -> second branch at 10
            0x0071, 2, 0, 0x000f, // cleanup then return
            0x0071, 1, 0, 0x000a, // second result
            0x0071, 2, 0, 0x000f, // cleanup then return
            0x010d, 0x0071, 2, 0, 0x0127, // exceptional cleanup
        ],
        vec![(None, 18)],
        0,
        6,
        "I",
    );
    let code = class.methods[0].code.as_mut().unwrap();
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 10,
        end: 14,
        catches: vec![(None, 18)].into(),
    });
    class
}
#[test]
fn shared_cleanup_with_multiple_normal_exits() {
    let class = sample();
    let code = native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap();
    assert_eq!(code.source.matches("finally").count(), 1, "{}", code.source);
    assert_eq!(
        code.source.matches(".touch()").count(),
        1,
        "{}",
        code.source
    );
    assert_eq!(code.source.matches("return ").count(), 2, "{}", code.source);
}
#[test]
fn cleanup_proof_rejects_changed_dispatch_or_bypass() {
    let mut class = sample();
    class.methods[0].code.as_mut().unwrap().try_regions[1].end = 10;
    assert!(native_java::render_method("sample.Effects", &class, &class.methods[0]).is_err());
    let mut class = sample();
    class.methods[0].code.as_mut().unwrap().instructions[5] = 13; // direct to return
    assert!(native_java::render_method("sample.Effects", &class, &class.methods[0]).is_err());
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn shared_cleanup_executes_once_and_preserves_exception_precedence() {
    use std::process::Command;
    let class = sample();
    let source = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-finally-100-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    let java = format!(
        r#"package sample;
public class Effects {{
 static int mode, count; static boolean cleanupFails;
 static int first() {{if(mode==2)throw new IllegalArgumentException("body");return mode;}}
 static int second() {{return 42;}}
 static void touch() {{count++;if(cleanupFails)throw new IllegalStateException("cleanup");}}
 {source}
 public static void main(String[] args) {{
  for(mode=0;mode<3;mode++)for(int fail=0;fail<2;fail++){{
   cleanupFails=fail==1;count=0;
   try{{int result=test();if(cleanupFails||mode==2||result!=(mode==0?42:1))throw new AssertionError();}}
   catch(IllegalArgumentException e){{if(mode!=2||cleanupFails)throw new AssertionError();}}
   catch(IllegalStateException e){{if(!cleanupFails)throw new AssertionError();}}
   if(count!=1)throw new AssertionError("cleanup count "+count);
  }}
 }}
}}"#
    );
    std::fs::write(dir.join("sample/Effects.java"), java).unwrap();
    for (command, args) in [
        ("javac", vec!["sample/Effects.java"]),
        ("java", vec!["-cp", ".", "sample.Effects"]),
    ] {
        let result = Command::new(command)
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

#[test]
fn distinct_catchall_handlers_render_independently() {
    let mut class = fixture(
        vec![
            0x0071, 0, 0, 0x000a, // first
            0x0071, 1, 0, 0x000a, // second
            0x000f, 0x010d, 0x0027, // first handler rethrows value? overridden below
            0x010d, 0x0127,
        ],
        vec![(None, 9)],
        0,
        4,
        "I",
    );
    let code = class.methods[0].code.as_mut().unwrap();
    code.instructions[10] = 0x0127;
    code.tries = 2;
    code.try_regions.push(DexTryRegion {
        start: 4,
        end: 8,
        catches: vec![(None, 11)].into(),
    });
    let source = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    assert_eq!(
        source.matches("catch (java.lang.Throwable").count(),
        2,
        "{source}"
    );
    assert_eq!(source.matches(".first()").count(), 1, "{source}");
    assert_eq!(source.matches(".second()").count(), 1, "{source}");
}
