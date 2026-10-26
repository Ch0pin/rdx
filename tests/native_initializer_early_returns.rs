//! Original <clinit> control flow with final fields on only one normal path.
use rdx::{
    native_dex::{DexClass, DexCode, DexField, DexMethod, DexSymbols, DexTryRegion, DexValue},
    native_java,
};
use std::sync::Arc;
fn fixture() -> DexClass {
    let symbols = Arc::new(DexSymbols {
        types: vec!["Lsample/Init;".into(), "I".into(), "Lsample/Hooks;".into()],
        strings: vec!["VALUE".into(), "choose".into(), "touch".into()],
        protos: vec![("Z".into(), vec![]), ("V".into(), vec![])],
        fields: vec![(0, 1, 0)],
        methods: vec![(2, 0, 1), (2, 1, 2)],
        ..Default::default()
    });
    DexClass {
        descriptor: "Lsample/Init;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![DexField {
            declaring_type: "Lsample/Init;".into(),
            name: "VALUE".into(),
            field_type: "I".into(),
            access_flags: 0x19,
            is_static: true,
        }],
        methods: vec![DexMethod {
            declaring_type: "Lsample/Init;".into(),
            name: "<clinit>".into(),
            return_type: "V".into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 0x10008,
            code: Some(DexCode {
                registers: 1,
                ins: 0,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![
                    0x0071, 0, 0, 0x000a, 0x0038, 6, 0x7012, 0x0067, 0, 0x000e, 0x0071, 1, 0,
                    0x000e,
                ],
                offset: 0,
            }),
        }],
        symbols,
    }
}
#[test]
fn early_exit_initializes_only_missing_final_default_and_preserves_links() {
    let class = fixture();
    let out = native_java::render_method("sample.Init", &class, &class.methods[0]).unwrap();
    assert!(out.source.contains("rdxInitializerExit:"), "{}", out.source);
    assert_eq!(
        out.source.matches("break rdxInitializerExit;").count(),
        2,
        "{}",
        out.source
    );
    assert!(out.source.contains("VALUE = 0;"), "{}", out.source);
    assert!(out.source.contains("VALUE = 7;"), "{}", out.source);
    assert!(out.links.iter().any(|l| l.label == "sample.Init.VALUE:I"));
    assert!(!out.source.contains("return;"), "{}", out.source);
}
#[test]
fn single_normal_exit_before_throw_keeps_initializer_control_flow() {
    let mut class = fixture();
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0071, 0, 0, 0x000a, 0x0038, 6, 0x7012, 0x0067, 0, 0x000e, 0x0012, 0x0027,
    ];
    let out = native_java::render_method("sample.Init", &class, &class.methods[0]).unwrap();
    assert!(out.source.contains("rdxInitializerExit:"), "{}", out.source);
    assert_eq!(out.source.matches("break rdxInitializerExit;").count(), 1);
    assert!(out.source.contains("throw null;"), "{}", out.source);
    assert!(!out.source.contains("return;"));
}

#[test]
fn encoded_nondefault_duplicate_writes_cycles_and_handlers_decline() {
    for shape in 0..5 {
        let mut class = fixture();
        let code = class.methods[0].code.as_mut().unwrap();
        match shape {
            0 => {
                class.static_values_offset = 1;
                class.static_values = vec![DexValue::Int(9)];
            }
            1 => {
                code.instructions.splice(9..9, [0x0067, 0]);
                code.instructions[5] = 8;
            }
            2 => {
                code.instructions[10] = 0x0028;
            }
            3 => {
                code.tries = 1;
                code.try_regions = vec![DexTryRegion {
                    start: 0,
                    end: 4,
                    catches: vec![(None, 10)].into(),
                }];
            }
            4 => {
                code.instructions[8] = 9;
            }
            _ => unreachable!(),
        }
        assert!(
            native_java::render_method("sample.Init", &class, &class.methods[0]).is_err(),
            "accepted {shape}"
        );
    }
}
#[test]
fn identical_final_state_at_join_is_required() {
    let mut class = fixture();
    // One path assigns VALUE before the common normal tail; the other omits it.
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x0071, 0, 0, 0x000a, 0x0038, 6, 0x7012, 0x0067, 0, 0x0128, 0x0071, 1, 0, 0x0038, 3,
        0x000e, 0x000e,
    ];
    assert!(native_java::render_method("sample.Init", &class, &class.methods[0]).is_err());
}
#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_early_initializer_exits_keep_final_values_effects_and_failure_identity() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-init-exits-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    let class = fixture();
    let init = native_java::render_method("sample.Init", &class, &class.methods[0]).unwrap();
    // Keep a real blank-final declaration: javac must prove assignment on both paths.
    fs::write(
        base.join("sample/Init.java"),
        format!(
            "package sample; public class Init {{ public static final int VALUE; {} }}",
            init.source
        ),
    )
    .unwrap();
    fs::write(base.join("sample/Hooks.java"),"package sample; public class Hooks {static String trace=\"\";static final RuntimeException marker=new RuntimeException();static boolean choose(){trace+=\"C\";return Boolean.getBoolean(\"choice\");}static void touch(){trace+=\"T\";if(Boolean.getBoolean(\"fail\"))throw marker;}}").unwrap();
    fs::write(base.join("sample/Check.java"),"package sample; public class Check {public static void main(String[] args){boolean choice=Boolean.getBoolean(\"choice\"),fail=Boolean.getBoolean(\"fail\");try{int x=Init.VALUE;if(!choice&&fail)throw new AssertionError();if(x!=(choice?7:0)||!Hooks.trace.equals(choice?\"C\":\"CT\"))throw new AssertionError();}catch(ExceptionInInitializerError error){if(choice||!fail||error.getCause()!=Hooks.marker||!Hooks.trace.equals(\"CT\"))throw new AssertionError(error);}System.out.print(\"ok\");}}").unwrap();
    let out = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Init.java",
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
    for choice in [false, true] {
        for fail in [false, true] {
            let out = Command::new("java")
                .args([
                    format!("-Dchoice={choice}"),
                    format!("-Dfail={fail}"),
                    "-cp".into(),
                    ".".into(),
                    "sample.Check".into(),
                ])
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
    }
}
