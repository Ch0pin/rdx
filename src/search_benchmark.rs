//! Reproducible end-to-end search comparison, without printing decompiled source.
use crate::search::{self, CompiledQuery, SearchCache, SearchQuery, SearchUpdate};
use anyhow::{Context, Result, ensure};
use rdx::engine::{DecompilerEngine, NativeEngine};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, sync::atomic::AtomicBool, time::Instant};

type Matches = BTreeSet<(String, usize, usize, String)>;

fn run(
    engine: &mut NativeEngine,
    cache: &mut SearchCache,
    classes: &[String],
    query: &CompiledQuery,
) -> (Value, Matches) {
    let mut hits = BTreeSet::new();
    let before = (
        cache.hits,
        cache.disk_hits,
        cache.index_rejections,
        cache.source_fetches,
    );
    let started = Instant::now();
    let mut first_result_seconds = None;
    let mut matched_texts = BTreeSet::new();
    let summary = search::run_search(
        engine,
        cache,
        1,
        classes,
        None,
        query,
        &AtomicBool::new(false),
        |update| {
            if let SearchUpdate::Batch(batch) = update {
                first_result_seconds.get_or_insert_with(|| started.elapsed().as_secs_f64());
                for hit in &batch {
                    let text: String = hit
                        .document
                        .source
                        .chars()
                        .skip(hit.start)
                        .take(hit.end - hit.start)
                        .collect();
                    matched_texts.insert((hit.document.name.clone(), hit.kind.clone(), text));
                }
                hits.extend(
                    batch
                        .into_iter()
                        .map(|hit| (hit.document.name.clone(), hit.start, hit.end, hit.kind)),
                );
            }
        },
    );
    (
        json!({
            "seconds": started.elapsed().as_secs_f64(), "scanned": summary.scanned,
            "total": summary.total, "hits": summary.hits, "skipped": summary.skipped,
            "limited": summary.limited, "errors": summary.errors,
            "memory_hits": cache.hits - before.0, "disk_hits": cache.disk_hits - before.1,
            "index_rejections": cache.index_rejections - before.2,
            "source_fetches": cache.source_fetches - before.3,
            "first_result_seconds": first_result_seconds,
            "matched_texts": matched_texts,
        }),
        hits,
    )
}

fn variant(
    path: &Path,
    query: &CompiledQuery,
    limit: usize,
    legacy: bool,
) -> Result<(Value, Matches)> {
    let load = Instant::now();
    let mut engine = NativeEngine::start()?;
    let project = engine.open(path)?;
    let load_seconds = load.elapsed().as_secs_f64();
    let count = limit.min(project.classes.len());
    let classes: Vec<_> = (0..count)
        .map(|i| project.classes[i * project.classes.len() / count].clone())
        .collect();
    let mut cache = if legacy {
        SearchCache::legacy_for_benchmark()
    } else {
        SearchCache::default()
    };
    let (cold, cold_hits) = run(&mut engine, &mut cache, &classes, query);
    let (warm, warm_hits) = run(&mut engine, &mut cache, &classes, query);
    ensure!(cold_hits == warm_hits, "Cold/warm result parity failed");
    ensure!(
        cold["skipped"] == 0 && warm["skipped"] == 0,
        "Benchmark has skipped documents: cold={cold}, warm={warm}"
    );
    ensure!(
        cold["limited"] == false && warm["limited"] == false,
        "Choose a narrower query: result limits make this comparison incomplete"
    );
    Ok((
        json!({"load_seconds":load_seconds,"project_classes":project.classes.len(),"corpus":classes,"cold":cold,"warm":warm}),
        warm_hits,
    ))
}

pub fn execute(path: &Path, text: &str, limit: usize) -> Result<()> {
    ensure!(
        (1..=20_000).contains(&limit),
        "Class limit must be 1–20,000"
    );
    let query = CompiledQuery::new(SearchQuery {
        text: text.into(),
        excluded_packages: Vec::new(),
        ..Default::default()
    })
    .map_err(anyhow::Error::msg)?;
    eprintln!("Benchmarking previous search (cold and warm), up to {limit} classes…");
    let (legacy, old_hits) =
        variant(path, &query, limit, true).context("Previous search baseline")?;
    eprintln!("Benchmarking indexed search (cold and warm), same corpus…");
    let (indexed, new_hits) = variant(path, &query, limit, false).context("Indexed search")?;
    ensure!(
        legacy["corpus"] == indexed["corpus"] && old_hits == new_hits,
        "Baseline/index result parity failed"
    );
    let seconds = |v: &Value, key: &str| v[key]["seconds"].as_f64().unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "apk":path.canonicalize()?,"apk_bytes":std::fs::metadata(path)?.len(),
            "query":text,"scope":"Code; case-insensitive literal; resources excluded", "parity":true,
            "legacy":legacy,"indexed":indexed,
            "cold_speedup":seconds(&legacy,"cold") / seconds(&indexed,"cold"),
            "warm_speedup":seconds(&legacy,"warm") / seconds(&indexed,"warm"),
            "first_to_indexed_repeat_speedup":seconds(&legacy,"cold") / seconds(&indexed,"warm"),
            "note":"Fresh native engine per variant. Loading excluded from search timings. Warm-to-warm is the repeat-search comparison; first-to-repeat is not cold speedup."
        }))?
    );
    Ok(())
}

/// Benchmark the currently compiled implementation, for before/after binary comparisons.
pub fn execute_current(path: &Path, text: &str, limit: usize) -> Result<()> {
    ensure!(
        (1..=200_000).contains(&limit),
        "Class limit must be 1–200,000"
    );
    let query = CompiledQuery::new(SearchQuery {
        text: text.into(),
        excluded_packages: Vec::new(),
        ..Default::default()
    })
    .map_err(anyhow::Error::msg)?;
    let (result, hits) = variant(path, &query, limit, false)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "query":text, "apk":path.canonicalize()?, "result":result, "matches":hits,
            "note":"Current implementation; native Rust engine; load excluded; cold/warm exact match parity checked"
        }))?
    );
    Ok(())
}
