use rdx::{engine::NativeEngine, package_container::PackageContainer};

#[test]
#[ignore = "requires RDX_ACTIVITY_APK, RDX_ACTIVITY_CLASSES and RDX_ACTIVITY_OUTPUT"]
fn real_activity_reconstruction_and_icon() {
    let apk = std::env::var_os("RDX_ACTIVITY_APK").unwrap();
    let output = std::path::PathBuf::from(std::env::var_os("RDX_ACTIVITY_OUTPUT").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let container = PackageContainer::open(std::path::Path::new(&apk)).unwrap();
    let mut engine = NativeEngine::start().unwrap();
    engine.open_prepared(container.clone()).unwrap();
    let icon = rdx::app_icon::load(&container, engine.resource_table()).expect("launcher icon");
    icon.save(output.join("icon.png")).unwrap();
    for name in std::env::var("RDX_ACTIVITY_CLASSES").unwrap().split(',') {
        let (code, coverage) = engine
            .decompile_class_source_report(name, &false.into())
            .unwrap();
        std::fs::write(output.join(format!("{name}.java")), &code.source).unwrap();
        std::fs::write(
            output.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&coverage).unwrap(),
        )
        .unwrap();
        eprintln!("{name}: {}", coverage["counts"]);
        assert_eq!(coverage["counts"]["omitted"], 0, "{name}");
        let minimum_java = match name {
            "com.capitalone.autonavigator.ui.activities.AnActivity" => Some(85),
            "com.capitalone.autonavigator.ui.activities.data.access.rational.activity.DataAccessRationaleActivity" => {
                Some(20)
            }
            _ => None,
        };
        if let Some(minimum) = minimum_java {
            assert!(
                coverage["counts"]["decompiled"].as_u64().unwrap() >= minimum,
                "reconstruction regression for {name}: {}",
                coverage["counts"]
            );
        }
        for line in code.source.lines().take_while(|l| l.starts_with("//")) {
            eprintln!("{line}");
        }
    }
}

fn boolean_entry_integer_loop() -> rdx::native_dex::DexClass {
    use rdx::native_dex::{DexClass, DexCode, DexMethod, DexSymbols};
    DexClass {
        descriptor: "Lsample/Counter;".into(),
        superclass: Some("Ljava/lang/Object;".into()),
        interfaces: vec![],
        access_flags: 1,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        symbols: std::sync::Arc::new(DexSymbols::default()),
        methods: vec![DexMethod {
            declaring_type: "Lsample/Counter;".into(),
            name: "count".into(),
            parameters: vec!["I".into()],
            return_type: "I".into(),
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 4,
                ins: 1,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                offset: 0,
                instructions: vec![
                    0x0338, 4, 0x1012, 0x0228, 0x0012, 0x4112, 0x1035, 5, 0x00d8, 0x0100, 0xfc28,
                    0x000f,
                ],
            }),
        }],
    }
}

#[test]
fn integer_loop_does_not_retain_boolean_entry_storage() {
    let root = boolean_entry_integer_loop();
    let source = rdx::native_java::render("sample.Counter", &root)
        .unwrap()
        .source;
    assert!(
        source.contains("while (") && source.contains("return "),
        "{source}"
    );
    assert!(!source.contains(".method"));
}

#[test]
#[ignore = "requires javac/java; root runs serially"]
fn jvm_integer_loop_entry_preserves_both_paths() {
    use std::{fs, process::Command};
    let base = std::env::temp_dir().join(format!("rdx-loop-entry-{}", std::process::id()));
    fs::create_dir_all(base.join("sample")).unwrap();
    fs::write(
        base.join("sample/Counter.java"),
        rdx::native_java::render("sample.Counter", &boolean_entry_integer_loop())
            .unwrap()
            .source,
    )
    .unwrap();
    fs::write(base.join("sample/Check.java"), "package sample; public class Check { public static void main(String[] args) {for(int n:new int[]{0,1,-1,Integer.MIN_VALUE,Integer.MAX_VALUE})if(Counter.count(n)!=4)throw new AssertionError(n); System.out.print(\"ok\");} }").unwrap();
    let out = Command::new("javac")
        .args([
            "-Xlint:all",
            "-Werror",
            "sample/Counter.java",
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
