use rdx::{
    engine::{DecompilerEngine, NativeEngine},
    load_limits::{LoadLimitExceeded, LoadLimits},
};
use std::path::Path;

#[test]
fn explicit_large_profile_keeps_small_apk_results_identical() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
    let mut standard = NativeEngine::default();
    let mut large = NativeEngine::default();
    let a = standard.open(&path).unwrap();
    let b = large.open_with_limits(&path, LoadLimits::Large).unwrap();
    assert_eq!(a.classes, b.classes);
    assert_eq!(a.resources, b.resources);
    for name in &a.classes {
        assert_eq!(
            standard.decompile(name).unwrap(),
            large.decompile(name).unwrap()
        );
    }
    let missing = tempfile::tempdir().unwrap();
    assert!(large.open(&missing.path().join("missing.dex")).is_err());
    assert!(
        large.decompile(&a.classes[0]).is_err(),
        "failed loads must clear previous project state"
    );
}

#[test]
#[ignore = "requires RDX_TEST_APK pointing to an oversized local APK"]
fn oversized_apk_retries_with_bounded_large_profile() {
    let path = std::env::var("RDX_TEST_APK").unwrap();
    let mut engine = NativeEngine::default();
    let started = std::time::Instant::now();
    let error = engine.open(Path::new(&path)).unwrap_err();
    assert!(
        error.downcast_ref::<LoadLimitExceeded>().is_some(),
        "{error:#}"
    );
    let standard_seconds = started.elapsed().as_secs_f64();
    let started = std::time::Instant::now();
    let project = engine
        .open_with_limits(Path::new(&path), LoadLimits::Large)
        .unwrap();
    let large_seconds = started.elapsed().as_secs_f64();
    if let Ok(expected) = std::env::var("RDX_EXPECT_CLASSES") {
        assert_eq!(project.classes.len(), expected.parse::<usize>().unwrap());
    }
    let samples: Vec<_> = project
        .classes
        .iter()
        .filter(|name| name.starts_with("com.okinc."))
        .filter(|name| {
            engine.dex_class(name).is_some_and(|c| {
                !c.methods.is_empty()
                    && c.methods.len() <= 8
                    && c.methods
                        .iter()
                        .filter_map(|m| m.code.as_ref())
                        .map(|c| c.instructions.len())
                        .sum::<usize>()
                        <= 500
            })
        })
        .take(3)
        .cloned()
        .collect();
    assert!(
        !samples.is_empty(),
        "expected source samples from the supplied APK"
    );
    let icon = engine
        .prepared_container()
        .and_then(|container| rdx::app_icon::load(&container, engine.resource_table()));
    if std::env::var_os("RDX_REQUIRE_ICON").is_some() {
        assert!(
            icon.is_some(),
            "launcher icon failed to decode: {:?}",
            engine.resource_error()
        );
    }
    let mut reports = Vec::new();
    for name in samples {
        let (code, coverage) = engine
            .decompile_class_source_report(&name, &std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert!(!code.source.is_empty());
        reports.push(serde_json::json!({"class_id":name,"chars":code.source.len(),"coverage":coverage["counts"]}));
    }
    eprintln!(
        "LARGE_APK_RESULT {}",
        serde_json::json!({"classes":project.classes.len(),"icon_decoded":icon.is_some(),"standard_failure":format!("{error:#}"),"standard_seconds":standard_seconds,"large_seconds":large_seconds,"sample_sources":reports})
    );
}
