use rdx::engine::{DecompilerEngine, NativeEngine};
use std::sync::atomic::AtomicBool;

#[test]
fn implementation_queries_validate_source_and_reject_static_methods() {
    let mut engine = NativeEngine::default();
    engine
        .open(std::path::Path::new("tests/fixtures/hello.dex"))
        .unwrap();
    let code = engine.decompile_with_metadata("sample.Hello").unwrap();
    let declaration = code
        .links
        .iter()
        .find(|l| l.label == "sample.Hello")
        .unwrap();
    let cancel = AtomicBool::new(false);
    let (_, results) = engine
        .implementations_at(
            "sample.Hello",
            declaration.start,
            &code.source_hash,
            &cancel,
        )
        .unwrap();
    assert!(results.is_empty());
    assert!(
        engine
            .implementations_at("sample.Hello", declaration.start, "stale", &cancel)
            .is_err()
    );
    let method = code
        .links
        .iter()
        .find(|l| l.label.contains("answer("))
        .unwrap();
    assert!(
        engine
            .implementations_at("sample.Hello", method.start, &code.source_hash, &cancel)
            .unwrap_err()
            .to_string()
            .contains("cannot be overridden")
    );
}

#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn play_store_interface_and_method_implementations_have_exact_navigation() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let code = engine.decompile_with_metadata("cewj").unwrap();
    let class = code.links.iter().find(|l| l.label == "cewj").unwrap();
    let method = code
        .links
        .iter()
        .find(|l| l.label == "cewj.a()Ljava/lang/Object;")
        .unwrap();
    let cancel = AtomicBool::new(false);
    for (offset, methods) in [(class.start, false), (method.start, true)] {
        let (_, results) = engine
            .implementations_at("cewj", offset, &code.source_hash, &cancel)
            .unwrap();
        assert!(!results.is_empty());
        assert!(results.windows(2).all(|pair| pair[0] < pair[1]));
        println!("cewj methods={methods}: {} results", results.len());
        for symbol in results.iter().take(3) {
            let result = engine.implementation_result(symbol).unwrap();
            let code = result.code.unwrap();
            let occurrence = &result.occurrences[0];
            let destination = engine
                .navigate(&result.class, occurrence.start, &code.source_hash)
                .unwrap();
            assert_eq!(destination.class, result.class, "{symbol}");
            assert_eq!(destination.position, occurrence.start, "{symbol}");
            assert!(occurrence.end <= code.source.chars().count());
        }
    }
}
