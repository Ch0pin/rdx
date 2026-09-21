#[path = "../src/manifest_links.rs"]
mod manifest_links;
use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
#[ignore = "Set RDX_MANIFEST_APK and run --ignored"]
fn actual_archive_manifest_links_open_component_declarations() {
    let path = std::env::var_os("RDX_MANIFEST_APK").expect("RDX_MANIFEST_APK required");
    let path = std::path::Path::new(&path);
    let archive = rdx::apk::Archive::open(path).unwrap();
    let entry = archive
        .entries
        .iter()
        .find(|e| e.path == "AndroidManifest.xml")
        .unwrap();
    let rdx::apk::Preview::Text { text, .. } = archive.preview(entry.index).unwrap() else {
        panic!("manifest not decoded")
    };
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(path).unwrap();
    let links = manifest_links::links(&text, &project.classes);
    assert!(
        !links.is_empty(),
        "no manifest component links; decoded prefix: {}",
        text.chars().take(1200).collect::<String>()
    );
    eprintln!("{} component links", links.len());
    for link in links.iter().take(3) {
        let started = std::time::Instant::now();
        let destination = engine.navigate_class(&link.label).unwrap();
        eprintln!("{}: {:.3}s", link.label, started.elapsed().as_secs_f64());
        assert_eq!(destination.class, link.label);
        assert!(
            destination
                .code
                .definitions
                .iter()
                .any(|d| d.kind == "class" && d.start == destination.position)
        );
    }
}
