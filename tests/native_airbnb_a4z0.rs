use rdx::engine::{DecompilerEngine, NativeEngine, source_identity};
use std::{path::Path, sync::atomic::AtomicBool};

/// Both reported methods must reconstruct completely in the actual loaded APK.
/// Generic control-flow semantic fixtures accompany the implementation.
#[test]
#[ignore = "requires the pinned Airbnb APK in RDX_TEST_APK; run serially"]
fn airbnb_a4z0_reader_and_svg_methods_have_complete_java_and_navigation() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeEngine::default();
    engine.open(Path::new(&path)).unwrap();
    let cancel = AtomicBool::new(false);
    for name in ["ǃ", "invoke"] {
        let id = format!("a4z0.{name}(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;");
        let (code, fallback) = engine.decompile_method_source(&id, &cancel).unwrap();
        assert!(fallback.is_none(), "{id}: {fallback:?}");
        assert!(!code.source.contains(".method "), "{id}");
        assert_eq!(code.source_hash, source_identity(&code.source));
        assert!(
            code.source.contains("while (") || code.source.contains("for ("),
            "{id}: loop missing"
        );
        let chars = code.source.chars().count();
        assert!(code.links.iter().all(|l| l.start < l.end && l.end <= chars));
        assert!(
            code.definitions
                .iter()
                .any(|d| d.kind == "method" && d.name == name)
        );
    }
    let class = engine.decompile_with_metadata("a4z0").unwrap();
    assert!(!class.source.contains(".method a4z0."));
    for name in ["ǃ", "invoke"] {
        let declaration = class
            .definitions
            .iter()
            .find(|d| d.kind == "method" && d.name == name)
            .unwrap();
        let target = engine
            .navigate("a4z0", declaration.start, &class.source_hash)
            .unwrap();
        assert_eq!(target.position, declaration.start);
        assert_eq!(target.class, "a4z0");
    }
}
