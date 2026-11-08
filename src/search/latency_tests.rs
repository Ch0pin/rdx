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
                        hit.origin.name.clone(),
                        hit.start,
                        hit.end,
                        hit.kind,
                        hit.origin.source_hash.clone(),
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
            &mut |_, _| {},
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
        assert!(!result.cancelled);
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
            |_, _| {},
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
        |_, _| cancel.store(true, Ordering::Relaxed),
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
                    owner = Some(hits[0].origin.name.clone());
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
                        &mut |_, _| {},
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
                                (hit.origin.name.clone(), hit.start, hit.end, hit.kind)
                            }));
                        }
                    },
                    &mut |_, _| {},
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
                assert!(!summary.cancelled);
                if let Some(expected) = &baseline {
                    assert_eq!(&hits, expected);
                } else {
                    baseline = Some(hits);
                }
            }
        }
    }
}

#[test]
#[ignore = "Set RDX_TEST_APK; phase breakdown of a cold and warm Code search"]
fn apk_code_search_profile() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(std::path::Path::new(&path)).unwrap();
    let total = project.classes.len();
    let query = CompiledQuery::new(SearchQuery {
        text: "getSharedPreferences".into(),
        ..Default::default()
    })
    .unwrap();
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let mut selected = query.ordered_classes(&project.classes);
    let ordered = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let mut cache = SearchCache::default();
    candidates::prioritize(
        &mut engine,
        &mut cache,
        &mut selected,
        &query.matcher,
        &cancel,
        &mut |_, _| {},
    );
    eprintln!(
        "SEARCH_PROFILE {}",
        serde_json::json!({
            "phase":"ordering","classes":total,"order_seconds":ordered,
            "prioritize_seconds":started.elapsed().as_secs_f64(),
        })
    );
    let count = total.min(1500);
    let sample: Vec<&String> = (0..count)
        .map(|i| &project.classes[i * total / count])
        .collect();
    let (mut render, mut prepare, mut matching) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let (mut source_bytes, mut record_bytes, mut errors, mut hits) =
        (0usize, 0usize, 0usize, 0usize);
    let mut slowest: Vec<(f64, String)> = Vec::new();
    for name in &sample {
        let t = Instant::now();
        let code = match engine.decompile_search_until(name, &|| false) {
            Ok(code) => code,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
        let took = t.elapsed();
        render += took;
        slowest.push((took.as_secs_f64(), (*name).clone()));
        source_bytes += code.source.len();
        let document = Arc::new(SearchDocument {
            target: SearchTarget::Class((*name).clone()),
            name: (*name).clone(),
            source: code.source,
            syntax: "java".into(),
            links: code.links,
            source_hash: Some(code.source_hash),
            metadata_complete: true,
        });
        let definitions = Arc::new(code.definitions);
        let value: CachedValue = Some((document.clone(), definitions.clone(), None));
        let t = Instant::now();
        let prepared = crate::search_index::PreparedEntry::prepare(&value).unwrap();
        prepare += t.elapsed();
        record_bytes += prepared.estimated_bytes();
        let t = Instant::now();
        hits += matches(document, &definitions, &query, usize::MAX).len();
        matching += t.elapsed();
    }
    slowest.sort_by(|a, b| b.0.total_cmp(&a.0));
    slowest.truncate(5);
    let per_class = |d: Duration| d.as_secs_f64() * 1000.0 / count as f64;
    let scale = total as f64 / count as f64;
    eprintln!(
        "SEARCH_PROFILE {}",
        serde_json::json!({
            "phase":"sample","sampled":count,"errors":errors,"hits":hits,
            "render_ms_per_class":per_class(render),
            "prepare_ms_per_class":per_class(prepare),
            "match_ms_per_class":per_class(matching),
            "source_bytes_per_class":source_bytes / count,
            "record_bytes_per_class":record_bytes / count,
            "projected_source_gib":source_bytes as f64 * scale / f64::from(1u32 << 30),
            "projected_record_gib":record_bytes as f64 * scale / f64::from(1u32 << 30),
            "projected_serial_render_minutes":render.as_secs_f64() * scale / 60.0,
            "slowest":slowest,
        })
    );
    let count = total.min(12_000);
    let slice: Vec<&String> = (0..count)
        .map(|i| &project.classes[i * total / count])
        .collect();
    let mut cache = SearchCache::default();
    for (mode, text) in [
        ("cold", "getSharedPreferences"),
        ("warm_same_query", "getSharedPreferences"),
        ("warm_other_query", "android.intent.action.VIEW"),
    ] {
        let query = CompiledQuery::new(SearchQuery {
            text: text.into(),
            ..Default::default()
        })
        .unwrap();
        let before = (
            cache.source_fetches,
            cache.index_rejections,
            cache.disk_hits,
        );
        let started = Instant::now();
        let summary = streaming::run(
            &mut engine,
            &mut cache,
            &slice,
            &query,
            &cancel,
            &mut |_| {},
            &mut |_, _| {},
        );
        let seconds = started.elapsed().as_secs_f64();
        let (entries, payloads, metadata, payload_bytes) = cache.index_stats();
        eprintln!(
            "SEARCH_PROFILE {}",
            serde_json::json!({
                "phase":"pool","mode":mode,"classes":count,"seconds":seconds,
                "classes_per_second":count as f64 / seconds,
                "projected_full_minutes":total as f64 / (count as f64 / seconds) / 60.0,
                "scanned":summary.scanned,"hits":summary.hits,"skipped":summary.skipped,
                "source_fetches":cache.source_fetches - before.0,
                "index_rejections":cache.index_rejections - before.1,
                "disk_hits":cache.disk_hits - before.2,
                "index_entries":entries,"index_payloads":payloads,
                "index_metadata_mib":metadata / 1048576,"index_payload_mib":payload_bytes / 1048576,
                "errors":summary.errors,
            })
        );
    }
}

#[test]
#[ignore = "Set RDX_TEST_APK; worker scaling of the Code search pool versus raw parallel rendering"]
fn apk_code_search_scaling() {
    let path = std::env::var_os("RDX_TEST_APK").unwrap();
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(std::path::Path::new(&path)).unwrap();
    let total = project.classes.len();
    let count = total.min(6000);
    let slice: Vec<&String> = (0..count)
        .map(|i| &project.classes[i * total / count])
        .collect();
    let mut times: Vec<(f64, &String)> = Vec::with_capacity(count);
    let started = Instant::now();
    for name in &slice {
        let t = Instant::now();
        let _ = engine.decompile_search_until(name, &|| false);
        times.push((t.elapsed().as_secs_f64(), name));
    }
    let serial = started.elapsed().as_secs_f64();
    times.sort_by(|a, b| b.0.total_cmp(&a.0));
    let sum: f64 = times.iter().map(|t| t.0).sum();
    let share = |n: usize| times.iter().take(n).map(|t| t.0).sum::<f64>() / sum;
    let pct = |p: f64| times[((count as f64 * (1.0 - p)) as usize).min(count - 1)].0 * 1000.0;
    let over = |ms: f64| times.iter().filter(|t| t.0 * 1000.0 >= ms).count();
    eprintln!(
        "SEARCH_SCALING {}",
        serde_json::json!({
            "phase":"distribution","classes":count,"serial_seconds":serial,
            "p50_ms":pct(0.5),"p90_ms":pct(0.9),"p99_ms":pct(0.99),"max_ms":times[0].0*1000.0,
            "top_1pct_share":share(count/100),"top_10_share":share(10),"top_1_share":share(1),
            "over_100ms":over(100.0),"over_500ms":over(500.0),"over_1s":over(1000.0),
            "slowest":times.iter().take(8).map(|t|(t.0,t.1.as_str())).collect::<Vec<_>>(),
        })
    );
    for workers in [1usize, 4, 12] {
        let started = Instant::now();
        std::thread::scope(|scope| {
            for w in 0..workers {
                let mut worker = engine.clone();
                let names: Vec<&String> = slice.iter().skip(w).step_by(workers).copied().collect();
                scope.spawn(move || {
                    for name in names {
                        let _ = worker.decompile_search_until(name, &|| false);
                    }
                });
            }
        });
        let seconds = started.elapsed().as_secs_f64();
        eprintln!(
            "SEARCH_SCALING {}",
            serde_json::json!({"phase":"raw_parallel","workers":workers,"classes":count,"seconds":seconds,
                "classes_per_second":count as f64/seconds,"speedup":serial/seconds})
        );
    }
    let query = CompiledQuery::new(SearchQuery {
        text: "getSharedPreferences".into(),
        ..Default::default()
    })
    .unwrap();
    let cancel = AtomicBool::new(false);
    for workers in [1usize, 4, 12] {
        let mut cache = SearchCache::default();
        let started = Instant::now();
        let summary = streaming::run_with_workers(
            &mut engine,
            &mut cache,
            &slice,
            &query,
            &cancel,
            &mut |_| {},
            &mut |_, _| {},
            workers,
        );
        let seconds = started.elapsed().as_secs_f64();
        eprintln!(
            "SEARCH_SCALING {}",
            serde_json::json!({"phase":"pool","workers":workers,"classes":count,"seconds":seconds,
                "classes_per_second":count as f64/seconds,"speedup":serial/seconds,
                "scanned":summary.scanned,"hits":summary.hits,"errors":summary.errors.len()})
        );
    }
}
