//! End-to-end coverage checks and an opt-in bounded real-APK latency probe.
use super::*;
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

#[test]
fn ranked_search_preserves_complete_source_results_across_queries() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(&path).unwrap();
    let cancel = AtomicBool::new(false);
    // Include generated Java syntax/locals, regex, case sensitivity, and absence.
    for (text, regex, case_sensitive) in [
        ("doubleValue", false, false),
        ("sample.Target", false, false),
        ("return ", false, false),
        ("int ", false, true),
        ("(?i)double.*value", true, false),
        ("DOUBLeVALUE", false, false),
        ("DOUBLeVALUE", false, true),
        ("not-present-K", false, false),
    ] {
        let query = CompiledQuery::new(SearchQuery {
            text: text.into(),
            regex,
            case_sensitive,
            excluded_packages: Vec::new(),
            ..Default::default()
        })
        .unwrap();
        let mut expected = BTreeSet::new();
        let mut actual = BTreeSet::new();
        let collect = |set: &mut BTreeSet<_>, update| {
            if let SearchUpdate::Batch(batch) = update {
                set.extend(batch.into_iter().map(|hit| {
                    (
                        hit.document.name.clone(),
                        hit.start,
                        hit.end,
                        hit.kind,
                        hit.document.source_hash.clone(),
                    )
                }));
            }
        };
        let baseline = streaming::run_with_workers(
            &mut engine,
            &mut SearchCache::default(),
            &query.ordered_classes(&project.classes),
            &query,
            &cancel,
            &mut |u| collect(&mut expected, u),
            &mut |_| {},
            1,
        );
        let mut cache = SearchCache::default();
        let result = run_search(
            &mut engine,
            &mut cache,
            1,
            &project.classes,
            None,
            &query,
            &cancel,
            |u| collect(&mut actual, u),
        );
        assert_eq!(actual, expected, "query {text}");
        assert_eq!(result.scanned, baseline.scanned);
        assert_eq!(result.skipped, 0);
        assert!(!result.cancelled && !result.limited);
        // A fully indexed repeat must not revisit the metadata planning pass.
        let fetched = cache.source_fetches;
        let warm = run_search_interactive(
            &mut engine,
            &mut cache,
            1,
            &project.classes,
            None,
            &query,
            &cancel,
            |_| {},
            |_| {},
        );
        assert_eq!(warm.hits, result.hits);
        assert_eq!(cache.source_fetches, fetched);
    }
}

#[test]
fn candidate_planning_is_interactive_and_cancellable() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(&path).unwrap();
    let query = CompiledQuery::new(SearchQuery {
        text: "doubleValue".into(),
        ..Default::default()
    })
    .unwrap();
    let cancel = AtomicBool::new(false);
    let mut cache = SearchCache::default();
    let summary = run_search_interactive(
        &mut engine,
        &mut cache,
        1,
        &project.classes,
        None,
        &query,
        &cancel,
        |_| {},
        |_| cancel.store(true, Ordering::Relaxed),
    );
    assert!(summary.cancelled);
    assert_eq!(cache.source_fetches, 0);
}

#[test]
#[ignore = "Set RDX_TEST_APK; bounded 10s baseline and 30s first-result probes"]
fn apk_general_search_first_results() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(std::path::Path::new(&path)).unwrap();
    for (text, regex) in [
        ("parseuri", false),
        ("getSharedPreferences", false),
        ("android.intent.action.VIEW", false),
        ("(?i)overridePendingTransition", true),
    ] {
        let query = CompiledQuery::new(SearchQuery {
            text: text.into(),
            regex,
            ..Default::default()
        })
        .unwrap();
        let mut cache = SearchCache::default();
        for mode in ["baseline", "ranked", "repeat"] {
            let cancel = AtomicBool::new(false);
            let started = Instant::now();
            let mut first = None;
            let mut owner = None;
            let mut emit = |update| {
                if let SearchUpdate::Batch(hits) = update {
                    first = Some(started.elapsed().as_secs_f64());
                    owner = Some(hits[0].document.name.clone());
                    cancel.store(true, Ordering::Relaxed);
                }
            };
            let before = cache.source_fetches;
            let mut baseline_fetches = 0;
            // Watchdog also interrupts a slow class render, not just class boundaries.
            let summary = std::thread::scope(|scope| {
                let (done, wait) = std::sync::mpsc::channel();
                let cancel_ref = &cancel;
                scope.spawn(move || {
                    let seconds = if mode == "baseline" { 10 } else { 30 };
                    if wait.recv_timeout(Duration::from_secs(seconds)).is_err() {
                        cancel_ref.store(true, Ordering::Relaxed);
                    }
                });
                let summary = if mode == "baseline" {
                    let mut baseline_cache = SearchCache::default();
                    let summary = streaming::run_with_workers(
                        &mut engine,
                        &mut baseline_cache,
                        &query.ordered_classes(&project.classes),
                        &query,
                        &cancel,
                        &mut emit,
                        &mut |_| {},
                        1,
                    );
                    baseline_fetches = baseline_cache.source_fetches;
                    summary
                } else {
                    run_search(
                        &mut engine,
                        &mut cache,
                        1,
                        &project.classes,
                        None,
                        &query,
                        &cancel,
                        &mut emit,
                    )
                };
                let _ = done.send(());
                summary
            });
            eprintln!(
                "SEARCH_LATENCY {}",
                serde_json::json!({
                    "query":text, "mode":mode, "first_seconds":first, "owner":owner,
                    "elapsed_seconds":started.elapsed().as_secs_f64(), "scanned":summary.scanned,
                    "total":summary.total, "source_fetches":if mode == "baseline" { baseline_fetches } else { cache.source_fetches-before },
                    "errors":summary.errors,
                })
            );
            if mode != "baseline" {
                assert!(
                    first.is_some(),
                    "{mode}: {text}: no result within 30 seconds"
                );
            }
        }
    }
}

#[test]
#[ignore = "Set RDX_TEST_APK; complete serial/parallel parity on 500 real classes"]
fn apk_general_search_complete_sample() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(std::path::Path::new(&path)).unwrap();
    let count = project.classes.len().min(500);
    let classes: Vec<_> = (0..count)
        .map(|i| &project.classes[i * project.classes.len() / count])
        .collect();
    for text in [
        "parseuri",
        "getSharedPreferences",
        "android.intent.action.VIEW",
    ] {
        let query = CompiledQuery::new(SearchQuery {
            text: text.into(),
            ..Default::default()
        })
        .unwrap();
        let mut baseline = None;
        for workers in [1, 4] {
            let mut cache = SearchCache::default();
            for mode in ["cold", "warm"] {
                let mut hits = BTreeSet::new();
                let started = Instant::now();
                let before = cache.source_fetches;
                let rejected = cache.index_rejections;
                let summary = streaming::run_with_workers(
                    &mut engine,
                    &mut cache,
                    &classes,
                    &query,
                    &AtomicBool::new(false),
                    &mut |update| {
                        if let SearchUpdate::Batch(batch) = update {
                            hits.extend(batch.into_iter().map(|hit| {
                                (hit.document.name.clone(), hit.start, hit.end, hit.kind)
                            }));
                        }
                    },
                    &mut |_| {},
                    workers,
                );
                eprintln!(
                    "SEARCH_COMPLETE {}",
                    serde_json::json!({
                        "query":text, "workers":workers, "mode":mode, "seconds":started.elapsed().as_secs_f64(),
                        "scanned":summary.scanned, "hits":summary.hits, "source_fetches":cache.source_fetches-before,
                        "index_rejections":cache.index_rejections-rejected, "errors":summary.errors,
                    })
                );
                assert_eq!(summary.scanned, count);
                assert_eq!(summary.skipped, 0);
                assert!(!summary.cancelled && !summary.limited);
                if let Some(expected) = &baseline {
                    assert_eq!(&hits, expected);
                } else {
                    baseline = Some(hits);
                }
            }
        }
    }
}
