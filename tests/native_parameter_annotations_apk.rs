//! Compare emitted parameter annotations with an independent raw DEX inventory.
use rdx::engine::{DecompilerEngine, NativeEngine};
use std::collections::BTreeMap;

#[test]
#[ignore = "requires RDX_ANNOTATION_APK and RDX_PARAMETER_INVENTORY"]
fn inventoried_parameter_annotations_are_visible_and_navigable() {
    let apk = std::env::var_os("RDX_ANNOTATION_APK").expect("APK required");
    let inventory = std::env::var_os("RDX_PARAMETER_INVENTORY").expect("inventory required");
    let data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(inventory).unwrap()).unwrap();
    let mut classes = BTreeMap::<String, BTreeMap<(String, String), usize>>::new();
    for entry in data["parameter_annotations"].as_array().unwrap() {
        let key = (
            entry["method"].as_str().unwrap().to_owned(),
            entry["annotation"].as_str().unwrap().to_owned(),
        );
        *classes
            .entry(entry["class"].as_str().unwrap().to_owned())
            .or_default()
            .entry(key)
            .or_default() += 1;
    }
    assert!(!classes.is_empty());
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&apk)).unwrap();
    let mut count = 0;
    let mut checked_types = std::collections::HashSet::new();
    for (class, expected) in &classes {
        let code = engine.decompile_with_metadata(class).unwrap();
        let chars = code.source.chars().collect::<Vec<_>>();
        for ((method, annotation), expected_count) in expected {
            let mut actual = 0;
            for definition in code
                .definitions
                .iter()
                .filter(|d| d.kind == "method" && d.name == *method)
            {
                let end = chars[definition.end..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(chars.len(), |n| definition.end + n);
                for link in code.links.iter().filter(|l| {
                    l.label == *annotation
                        && l.start > definition.end
                        && l.end <= end
                        && chars.get(l.start - 1) == Some(&'@')
                }) {
                    actual += 1;
                    if checked_types.insert(annotation.clone()) {
                        let target = engine
                            .navigate(class, link.start, &code.source_hash)
                            .unwrap();
                        assert_eq!(target.class, *annotation);
                        assert!(
                            target.code.source.contains("@interface "),
                            "{}",
                            target.code.source
                        );
                        assert!(!target.code.source.contains("Native DEX disassembly"));
                    }
                }
            }
            assert_eq!(
                actual, *expected_count,
                "{class}.{method} @{annotation}\n{}",
                code.source
            );
            count += actual;
        }
    }
    println!(
        "Verified {count} parameter annotations in {} classes and navigation to {} annotation types",
        classes.len(),
        checked_types.len()
    );
}
