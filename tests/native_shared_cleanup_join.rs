use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let class = DexClass {
        descriptor: "Lsample/Cleanup;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            types: vec!["Ljava/io/BufferedReader;".into(), "Lsample/Effects;".into()],
            strings: vec!["readLine".into(), "close".into(), "after".into()],
            protos: vec![("Ljava/lang/String;".into(), vec![]), ("V".into(), vec![])],
            methods: vec![(0, 0, 0), (0, 1, 1), (1, 1, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Cleanup;".into(),
            name: "run".into(),
            return_type: "V".into(),
            parameters: vec!["Ljava/io/BufferedReader;".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 3,
                ins: 1,
                outs: 1,
                tries: 3,
                try_regions: vec![
                    DexTryRegion {
                        start: 0,
                        end: 4,
                        catches: vec![(Some("Ljava/io/IOException;".into()), 11), (None, 6)].into(),
                    },
                    DexTryRegion {
                        start: 7,
                        end: 10,
                        catches: vec![(Some("Ljava/io/IOException;".into()), 10)].into(),
                    },
                    DexTryRegion {
                        start: 11,
                        end: 14,
                        catches: vec![(Some("Ljava/io/IOException;".into()), 14)].into(),
                    },
                ],
                instructions: vec![
                    0x106e, 0, 2, 0x000c, 0x0007, 0x0628, 0x010d, 0x106e, 1, 2, 0x0127, 0x106e, 1,
                    2, 0x0071, 2, 0, 0x000e,
                ],
                offset: 0,
            }),
        }],
    };
    class
        .symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&class]).unwrap()))
        .unwrap();
    class
}

#[test]
fn shared_cleanup_stays_outside_protected_read() {
    let class = fixture();
    let source = native_java::render_method("sample.Cleanup", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("catch (java.io.IOException"), "{source}");
    assert!(source.contains("catch (java.lang.Throwable"), "{source}");
    assert_eq!(source.matches(".close()").count(), 2, "{source}");
    let normal_close = source.rfind(".close()").unwrap();
    let first_catch = source.find("catch (java.io.IOException").unwrap();
    assert!(normal_close > first_catch, "{source}");
}

#[test]
#[ignore = "requires javac and java"]
fn shared_cleanup_preserves_throw_identity_and_effect_counts() {
    use std::process::Command;
    let class = fixture();
    let source = native_java::render_method("sample.Cleanup", &class, &class.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-cleanup-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    std::fs::write(dir.join("sample/Effects.java"),
        "package sample; public class Effects { static int after; public static void after() { after++; } }").unwrap();
    std::fs::write(dir.join("sample/Cleanup.java"), format!(r#"package sample;
        public class Cleanup {{
            {source}
            static class Reader extends java.io.BufferedReader {{
                int closes; boolean failRead; boolean failClose; RuntimeException boom;
                Reader() {{ super(new java.io.StringReader("x")); }}
                public String readLine() throws java.io.IOException {{
                    if (boom != null) throw boom;
                    if (failRead) throw new java.io.IOException("read");
                    return "x";
                }}
                public void close() throws java.io.IOException {{ closes++; if (failClose) throw new java.io.IOException("close"); }}
            }}
            public static void main(String[] ignored) {{
                Reader r = new Reader(); run(r); if (r.closes != 1 || Effects.after != 1) throw new AssertionError("normal");
                r = new Reader(); r.failRead = true; run(r); if (r.closes != 1 || Effects.after != 2) throw new AssertionError("read io");
                r = new Reader(); r.failClose = true; run(r); if (r.closes != 1 || Effects.after != 3) throw new AssertionError("close io");
                r = new Reader(); r.boom = new RuntimeException("original"); r.failClose = true;
                try {{ run(r); throw new AssertionError("missing boom"); }}
                catch (RuntimeException e) {{ if (e != r.boom || r.closes != 1 || Effects.after != 3) throw new AssertionError("identity"); }}
            }}
        }}"#)).unwrap();
    let compiled = Command::new("javac")
        .arg(dir.join("sample/Cleanup.java"))
        .arg(dir.join("sample/Effects.java"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let run = Command::new("java")
        .arg("-cp")
        .arg(&dir)
        .arg("sample.Cleanup")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::remove_dir_all(dir).unwrap();
}
