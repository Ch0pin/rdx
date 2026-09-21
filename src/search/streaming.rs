//! Native in-process search. No worker protocol, source handoff files or JVM.
use super::*;
use std::time::{Duration, Instant};

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    engine: &mut NativeEngine,
    cache: &mut SearchCache,
    classes: &[&String],
    query: &CompiledQuery,
    cancel: &AtomicBool,
    emit: &mut impl FnMut(SearchUpdate),
    interactive: &mut impl FnMut(&mut NativeEngine),
) -> SearchSummary {
    let mut summary = SearchSummary {
        total: classes.len(),
        ..Default::default()
    };
    let mut retained = 0;
    let mut last_progress = Instant::now();
    for name in classes {
        interactive(engine);
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        let key = SearchTarget::Class((**name).clone());
        summary.scanned += 1;
        if cache.disk.rejects(
            &key,
            &query.query.text,
            query.query.case_sensitive,
            query.query.regex,
            false,
        ) {
            cache.index_rejections += 1;
        } else {
            let cached = cache.get(&key).or_else(|| {
                let value = cache.disk.load(&key);
                if value.is_some() {
                    cache.disk_hits += 1;
                }
                value
            });
            let value = match cached {
                Some(value) => value,
                None => {
                    cache.source_fetches += 1;
                    match engine.decompile_with_metadata(name) {
                        Ok(code) => {
                            let document = Arc::new(SearchDocument {
                                target: key.clone(),
                                name: (**name).clone(),
                                source: code.source,
                                syntax: "java".into(),
                                links: code.links,
                                source_hash: Some(code.source_hash),
                                metadata_complete: true,
                            });
                            let value = Some((document, Arc::new(code.definitions), None));
                            cache.insert(key, value.clone());
                            value
                        }
                        Err(error) => {
                            record_error(&mut summary, format!("{name}: {error:#}"));
                            None
                        }
                    }
                }
            };
            if let Some((document, definitions, note)) = value {
                if let Some(note) = note {
                    summary.skipped += 1;
                    record_error(&mut summary, note);
                }
                let hits = matches(
                    document.clone(),
                    &definitions,
                    query,
                    MAX_HITS - summary.hits,
                );
                if !hits.is_empty() {
                    let cost = cached_cost(
                        &document.target,
                        &Some((document.clone(), definitions, None)),
                    );
                    if retained + cost > MAX_RETAINED {
                        summary.limited = true;
                        break;
                    }
                    retained += cost;
                    summary.hits += hits.len();
                    emit(SearchUpdate::Batch(hits));
                    if summary.hits >= MAX_HITS {
                        summary.limited = true;
                        break;
                    }
                }
            } else {
                summary.skipped += 1;
            }
        }
        if last_progress.elapsed() >= Duration::from_millis(50) {
            emit(SearchUpdate::Progress {
                scanned: summary.scanned,
                total: summary.total,
                hits: summary.hits,
                skipped: summary.skipped,
            });
            last_progress = Instant::now();
        }
    }
    summary.cancelled |= cancel.load(Ordering::Relaxed);
    summary.retained_bytes = retained;
    emit(SearchUpdate::Progress {
        scanned: summary.scanned,
        total: summary.total,
        hits: summary.hits,
        skipped: summary.skipped,
    });
    summary
}
