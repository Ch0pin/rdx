use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::{fs, process::Command, sync::Arc};

fn fixture(handler_words: Vec<u16>, handler_return: &str) -> DexClass {
    let handler_end = 4 + handler_words.len();
    let mut instructions = vec![0x0071, 0, 0, (((handler_end - 3) as u16) << 8) | 0x28];
    instructions.extend(handler_words);
    instructions.extend([0x0071, 2, 0, 0x000e]);
    let class = DexClass {
        descriptor: "Lsample/Retry;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: Arc::new(DexSymbols {
            strings: vec!["normal".into(), "recover".into(), "after".into()],
            types: vec!["Lsample/Retry;".into()],
            protos: vec![("V".into(), vec![]), (handler_return.into(), vec![])],
            methods: vec![(0, 0, 0), (0, 1, 1), (0, 0, 2)],
            ..Default::default()
        }),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Retry;".into(),
            name: "run".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 0,
                ins: 0,
                outs: 0,
                tries: 1,
                try_regions: vec![DexTryRegion {
                    start: 0,
                    end: handler_end as u32,
                    catches: vec![(Some("Ljava/lang/NoSuchFieldError;".into()), 4)].into(),
                }],
                instructions,
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
fn self_covered_void_handler_retries_matching_error_then_joins() {
    let class = fixture(vec![0x0071, 1, 0], "V");
    let source = native_java::render_method("sample.Retry", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(
        source.contains("catch (java.lang.NoSuchFieldError"),
        "{source}"
    );
    assert!(source.contains("while (true) {"), "{source}");
    assert_eq!(
        source.matches("sample.Retry.recover()").count(),
        1,
        "{source}"
    );
    assert_eq!(
        source.matches("sample.Retry.after()").count(),
        1,
        "{source}"
    );
    assert!(source.find("sample.Retry.after()").unwrap() > source.rfind("while (true)").unwrap());
}

#[test]
fn self_covered_multicall_or_result_handler_remains_fallback() {
    for (words, ret) in [
        (vec![0x0071, 1, 0, 0x0071, 1, 0], "V"),
        (vec![0x0071, 1, 0], "I"),
    ] {
        let class = fixture(words, ret);
        assert!(native_java::render_method("sample.Retry", &class, &class.methods[0]).is_err());
    }
    let mut class = fixture(vec![0x0071, 1, 0], "V");
    let words = &mut class.methods[0].code.as_mut().unwrap().instructions;
    // Normal flow skips the post-handler call, so it cannot be retried as part
    // of the self-covered handler's exception edge.
    words[3] = 0x0728;
    assert!(native_java::render_method("sample.Retry", &class, &class.methods[0]).is_err());

    let mut class = fixture(vec![0x1071, 1, 0], "V");
    Arc::get_mut(&mut class.symbols).unwrap().protos[1].1 = vec!["Ljava/lang/Class;".into()];
    let code = class.methods[0].code.as_mut().unwrap();
    code.registers = 1;
    code.try_regions[0].start = 2;
    code.try_regions[0].end = 9;
    code.try_regions[0].catches = vec![(Some("Ljava/lang/NoSuchFieldError;".into()), 6)].into();
    code.instructions = vec![
        0x001c, 0, 0x0071, 0, 0, 0x0428, 0x1071, 1, 0, 0x0071, 2, 0, 0x000e,
    ];
    let source = native_java::render_method("sample.Retry", &class, &class.methods[0])
        .unwrap()
        .source;
    assert!(source.find("java.lang.Class v0").unwrap() < source.find("while (true)").unwrap());
    assert_eq!(
        source.matches("sample.Retry.recover(v0)").count(),
        1,
        "{source}"
    );
}

#[test]
#[ignore = "requires javac and java on PATH"]
fn self_covered_handler_jvm_preserves_success_retry_and_unmatched_throw() {
    let class = fixture(vec![0x0071, 1, 0], "V");
    let method = native_java::render_method("sample.Retry", &class, &class.methods[0])
        .unwrap()
        .source;
    let directory = std::env::temp_dir().join(format!("rdx-handler-retry-{}", std::process::id()));
    fs::create_dir_all(directory.join("sample")).unwrap();
    let java = format!(
        "package sample; public class Retry {{ static String log = \"\"; static boolean failNormal; static int failRecovery; static boolean other; static void normal() {{ log += \"N\"; if (failNormal) throw new NoSuchFieldError(); }} static void recover() {{ log += \"R\"; if (other) throw new IllegalStateException(); if (failRecovery-- > 0) throw new NoSuchFieldError(); }} static void after() {{ log += \"A\"; }} {method} public static void main(String[] args) {{ run(); if (!log.equals(\"NA\")) throw new AssertionError(log); log = \"\"; failNormal = true; failRecovery = 2; run(); if (!log.equals(\"NRRRA\")) throw new AssertionError(log); log = \"\"; other = true; try {{ run(); throw new AssertionError(); }} catch (IllegalStateException expected) {{ if (!log.equals(\"NR\")) throw new AssertionError(log); }} }} }}"
    );
    fs::write(directory.join("sample/Retry.java"), java).unwrap();
    for (program, args) in [
        ("javac", vec!["sample/Retry.java"]),
        ("java", vec!["sample.Retry"]),
    ] {
        let output = Command::new(program)
            .args(args)
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
