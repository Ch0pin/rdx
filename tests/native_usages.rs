use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
fn unrelated_usage_scan_returns_no_source_document() {
    let mut engine = NativeEngine::start().unwrap();
    let project = engine
        .open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/navigation.dex")
                .as_path(),
        )
        .unwrap();
    assert!(!project.classes.is_empty());
    for class in project.classes {
        let result = engine.usages_in_class("no.such.Symbol", &class).unwrap();
        assert!(result.occurrences.is_empty());
        assert!(result.code.is_none(), "nonmatching {class} retained source");
    }
}
