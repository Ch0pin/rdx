use rdx::engine::{DecompilerEngine, NativeEngine};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::atomic::AtomicBool,
};

#[test]
fn indexed_candidates_preserve_full_source_link_results_and_repeat_queries() {
    for fixture in ["navigation.apk", "navigation.dex"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let mut engine = NativeEngine::start().unwrap();
        let project = engine.open(&path).unwrap();
        let mut oracle: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for owner in &project.classes {
            let code = engine.decompile_with_metadata(owner).unwrap();
            for link in &code.links {
                if !code
                    .definitions
                    .iter()
                    .any(|d| d.start == link.start && d.end == link.end)
                {
                    oracle
                        .entry(link.label.clone())
                        .or_default()
                        .insert(owner.clone());
                }
            }
        }
        assert!(!oracle.is_empty(), "fixture must exercise references");
        let cancel = AtomicBool::new(false);
        assert!(
            engine
                .usage_index_candidates("missing.Owner.absent()V", &cancel, |_, _| {})
                .unwrap()
                .is_empty(),
            "known-good fixtures must not fall back to a full scan"
        );
        for (target, expected) in oracle {
            let candidates = engine
                .usage_index_candidates(&target, &cancel, |_, _| {})
                .unwrap();
            let again = engine
                .usage_index_candidates(&target, &cancel, |_, _| {})
                .unwrap();
            assert_eq!(candidates, again, "repeated {target}");
            let candidate_set: BTreeSet<_> = candidates.iter().cloned().collect();
            assert!(
                expected.is_subset(&candidate_set),
                "missing owners for {fixture}: {target}: expected {expected:?}, got {candidate_set:?}"
            );
            let actual: BTreeSet<_> = candidates
                .iter()
                .filter(|owner| {
                    let result = engine.usages_in_class(&target, owner).unwrap();
                    if let Some(code) = result.code {
                        let chars: Vec<_> = code.source.chars().collect();
                        for occurrence in &result.occurrences {
                            assert!(
                                occurrence.start < occurrence.end && occurrence.end <= chars.len()
                            );
                            assert!(code.links.iter().any(|link| link.label == target
                                && link.start == occurrence.start
                                && link.end == occurrence.end));
                        }
                    }
                    !result.occurrences.is_empty()
                })
                .cloned()
                .collect();
            assert_eq!(actual, expected, "full-scan parity for {fixture}: {target}");
        }
        assert!(engine.usage_index_stats().is_some());
    }
}

#[test]
fn cancelled_lookup_and_project_replacement_do_not_reuse_stale_candidates() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut engine = NativeEngine::start().unwrap();
    engine.open(&fixtures.join("navigation.apk")).unwrap();
    let cancel = AtomicBool::new(false);
    let target = "sample.Target.doubleValue(I)I";
    assert!(
        !engine
            .usage_index_candidates(target, &cancel, |_, _| {})
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .usage_index_candidates(target, &AtomicBool::new(true), |_, _| {})
            .is_none()
    );
    assert!(
        !engine
            .usage_index_candidates(target, &cancel, |_, _| {})
            .unwrap()
            .is_empty()
    );
    engine.open(&fixtures.join("hello.apk")).unwrap();
    assert!(
        engine
            .usage_index_candidates(target, &cancel, |_, _| {})
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .open(&fixtures.join("missing-index-fixture.apk"))
            .is_err()
    );
    assert!(engine.usage_index_stats().is_none());
    assert!(
        engine
            .usage_index_candidates(target, &cancel, |_, _| {})
            .is_none()
    );
}
