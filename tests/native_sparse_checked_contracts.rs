use rdx::{
    native_dex::{DexClass, DexCode, DexMethod, DexSymbols, DexTryRegion},
    native_hierarchy::TypeHierarchy,
    native_java,
};
use std::{process::Command, sync::Arc};
fn method(owner: &str, name: Arc<str>) -> DexMethod {
    DexMethod {
        declaring_type: Arc::from(owner),
        name,
        return_type: Arc::from("V"),
        parameters: vec![],
        thrown_types: vec![],
        access_flags: 0x109,
        code: None,
    }
}
fn class(owner: &str, symbols: Arc<DexSymbols>, methods: Vec<DexMethod>) -> DexClass {
    DexClass {
        symbols,
        descriptor: Arc::from(owner),
        superclass: Some(Arc::from("Ljava/lang/Object;")),
        interfaces: vec![],
        access_flags: 0x11,
        annotations_offset: 0,
        static_values_offset: 0,
        static_values: vec![],
        fields: vec![],
        methods,
    }
}
fn fixture(exhaust: bool) -> (DexClass, Arc<DexSymbols>) {
    let symbols = Arc::new(DexSymbols {
        types: vec![Arc::from("Lsample/Api;")],
        strings: vec!["tick".into()],
        protos: vec![(Arc::from("V"), vec![])],
        methods: vec![(0, 0, 0)],
        ..Default::default()
    });
    let mut tick = method("Lsample/Api;", Arc::from("tick"));
    tick.thrown_types = vec![Arc::from("Ljava/io/IOException;")];
    let api = class("Lsample/Api;", symbols.clone(), vec![tick]);
    let mut run = method("Lsample/Calls;", Arc::from("run"));
    run.access_flags = 9;
    run.thrown_types = vec![Arc::from("Ljava/io/IOException;")];
    run.code = Some(DexCode {
        registers: 1,
        ins: 0,
        outs: 0,
        tries: 1,
        try_regions: vec![DexTryRegion {
            start: 0,
            end: 3,
            catches: vec![(None, 4)].into(),
        }],
        instructions: vec![0x0071, 0, 0, 0x000e, 0x000d, 0x0027],
        offset: 0,
    });
    let calls = class("Lsample/Calls;", symbols.clone(), vec![run]);
    let padding = class(
        "Lsample/Padding;",
        symbols.clone(),
        if exhaust {
            (0..256_001)
                .map(|i| method("Lsample/Padding;", Arc::from(format!("native{i}"))))
                .collect()
        } else {
            vec![]
        },
    );
    let h = TypeHierarchy::from_classes([&api, &calls, &padding]).unwrap();
    symbols.hierarchy.set(Arc::new(h)).unwrap();
    (calls, symbols)
}
#[test]
fn small_exact_checked_contract_preserves_header_and_rethrow() {
    let (calls, symbols) = fixture(false);
    assert_eq!(
        symbols.hierarchy.get().unwrap().call_checked_effects(
            "Lsample/Api;",
            "tick",
            &[],
            "V",
            true
        ),
        Some(vec![Arc::from("Ljava/io/IOException;")])
    );
    let source = native_java::render_method("sample.Calls", &calls, &calls.methods[0])
        .unwrap()
        .source;
    assert!(source.contains("throws java.io.IOException"));
    assert!(source.contains("sample.Api.tick()"));
    assert!(source.contains("throw "));
}
#[test]
#[ignore = "requires javac/java and a 256001-family metadata fixture"]
fn exhausted_readability_contract_compiles_and_preserves_fault_identity() {
    let (calls, symbols) = fixture(true);
    let h = symbols.hierarchy.get().unwrap();
    assert_eq!(
        h.call_checked_effects("Lsample/Api;", "tick", &[], "V", true),
        Some(vec![Arc::from("Ljava/io/IOException;")])
    );
    assert_eq!(
        h.call_checked_effects("Lsample/Api;", "unseeded", &[], "V", true),
        Some(vec![Arc::from("Ljava/lang/Throwable;")])
    );
    let source = native_java::render_method("sample.Calls", &calls, &calls.methods[0])
        .unwrap()
        .source;
    let dir = std::env::temp_dir().join(format!("rdx-sparse-contract-jvm-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sample")).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    std::fs::write(
        dir.join("sample/Calls.java"),
        format!("package sample; public final class Calls {{\n{source}\n}}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("sample/Api.java"),"package sample; public final class Api { static Throwable fault; static int calls; public static void tick() throws java.io.IOException { calls++; if(fault instanceof java.io.IOException)throw (java.io.IOException)fault; if(fault instanceof RuntimeException)throw (RuntimeException)fault; if(fault instanceof Error)throw (Error)fault; } }").unwrap();
    std::fs::write(dir.join("sample/Main.java"),"package sample; public final class Main { public static void main(String[] ignored) throws Throwable { for(Throwable fault:new Throwable[]{null,new java.io.IOException(\"io\"),new RuntimeException(\"runtime\"),new AssertionError(\"error\")}) { Api.fault=fault; Api.calls=0; Throwable got=null; try { Calls.run(); } catch(Throwable x){got=x;} if(got!=fault||Api.calls!=1)throw new AssertionError(\"identity/order\"); } } }").unwrap();
    let compiled = Command::new("javac")
        .current_dir(&dir)
        .args(["sample/Calls.java", "sample/Api.java", "sample/Main.java"])
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .current_dir(&dir)
        .args(["-cp", ".", "sample.Main"])
        .output()
        .unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
}
