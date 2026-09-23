use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
fn direct_subclass_lookup_validates_source_and_results_navigate_to_declarations() {
    let mut engine = NativeEngine::default();
    engine
        .open(std::path::Path::new("tests/fixtures/hello.dex"))
        .unwrap();
    let code = engine.decompile_with_metadata("sample.Hello").unwrap();
    let parent = code
        .links
        .iter()
        .find(|l| l.label == "sample.Hello")
        .unwrap();
    let (name, children) = engine
        .direct_subclasses_at("sample.Hello", parent.start, &code.source_hash)
        .unwrap();
    assert_eq!(name, "sample.Hello");
    assert!(children.is_empty());
    assert!(
        engine
            .direct_subclasses_at("sample.Hello", parent.start, "stale")
            .is_err()
    );
    let result = engine.class_declaration_result("sample.Hello").unwrap();
    let occurrence = &result.occurrences[0];
    let code = result.code.unwrap();
    assert_eq!(
        code.source
            .chars()
            .skip(occurrence.start)
            .take(occurrence.end - occurrence.start)
            .collect::<String>(),
        "Hello"
    );
    let declaration = engine.navigate_class("sample.Hello").unwrap();
    assert_eq!(declaration.position, occurrence.start);
    let method = code
        .links
        .iter()
        .find(|l| l.label.contains("answer("))
        .unwrap();
    assert!(
        engine
            .direct_subclasses_at("sample.Hello", method.start, &code.source_hash)
            .is_err()
    );
}

#[test]
#[ignore = "Requires RDX_TEST_APK"]
fn play_store_superclass_query_contains_clicked_class_and_valid_declaration() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeEngine::default();
    engine.open(std::path::Path::new(&path)).unwrap();
    let name = "com.google.android.finsky.applaunch.LaunchAppDeepLinkActivity";
    let code = engine.decompile_with_metadata(name).unwrap();
    let parent = code.links.iter().find(|l| l.label == "fu").unwrap();
    let (target, children) = engine
        .direct_subclasses_at(name, parent.start, &code.source_hash)
        .unwrap();
    assert_eq!(target, "fu");
    assert!(children.iter().any(|child| child == name));
    let result = engine.class_declaration_result(name).unwrap();
    let occurrence = &result.occurrences[0];
    let code = result.code.as_ref().unwrap();
    assert_eq!(
        code.source
            .chars()
            .skip(occurrence.start)
            .take(occurrence.end - occurrence.start)
            .collect::<String>(),
        "LaunchAppDeepLinkActivity"
    );
    println!("fu: {} direct subclasses", children.len());
}
