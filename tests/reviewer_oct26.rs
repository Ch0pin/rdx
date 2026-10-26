use rdx::{engine::NativeEngine, package_container::PackageContainer};
#[test]
#[ignore = "requires RDX_BASECAMP_APK"]
fn basecamp_queries_and_icon() {
    let path = std::path::PathBuf::from(std::env::var_os("RDX_BASECAMP_APK").unwrap());
    let container = PackageContainer::open(&path).unwrap();
    let mut engine = NativeEngine::start().unwrap();
    engine.open_prepared(container.clone()).unwrap();
    for path in [
        "AndroidManifest.xml",
        "res/mipmap-anydpi-v26/ic_launcher.xml",
        "res/drawable/ic_launcher_background.xml",
    ] {
        let source = engine.read_resource_with_metadata(path).unwrap().source;
        if path == "AndroidManifest.xml" {
            eprintln!(
                "application: {:?}",
                source.lines().find(|l| l.contains("<application"))
            );
        } else {
            eprintln!("{path}: {source}");
        }
    }
    for entry in engine
        .resource_table()
        .entries
        .values()
        .filter(|e| e.name.contains("launcher"))
    {
        eprintln!("{}: {:?}", entry.name, entry.variants);
    }
    let image = rdx::app_icon::load(&container, engine.resource_table());
    let image = image.expect("Basecamp launcher icon");
    assert_eq!(image.dimensions(), (48, 48));
    if let Ok(path) = std::env::var("RDX_ICON_OUTPUT") {
        image.save(path).unwrap();
    }
    let args = serde_json::json!({"query":"setAllow", "mode":"identifiers", "occurrence":"reference", "symbol_kind":"method", "limit":200});
    let started = std::time::Instant::now();
    let result = engine.search_identifiers(&args, &false.into()).unwrap();
    eprintln!(
        "setAllow: {} ms, {} references",
        started.elapsed().as_millis(),
        result["total"]
    );
    assert_eq!(result["coverage_complete"], true);
    assert_eq!(result["total"], 35);
    let summary = engine.search_identifiers(&serde_json::json!({"query":"java.io.File.delete()Z","match_mode":"exact","symbol_kind":"method","occurrence":"reference","projection":"summary"}),&false.into()).unwrap();
    eprintln!("delete summary: {summary}");
    assert_eq!(summary["total"], 84);
    assert!(summary["items"].as_array().unwrap().is_empty());
    assert!(engine.references_type("javax.net.ssl.X509TrustManager"));
    assert!(
        engine
            .implementation_names("javax.net.ssl.X509TrustManager", &false.into())
            .unwrap()
            .is_empty()
    );
}
