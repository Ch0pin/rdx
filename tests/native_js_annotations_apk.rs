//! Optional check against an independent raw-DEX JavascriptInterface inventory.
use rdx::engine::{DecompilerEngine, NativeEngine, source_identity};
use std::collections::BTreeMap;

#[test]
#[ignore = "Set RDX_ANNOTATION_APK and RDX_JS_INVENTORY; run --ignored"]
fn all_independently_inventoried_js_methods_have_visible_annotations_and_navigation() {
    let path = std::env::var_os("RDX_ANNOTATION_APK").expect("APK required");
    let inventory = std::env::var_os("RDX_JS_INVENTORY").expect("independent inventory required");
    let inventory: serde_json::Value =
        serde_json::from_slice(&std::fs::read(inventory).unwrap()).unwrap();
    let mut classes = BTreeMap::<String, Vec<String>>::new();
    for entry in inventory["methods"].as_array().unwrap() {
        classes
            .entry(entry["class"].as_str().unwrap().into())
            .or_default()
            .push(entry["method"].as_str().unwrap().into());
    }
    assert!(!classes.is_empty());
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let mut checked = 0;
    let mut fallback_classes = 0;
    for (class, names) in classes {
        let code = engine.decompile_with_metadata(&class).unwrap();
        assert_eq!(code.source_hash, source_identity(&code.source));
        let annotation_count = code
            .source
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                line.starts_with("@JavascriptInterface")
                    || line.starts_with("@android.webkit.JavascriptInterface")
            })
            .count();
        assert_eq!(
            annotation_count,
            names.len(),
            "annotation count in {class}:\n{}",
            code.source
        );
        if code.source.contains(".end method") {
            fallback_classes += 1;
        }
        let chars: Vec<_> = code.source.chars().collect();
        let fields: Vec<_> = code
            .definitions
            .iter()
            .filter(|d| d.kind == "field")
            .collect();
        for pair in fields.windows(2) {
            let between: String = chars[pair[0].end..pair[1].start].iter().collect();
            assert!(
                !between.contains("\n\n"),
                "extra blank field line in {class}: {between}"
            );
        }
        let definitions: Vec<_> = code
            .definitions
            .iter()
            .filter(|d| d.kind == "method")
            .collect();
        for name in &names {
            assert!(
                definitions.iter().enumerate().any(|(index, definition)| {
                    if &definition.name != name {
                        return false;
                    }
                    let previous = if index == 0 {
                        0
                    } else {
                        definitions[index - 1].end
                    };
                    let prefix: String = chars[previous..definition.start].iter().collect();
                    prefix.lines().any(|line| {
                        let line = line.trim_start();
                        line.starts_with("@JavascriptInterface")
                            || line.starts_with("@android.webkit.JavascriptInterface")
                    })
                }),
                "missing annotation on {class}.{name}"
            );
        }
        // Prefix insertion must preserve clickable declaration ranges.
        let definition = definitions
            .iter()
            .find(|d| names.contains(&d.name))
            .unwrap();
        assert!(definition.start < definition.end && definition.end <= chars.len());
        let destination = engine
            .navigate(&class, definition.start, &code.source_hash)
            .unwrap();
        assert_eq!(destination.class, class);
        assert!(
            destination
                .code
                .definitions
                .iter()
                .any(|d| d.kind == "method"
                    && d.name == definition.name
                    && d.start == destination.position)
        );
        checked += annotation_count;
    }
    assert_eq!(checked as u64, inventory["count"].as_u64().unwrap());
    eprintln!(
        "Verified {checked} JavascriptInterface annotations; {fallback_classes} classes include DEX fallback bodies"
    );
}
