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
    fixture(
        vec![
            0x0071, 0, 0, 0x000a, 0x0038, 3, 0x0228, 0x000f, 0x0071, 1, 0, 0x000a, 0x000f, 0x010d,
            0xf012, 0x000f,
        ],
        vec![(Some("Ljava/lang/RuntimeException;"), 13)],
        0,
        6,
        "I",
    )
}
#[test]
fn pure_early_return_keeps_effectful_continuation_outside_try() {
    let class = sample();
    let source = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    let second = source.find(".second()").unwrap();
    let catch = source.find("catch (").unwrap();
    assert!(second > catch, "outside effect moved inside try: {source}");
    assert_eq!(source.matches(".second()").count(), 1, "{source}");
    assert_eq!(source.matches(".first()").count(), 1, "{source}");
}
#[test]
fn distinct_effectful_frontiers_remain_rejected() {
    let class = fixture(
        vec![
            0x0071, 0, 0, 0x000a, 0x0038, 6, 0x0071, 1, 0, 0x000f, 0x0071, 1, 0, 0x000f, 0x010d,
            0xf012, 0x000f,
        ],
        vec![(Some("Ljava/lang/RuntimeException;"), 14)],
        0,
        6,
        "I",
    );
    let error =
        native_java::render_method("sample.Effects", &class, &class.methods[0]).unwrap_err();
    assert!(
        error.to_string().contains("distinct effectful exits"),
        "{error:#}"
    );
}
#[test]
#[ignore = "requires javac and java"]
fn outside_continuation_throw_is_not_caught_by_original_handler() {
    use std::process::Command;
    let class = sample();
    let source = native_java::render_method("sample.Effects", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-exit-frontier-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Effects.java"),format!(r#"package sample;
public class Effects {{
 static int mode,calls;static final RuntimeException failure=new IllegalStateException("outside");
 static int first(){{if(mode==2)throw new IllegalArgumentException("inside");return mode==0?0:1;}}
 static int second(){{calls++;if(mode==3)throw failure;return 42;}}
 {source}
 public static void main(String[] args){{for(mode=0;mode<4;mode++){{calls=0;
  try{{int result=test();if(mode==3||result!=(mode==0?0:mode==2?-1:42))throw new AssertionError("result");}}
  catch(RuntimeException actual){{if(mode!=3||actual!=failure)throw new AssertionError("wrong boundary",actual);}}
  if(calls!=(mode==0||mode==2?0:1))throw new AssertionError("effect count");
 }}}}
}}"#)).unwrap();
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
