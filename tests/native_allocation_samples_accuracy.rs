//! Actual-corpus regressions for the representative allocation failure families.
use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
#[ignore = "Set RDX_TEST_APK to the reported Zoom APK and run --ignored"]
fn extracted_allocation_samples_keep_effects_overloads_and_navigation() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    for (class, name) in [("_COROUTINE.b", "b"), ("a3.a", "G")] {
        let code = engine.decompile_with_metadata(class).unwrap();
        let definition = code
            .definitions
            .iter()
            .find(|d| d.kind == "method" && d.name == name)
            .unwrap();
        let end = code
            .definitions
            .iter()
            .filter(|d| d.kind == "method" && d.start > definition.start)
            .map(|d| d.start)
            .min()
            .unwrap_or(code.source.chars().count());
        let body: String = code
            .source
            .chars()
            .skip(definition.start)
            .take(end - definition.start)
            .collect();
        if name == "b" {
            // DEX resolves the second constructor argument "_" only after the
            // calls producing the third and fourth arguments. Java argument
            // order cannot preserve that trace with the current expression IR.
            assert!(
                body.contains(".end method"),
                "strict-order fallback: {body}"
            );
            assert!(body.find("getFileName()").unwrap() < body.find("getLineNumber()").unwrap());
            assert!(body.find("getLineNumber()").unwrap() < body.find("\"_\"").unwrap());
            assert!(body.contains("append(C)"), "original overload: {body}");
            continue;
        }
        assert!(!body.contains(".end method"), "{class}.{name}: {body}");
        assert_eq!(body.matches("Boolean.valueOf(").count(), 1, "{body}");
        assert!(body.contains("((Object) "), "{body}");
        assert!(
            body.find("SET_SHOW_LABEL_EXTENSION").unwrap() < body.find("Boolean.valueOf(").unwrap()
        );
        let expected = [
            "cl.a.<init>(Lus/zoom/component/businessline/meeting/render/operation/ZmRenderOperationType;Ljava/lang/Object;)V",
        ];
        for raw in expected {
            let link = code
                .links
                .iter()
                .find(|l| l.start > definition.start && l.start < end && l.label == raw)
                .expect("original overloaded call link");
            assert_eq!(
                engine
                    .resolve_usage_target(class, link.start, &code.source_hash)
                    .unwrap()
                    .id,
                raw
            );
        }
    }
}
