use rdx::{
    engine::{DecompilerEngine, NativeEngine},
    native_dex::{self, DexSymbols},
    native_xrefs::call_sites,
};
use std::sync::{Arc, atomic::AtomicBool};

#[test]
fn dex_calls_keep_overloads_dispatch_and_repeated_sites() {
    let mut class = native_dex::parse(include_bytes!("fixtures/hello.dex"))
        .unwrap()
        .classes
        .remove(0);
    class.symbols = Arc::new(DexSymbols {
        strings: vec!["run".into()],
        types: vec![Arc::from("LTarget;")],
        protos: vec![
            (Arc::from("V"), vec![]),
            (Arc::from("V"), vec![Arc::from("I")]),
        ],
        methods: vec![(0, 0, 0), (0, 1, 0)],
        ..Default::default()
    });
    class.methods.truncate(1);
    class.methods[0].code.as_mut().unwrap().instructions = vec![
        0x1071, 0, 0, 0x1071, 0, 0, 0x1072, 1, 0, 0x1074, 0, 0, 0x000e,
    ];
    let cancel = AtomicBool::new(false);
    let calls = call_sites(&class, None, &cancel).unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0].callee, "Target.run()V");
    assert_eq!(calls[1].pc, 3);
    assert_eq!(calls[2].callee, "Target.run(I)V");
    assert!(calls[2].dispatch.contains("declared"));
    assert!(calls[3].dispatch.starts_with("virtual"));
    assert!(
        call_sites(&class, Some("Other.run()V"), &cancel)
            .unwrap()
            .is_empty()
    );
    assert!(call_sites(&class, None, &AtomicBool::new(true)).is_err());
    // All standard and range invoke forms, plus polymorphic calls, retain pool identities.
    let mut words = Vec::new();
    for opcode in [0x6e, 0x6f, 0x70, 0x71, 0x72, 0x74, 0x75, 0x76, 0x77, 0x78] {
        words.extend([0x1000 | opcode, 0, 0]);
    }
    words.extend([0x10fa, 0, 0, 0, 0x01fb, 0, 0, 0, 0x000e]);
    class.methods[0].code.as_mut().unwrap().instructions = words;
    assert_eq!(call_sites(&class, None, &cancel).unwrap().len(), 12);
    // A payload containing an invoke-shaped word must not introduce a false edge.
    class.methods[0].code.as_mut().unwrap().instructions = vec![0x0300, 2, 2, 0, 0x1071, 0, 0x000e];
    assert!(call_sites(&class, None, &cancel).unwrap().is_empty());
    class.methods[0].code.as_mut().unwrap().instructions = vec![0x1071];
    assert!(call_sites(&class, None, &cancel).is_err());
}

#[test]
fn call_results_open_exact_dex_sites_and_reject_stale_sources() {
    let mut engine = NativeEngine::default();
    engine
        .open(std::path::Path::new("tests/fixtures/navigation.apk"))
        .unwrap();
    let cancel = AtomicBool::new(false);
    let code = engine.decompile_with_metadata("sample.Caller").unwrap();
    let method = code
        .definitions
        .iter()
        .find(|d| d.name == "compute")
        .unwrap();
    let target = engine
        .resolve_method_xref_target("sample.Caller", method.start, &code.source_hash, false)
        .unwrap();
    let results = engine
        .method_xrefs_in_class(&target.id, "sample.Caller", false, &cancel)
        .unwrap();
    let hit = results
        .occurrences
        .iter()
        .find(|hit| hit.enclosing.contains("sample.Target.doubleValue(I)I"))
        .unwrap();
    let document = engine.decompile_with_metadata(&results.class).unwrap();
    assert_eq!(
        document.source_hash,
        results.code.as_ref().unwrap().source_hash
    );
    let callers = engine
        .resolve_method_xref_target(&results.class, hit.start, &document.source_hash, true)
        .unwrap();
    let incoming = engine
        .method_xrefs_in_class(&callers.id, "sample.Caller", true, &cancel)
        .unwrap();
    assert!(
        incoming
            .occurrences
            .iter()
            .any(|site| site.start == hit.start)
    );
    assert!(
        incoming
            .occurrences
            .iter()
            .all(|site| !site.enclosing.contains("doubleValue(Ljava"))
    );
    let declaration = engine
        .navigate(&results.class, hit.start, &document.source_hash)
        .unwrap();
    assert_eq!(declaration.class, "sample.Target");
    assert!(
        engine
            .resolve_method_xref_target(&results.class, hit.start, "stale", true)
            .is_err()
    );
    assert!(
        engine
            .resolve_method_xref_target("sample.Caller", 0, &code.source_hash, false)
            .is_err()
    );
}

#[test]
#[ignore = "Set RDX_TEST_APK to the Play Store APK"]
fn real_apk_method_call_sites() {
    let mut engine = NativeEngine::default();
    engine
        .open(std::path::Path::new(
            &std::env::var_os("RDX_TEST_APK").unwrap(),
        ))
        .unwrap();
    let owner = "com.google.android.finsky.screenshotsactivity.ScreenshotsActivityV2";
    let code = engine.decompile_with_metadata(owner).unwrap();
    let method = code.definitions.iter().find(|d| d.name == "x").unwrap();
    let target = engine
        .resolve_method_xref_target(owner, method.start, &code.source_hash, false)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let result = engine
        .method_xrefs_in_class(&target.id, owner, false, &cancel)
        .unwrap();
    assert!(result.occurrences.len() >= 5);
    let doc = result.code.unwrap();
    for site in &result.occurrences {
        assert!(
            doc.links
                .iter()
                .any(|link| link.start == site.start && link.end == site.end)
        );
    }
    println!(
        "{} call sites in {:.3}s",
        result.occurrences.len(),
        started.elapsed().as_secs_f64()
    );
    let hit = &result.occurrences[0];
    let incoming_target = engine
        .resolve_method_xref_target(&result.class, hit.start, &doc.source_hash, true)
        .unwrap();
    let incoming = engine
        .method_xrefs_in_class(&incoming_target.id, owner, true, &cancel)
        .unwrap();
    assert!(
        incoming
            .occurrences
            .iter()
            .any(|site| site.start == hit.start)
    );
}

#[test]
fn explicit_dex_view_can_open_java_for_the_same_class() {
    let mut engine = NativeEngine::default();
    engine
        .open(std::path::Path::new("tests/fixtures/navigation.apk"))
        .unwrap();
    let dex = engine
        .decompile_with_metadata("dex://sample.Caller")
        .unwrap();
    assert!(dex.source.contains("Exact method call sites"));
    let java = engine.navigate_class("sample.Caller").unwrap();
    assert_eq!(java.class, "sample.Caller");
    assert!(java.code.source.contains("class Caller"));
    assert!(!java.code.source.contains(".class sample.Caller"));
    // Switching views never changes the exact instruction view or its source identity.
    let again = engine
        .decompile_with_metadata("dex://sample.Caller")
        .unwrap();
    assert_eq!(again.source_hash, dex.source_hash);
}

#[test]
#[ignore = "Set RDX_ZOOM_APK to the Zoom APK"]
fn schedule_e0_is_java_not_a_decompilation_fallback() {
    let mut engine = NativeEngine::default();
    engine
        .open(std::path::Path::new(
            &std::env::var_os("RDX_ZOOM_APK").unwrap(),
        ))
        .unwrap();
    let name = "com.zipow.videobox.fragment.schedule.e0";
    assert_eq!(engine.dex_class(name).unwrap().methods.len(), 16);
    let java = engine.navigate_class(name).unwrap();
    assert!(java.code.source.contains("public class e0 extends o"));
    assert!(java.code.source.contains("showInActivity("));
    assert!(
        !java
            .code
            .source
            .lines()
            .any(|line| line.trim_start().starts_with(".method "))
    );
    assert!(
        engine
            .decompile(&format!("dex://{name}"))
            .unwrap()
            .contains("Exact method call sites")
    );
}
