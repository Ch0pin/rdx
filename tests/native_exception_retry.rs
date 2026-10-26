use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Hooks;".into()],
        strings: vec!["touch".into(), "retry-value".into()],
        protos: vec![("V".into(), vec!["Ljava/lang/String;".into()])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let root = DexClass {
        descriptor: "Lsample/Retry;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: symbols.clone(),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Retry;".into(),
            name: "run".into(),
            return_type: "I".into(),
            parameters: vec!["Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 2,
                ins: 1,
                outs: 1,
                tries: 1,
                offset: 0,
                instructions: vec![0x001a, 1, 0x1071, 0, 0, 0x7012, 0x000f, 0x1007, 0xf828],
                try_regions: vec![DexTryRegion {
                    start: 0,
                    end: 5,
                    catches: vec![(Some("Ljava/lang/NoSuchFieldError;".into()), 7)].into(),
                }],
            }),
        }],
    };
    symbols
        .hierarchy
        .set(Arc::new(TypeHierarchy::from_classes([&root]).unwrap()))
        .unwrap();
    root
}

fn restored_fixture() -> DexClass {
    let mut root = fixture();
    let m = &mut root.methods[0];
    m.name = "restored".into();
    m.parameters = vec!["I".into()];
    let c = m.code.as_mut().unwrap();
    c.registers = 4;
    c.instructions = vec![
        0x3101, 0x001a, 1, 0x1071, 0, 0, 0x010f, 0x1201, 0x0113, 81, 0x2101, 0xf628,
    ];
    c.try_regions[0].start = 1;
    c.try_regions[0].end = 6;
    c.try_regions[0].catches = vec![(Some("Ljava/lang/NoSuchFieldError;".into()), 7)].into();
    root
}

#[test]
fn retry_handler_must_restore_every_live_word() {
    let mut root = restored_fixture();
    let code = native_java::render_method("sample.Retry", &root, &root.methods[0]).unwrap();
    assert!(code.source.contains("while (true)"), "{}", code.source);
    root.methods[0].code.as_mut().unwrap().instructions[10] = 0x0000;
    assert!(native_java::render_method("sample.Retry", &root, &root.methods[0]).is_err());
}

#[test]
fn detached_dead_copy_handler_retries_the_complete_protected_body() {
    let root = fixture();
    let code = native_java::render("sample.Retry", &root).unwrap();
    assert!(code.source.contains("while (true)"), "{}", code.source);
    assert!(code.source.contains("NoSuchFieldError"), "{}", code.source);
    assert!(
        code.source.contains("Hooks.touch(\"retry-value\")"),
        "{}",
        code.source
    );
    assert_eq!(code.source.matches("Hooks.touch(").count(), 1);
    assert!(code.links.iter().any(|l| l.label.contains("Hooks.touch")));
}

#[test]
fn retry_rejects_live_temporaries_and_effectful_handlers() {
    for bad in 0..2 {
        let mut root = fixture();
        let code = root.methods[0].code.as_mut().unwrap();
        if bad == 0 {
            code.instructions[5] = 0x0000;
            code.instructions[6] = 0x0011;
            root.methods[0].return_type = "Ljava/lang/String;".into();
        } else {
            code.instructions.splice(7..8, [0x1071, 0, 0]);
            code.instructions[10] = 0xf628;
        }
        assert!(
            native_java::render_method("sample.Retry", &root, &root.methods[0]).is_err(),
            "accepted bad retry {bad}"
        );
    }
}

#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_retry_preserves_attempt_count_arguments_and_uncaught_identity() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-detached-retry-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    let mut source = native_java::render("sample.Retry", &fixture())
        .unwrap()
        .source;
    let end = source.rfind('}').unwrap();
    let restored = restored_fixture();
    source.insert_str(
        end,
        &native_java::render_method("sample.Retry", &restored, &restored.methods[0])
            .unwrap()
            .source,
    );
    fs::write(base.join("sample/Retry.java"), source).unwrap();
    fs::write(
        base.join("sample/Hooks.java"),
        r#"package sample;
final class Hooks {
static int attempts, failures; static boolean fatal;
static final Error ERROR = new AssertionError("identity");
static void touch(String value) { if (!"retry-value".equals(value)) throw new AssertionError(value);
attempts++; if(fatal)throw ERROR; if(attempts<=failures)throw new NoSuchFieldError(); }
}"#,
    )
    .unwrap();
    fs::write(base.join("sample/Check.java"), r#"package sample;
public class Check {public static void main(String[] args) {
for(int failures:new int[]{0,1,3}){Hooks.attempts=0;Hooks.failures=failures;
if(Retry.run(new Object())!=7||Hooks.attempts!=failures+1)throw new AssertionError();}
Hooks.attempts=0;Hooks.failures=3;if(Retry.restored(42)!=42||Hooks.attempts!=4)throw new AssertionError();
Hooks.attempts=0;Hooks.fatal=true;try{Retry.run(null);throw new AssertionError();}
catch(Throwable failure){if(failure!=Hooks.ERROR||Hooks.attempts!=1)throw new AssertionError(failure);}
System.out.print("ok");}}
"#).unwrap();
    let out = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Retry.java",
            "sample/Hooks.java",
            "sample/Check.java",
        ])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("java")
        .args(["-cp", ".", "sample.Check"])
        .current_dir(&base)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"ok");
}
