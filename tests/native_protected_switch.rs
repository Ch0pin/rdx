use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_java,
};
use std::sync::Arc;

fn fixture() -> DexClass {
    let mut words = vec![
        0x1012, // v0 = 1, before the protected switch
        0x032b, 17, 0, // packed-switch v3, payload at pc18
        0x2012, 0x0528, // default: v0 = 2; goto pc10
        0x3012, 0x0328, // case 0: v0 = 3; goto pc10
        0x4012, 0x0128, // case 1: v0 = 4; goto pc10
        0x1071, 0, 4,      // Helper.maybeThrow(v4)
        0x010a, // move-result v1
        0x10b0, // v0 += v1
        0x000f, // return v0
        0x020d, // catch NumberFormatException
        0x000f, // return handler-visible v0
        0x0100, 2, 0, 0, 5, 0, 7, 0, // switch payload
    ];
    assert_eq!(words.len(), 26);
    let descriptor: Arc<str> = "Lsample/ProtectedSwitch;".into();
    DexClass {
        descriptor: descriptor.clone(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods: vec![DexMethod {
            declaring_type: descriptor,
            name: "run".into(),
            return_type: "I".into(),
            parameters: vec!["I".into(), "I".into()],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 5,
                ins: 2,
                outs: 1,
                tries: 1,
                try_regions: vec![DexTryRegion {
                    start: 1,
                    end: 14,
                    catches: vec![(Some("Ljava/lang/NumberFormatException;".into()), 16)].into(),
                }],
                offset: 0,
                instructions: std::mem::take(&mut words),
            }),
        }],
        symbols: Arc::new(DexSymbols {
            strings: vec!["maybeThrow".into()],
            types: vec!["Lsample/Helper;".into()],
            protos: vec![("I".into(), vec!["I".into()])],
            methods: vec![(0, 0, 0)],
            ..Default::default()
        }),
    }
}

#[test]
fn protected_switch_keeps_each_handlers_register_snapshot() {
    let class = fixture();
    let code = native_java::render("sample.ProtectedSwitch", &class).unwrap();
    assert!(code.source.contains("switch"), "{}", code.source);
    assert!(code.source.contains("catch"), "{}", code.source);
    assert!(!code.source.contains(".method"), "{}", code.source);
}

#[test]
#[ignore = "requires javac and java"]
fn protected_switch_jvm_checks_success_and_catch_snapshots() {
    use std::{fs, process::Command};
    let class = fixture();
    let java = native_java::render("sample.ProtectedSwitch", &class).unwrap();
    let dir = std::env::temp_dir().join(format!("rdx-protected-switch-{}", std::process::id()));
    fs::create_dir_all(dir.join("sample")).unwrap();
    fs::write(dir.join("sample/ProtectedSwitch.java"), java.source).unwrap();
    fs::write(
        dir.join("sample/Helper.java"),
        "package sample; public final class Helper { static int calls; public static int maybeThrow(int flag) { calls++; if (flag != 0) throw new NumberFormatException(); return 10; } }",
    ).unwrap();
    fs::write(
        dir.join("sample/Check.java"),
        "package sample; public final class Check { public static void main(String[] x) { int[] choices={-1,0,1}; int[] snapshots={2,3,4}; for(int i=0;i<3;i++){ if(ProtectedSwitch.run(choices[i],0)!=snapshots[i]+10)throw new AssertionError(\"success \"+i); if(ProtectedSwitch.run(choices[i],1)!=snapshots[i])throw new AssertionError(\"catch \"+i); } if(Helper.calls!=6)throw new AssertionError(\"calls \"+Helper.calls); }}",
    ).unwrap();
    let compile = Command::new("javac")
        .current_dir(&dir)
        .args([
            "sample/ProtectedSwitch.java",
            "sample/Helper.java",
            "sample/Check.java",
        ])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new("java")
        .current_dir(&dir)
        .args(["-cp", ".", "sample.Check"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    fs::remove_dir_all(dir).unwrap();
}
