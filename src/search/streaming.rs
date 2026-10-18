//! Native in-process search. No worker protocol, source handoff files or JVM.
use super::*;
use std::time::{Duration, Instant};
use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex, mpsc},
};

enum Prepared {
    Rejected,
    Cached,
    Rendering,
}

// Reservations cover the result channel and coordinator's out-of-order map.
// The required head may exceed the budget to guarantee forward progress. Each
// worker can additionally hold one rendered document while awaiting reservation.
struct ResultBudget {
    state: Mutex<(usize, usize, bool)>, // reserved bytes, required index, stopped
    wake: Condvar,
    limit: usize,
}
impl ResultBudget {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new((0, 0, false)),
            wake: Condvar::new(),
            limit,
        }
    }
    fn reserve(&self, index: usize, bytes: usize, cancel: &AtomicBool) -> bool {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.2 || cancel.load(Ordering::Relaxed) {
                return false;
            }
            if index == state.1 || bytes <= self.limit.saturating_sub(state.0) {
                state.0 = state.0.saturating_add(bytes);
                return true;
            }
            state = self
                .wake
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap()
                .0;
        }
    }
    fn head(&self, index: usize) {
        self.state.lock().unwrap().1 = index;
        self.wake.notify_all();
    }
    fn release(&self, bytes: usize) {
        let mut state = self.state.lock().unwrap();
        state.0 = state.0.saturating_sub(bytes);
        self.wake.notify_all();
    }
    fn stop(&self) {
        self.state.lock().unwrap().2 = true;
        self.wake.notify_all();
    }
}
type Rendered =
    std::result::Result<(CachedValue, Option<crate::search_index::PreparedEntry>), String>;
fn render_job(
    worker: &mut NativeEngine,
    name: &str,
    legacy: bool,
    cancelled: impl Fn() -> bool,
) -> Rendered {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let code = worker
            .decompile_search_until(name, &cancelled)
            .map_err(|error| format!("{error:#}"))?;
        let document = Arc::new(SearchDocument {
            target: SearchTarget::Class(name.into()),
            name: name.into(),
            source: code.source,
            syntax: "java".into(),
            links: code.links,
            source_hash: Some(code.source_hash),
            metadata_complete: true,
        });
        let value = Some((document, Arc::new(code.definitions), None));
        let prepared = if legacy {
            None
        } else {
            crate::search_index::PreparedEntry::prepare_cancellable(&value, &cancelled)
        };
        if cancelled() {
            return Err("Search cancelled".into());
        }
        Ok((value, prepared))
    }))
    .unwrap_or_else(|_| Err("Native source renderer panicked".into()))
}
fn rendered_bytes(result: &Rendered) -> usize {
    match result {
        Err(error) => error.capacity(),
        Ok((value, prepared)) => {
            value
                .as_ref()
                .map_or(0, |(doc, _, _)| queued_cost(&doc.target, value))
                + prepared
                    .as_ref()
                    .map_or(0, crate::search_index::PreparedEntry::estimated_bytes)
        }
    }
}

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
    let workers =
        std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).clamp(1, 12));
    run_with_workers(
        engine,
        cache,
        classes,
        query,
        cancel,
        emit,
        interactive,
        workers,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_with_workers(
    engine: &mut NativeEngine,
    cache: &mut SearchCache,
    classes: &[&String],
    query: &CompiledQuery,
    cancel: &AtomicBool,
    emit: &mut impl FnMut(SearchUpdate),
    interactive: &mut impl FnMut(&mut NativeEngine),
    workers: usize,
) -> SearchSummary {
    let workers = workers.clamp(1, 16);
    let window = workers * 16;
    let budget = ResultBudget::new(128 * 1024 * 1024);
    let legacy = cache.legacy;
    let stop = AtomicBool::new(false);
    let started = std::sync::atomic::AtomicUsize::new(0);
    let summary = std::thread::scope(|scope| {
        let (jobs_tx, jobs_rx) = mpsc::sync_channel::<usize>(window);
        let jobs_rx = Arc::new(Mutex::new(jobs_rx));
        let (results_tx, results_rx) = mpsc::sync_channel(window);
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let mut worker = engine.clone();
            let jobs_rx = jobs_rx.clone();
            let results_tx = results_tx.clone();
            let stop = &stop;
            let started = &started;
            let budget = &budget;
            handles.push(scope.spawn(move || {
                loop {
                    let Ok(index) = jobs_rx.lock().unwrap().recv() else {
                        break;
                    };
                    if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    started.fetch_add(1, Ordering::Relaxed);
                    let result = render_job(&mut worker, classes[index], legacy, || {
                        stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed)
                    });
                    let bytes = rendered_bytes(&result);
                    if !budget.reserve(index, bytes, cancel) {
                        break;
                    }
                    if results_tx.send((index, result, bytes)).is_err() {
                        budget.release(bytes);
                        break;
                    }
                }
            }));
        }

        let mut summary = SearchSummary {
            total: classes.len(),
            ..Default::default()
        };
        let mut retained = 0;
        let mut last_progress = Instant::now();
        let mut pending = VecDeque::new();
        let mut completed = HashMap::new();
        let mut next = 0;
        'scan: for (index, name) in classes.iter().enumerate() {
            budget.head(index);
            interactive(engine);
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }
            // Admission retains markers only; cached documents load at consumption.
            while next < classes.len() && pending.len() < window {
                interactive(engine);
                if cancel.load(Ordering::Relaxed) {
                    summary.cancelled = true;
                    break 'scan;
                }
                let key = SearchTarget::Class((*classes[next]).clone());
                let prepared = if cache.disk.rejects(
                    &key,
                    &query.query.text,
                    query.query.case_sensitive,
                    query.query.regex,
                    false,
                ) {
                    Prepared::Rejected
                } else if cache.contains(&key) {
                    Prepared::Cached
                } else {
                    if jobs_tx.send(next).is_err() {
                        record_error(&mut summary, "Source workers stopped".into());
                        break 'scan;
                    }
                    Prepared::Rendering
                };
                pending.push_back(prepared);
                next += 1;
            }
            let key = SearchTarget::Class((**name).clone());
            summary.scanned += 1;
            let prepared = pending.pop_front().unwrap();
            if matches!(prepared, Prepared::Rejected) {
                cache.index_rejections += 1;
            } else {
                let cached = match prepared {
                    Prepared::Cached => cache.get(&key).or_else(|| {
                        let value = cache.disk.load(&key);
                        if value.is_some() {
                            cache.disk_hits += 1;
                        }
                        value
                    }),
                    _ => None,
                };
                let value = match cached {
                    Some(value) => value,
                    None => {
                        if matches!(prepared, Prepared::Cached) {
                            // A failed deferred load must not enqueue its head behind
                            // workers holding future results at the byte barrier.
                            // One exceptional worker renders this required head.
                            let mut worker = engine.clone();
                            let results_tx = results_tx.clone();
                            let budget = &budget;
                            let stop = &stop;
                            let started = &started;
                            handles.push(scope.spawn(move || {
                                started.fetch_add(1, Ordering::Relaxed);
                                let result = render_job(&mut worker, name, legacy, || {
                                    stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed)
                                });
                                let bytes = rendered_bytes(&result);
                                if budget.reserve(index, bytes, cancel)
                                    && results_tx.send((index, result, bytes)).is_err()
                                {
                                    budget.release(bytes);
                                }
                            }));
                        }
                        while !completed.contains_key(&index) && !cancel.load(Ordering::Relaxed) {
                            interactive(engine);
                            match results_rx.recv_timeout(Duration::from_millis(20)) {
                                Ok((index, result, bytes)) => {
                                    completed.insert(index, (result, bytes));
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                                Err(mpsc::RecvTimeoutError::Disconnected) => {
                                    completed
                                        .insert(index, (Err("Source workers stopped".into()), 0));
                                }
                            }
                        }
                        if cancel.load(Ordering::Relaxed) {
                            summary.cancelled = true;
                            break;
                        }
                        let (rendered, bytes) = completed.remove(&index).unwrap();
                        budget.release(bytes);
                        if matches!(prepared, Prepared::Cached) {
                            // Complete the exceptional worker before another cache
                            // fallback can start, keeping the extra-worker bound at one.
                            while !handles.last().unwrap().is_finished() {
                                interactive(engine);
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            // Reap OS thread resources immediately; a corrupt cache
                            // may require this path for many consecutive classes.
                            let _ = handles.pop().unwrap().join();
                        }
                        if cancel.load(Ordering::Relaxed) {
                            summary.cancelled = true;
                            break;
                        }
                        match rendered {
                            Ok((value, prepared)) => {
                                cache.insert_prepared(key, value.clone(), prepared);
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
        stop.store(true, Ordering::Relaxed);
        budget.stop();
        drop(results_tx);
        drop(jobs_tx);
        // Dropping the receiver releases any worker blocked on sending a result.
        drop(results_rx);
        // A worker may be finishing resource decoration when cancellation lands.
        // Keep serving navigation during shutdown as well as during rendering.
        while handles.iter().any(|handle| !handle.is_finished()) {
            interactive(engine);
            std::thread::sleep(Duration::from_millis(5));
        }
        summary
    });
    cache.source_fetches += started.load(Ordering::Relaxed);
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_budget_blocks_future_results_but_required_head_bypasses_limit() {
        let budget = ResultBudget::new(10);
        let cancel = AtomicBool::new(false);
        assert!(budget.reserve(1, 10, &cancel));
        assert!(budget.reserve(0, 20, &cancel));
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                tx.send(budget.reserve(2, 1, &cancel)).unwrap();
            });
            assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
            budget.release(20);
            assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
            budget.head(2);
            assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        });
        assert_eq!(budget.state.lock().unwrap().0, 11);
    }

    #[test]
    fn result_budget_releases_capacity_and_shutdown_wakes_waiters() {
        let budget = ResultBudget::new(10);
        let cancel = AtomicBool::new(false);
        assert!(budget.reserve(1, 10, &cancel));
        let (tx, rx) = mpsc::channel();
        let first = tx.clone();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                first.send(budget.reserve(2, 10, &cancel)).unwrap();
            });
            assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
            budget.release(10);
            assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
            scope.spawn(|| {
                tx.send(budget.reserve(3, 10, &cancel)).unwrap();
            });
            assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
            budget.stop();
            assert!(!rx.recv_timeout(Duration::from_secs(1)).unwrap());
        });
    }

    #[test]
    fn result_budget_observes_cancellation_without_coordinator_notification() {
        let budget = ResultBudget::new(1);
        let cancel = AtomicBool::new(false);
        assert!(budget.reserve(1, 1, &cancel));
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                tx.send(budget.reserve(2, 1, &cancel)).unwrap();
            });
            assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
            cancel.store(true, Ordering::Relaxed);
            assert!(!rx.recv_timeout(Duration::from_secs(1)).unwrap());
        });
    }

    #[test]
    fn deferred_corrupt_cache_load_renders_head_and_preserves_hits() {
        use rdx::engine::DecompilerEngine;
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        let mut engine = NativeEngine::start().unwrap();
        let project = engine.open(&path).unwrap();
        let classes: Vec<_> = project.classes.iter().collect();
        let query = CompiledQuery::new(SearchQuery {
            text: "doubleValue".into(),
            excluded_packages: Vec::new(),
            ..Default::default()
        })
        .unwrap();
        let mut cache = SearchCache {
            budget: 0,
            ..Default::default()
        };
        let cancel = AtomicBool::new(false);
        let mut expected = Vec::new();
        let first = run_with_workers(
            &mut engine,
            &mut cache,
            &classes,
            &query,
            &cancel,
            &mut |update| {
                if let SearchUpdate::Batch(batch) = update {
                    expected.extend(
                        batch
                            .into_iter()
                            .map(|hit| (hit.document.name.clone(), hit.start, hit.end, hit.kind)),
                    );
                }
            },
            &mut |_| {},
            4,
        );
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert!(!expected.is_empty());
        assert!(cache.entries.is_empty());
        cache.disk.truncate_payload_for_test();
        let before = cache.source_fetches;
        let mut actual = Vec::new();
        let mut callbacks = 0;
        let second = run_with_workers(
            &mut engine,
            &mut cache,
            &classes,
            &query,
            &cancel,
            &mut |update| {
                if let SearchUpdate::Batch(batch) = update {
                    actual.extend(
                        batch
                            .into_iter()
                            .map(|hit| (hit.document.name.clone(), hit.start, hit.end, hit.kind)),
                    );
                }
            },
            &mut |_| {
                callbacks += 1;
            },
            12,
        );
        assert!(second.errors.is_empty(), "{:?}", second.errors);
        assert_eq!(actual, expected);
        assert_eq!(second.scanned, classes.len());
        assert_eq!(second.skipped, 0);
        assert!(cache.source_fetches > before);
        assert!(callbacks > classes.len());
    }

    #[test]
    fn parallel_render_failures_are_consumed_in_input_order() {
        let names: Vec<String> = (0..513).map(|n| format!("missing.Class{n}")).collect();
        let classes: Vec<_> = names.iter().collect();
        let query = CompiledQuery::new(SearchQuery {
            text: "needle".into(),
            ..Default::default()
        })
        .unwrap();
        for workers in [1, 4, 12, 16] {
            let mut cache = SearchCache::default();
            let summary = run_with_workers(
                &mut NativeEngine::default(),
                &mut cache,
                &classes,
                &query,
                &AtomicBool::new(false),
                &mut |_| {},
                &mut |_| {},
                workers,
            );
            assert_eq!(summary.scanned, names.len());
            assert_eq!(summary.skipped, names.len());
            assert_eq!(cache.source_fetches, names.len());
            assert!(!summary.cancelled);
            for (error, name) in summary.errors.iter().zip(&names) {
                assert!(error.starts_with(&format!("{name}:")), "{error}");
            }
        }
    }

    #[test]
    fn cancellation_with_admitted_jobs_stops_and_joins_workers() {
        for workers in [1, 4, 12, 16] {
            let window = workers * 16;
            let names: Vec<String> = (0..window * 2)
                .map(|n| format!("missing.Class{n}"))
                .collect();
            let classes: Vec<_> = names.iter().collect();
            let query = CompiledQuery::new(SearchQuery {
                text: "needle".into(),
                ..Default::default()
            })
            .unwrap();
            let cancel = AtomicBool::new(false);
            let mut cache = SearchCache::default();
            let mut callbacks = 0;
            let summary = run_with_workers(
                &mut NativeEngine::default(),
                &mut cache,
                &classes,
                &query,
                &cancel,
                &mut |_| {},
                &mut |_| {
                    callbacks += 1;
                    if callbacks == window + 2 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
                workers,
            );
            assert!(summary.cancelled);
            assert!(!summary.limited);
            assert_eq!(summary.scanned, 1);
            assert!(cache.source_fetches <= window);
        }
    }

    #[test]
    fn cancellation_from_interactive_callback_bounds_admitted_window() {
        let names: Vec<String> = (0..100).map(|n| format!("missing.Class{n}")).collect();
        let classes: Vec<_> = names.iter().collect();
        let query = CompiledQuery::new(SearchQuery {
            text: "needle".into(),
            ..Default::default()
        })
        .unwrap();
        for workers in [1, 4, 12, 16] {
            let cancel = AtomicBool::new(false);
            let mut cache = SearchCache::default();
            let mut callbacks = 0;
            let summary = run_with_workers(
                &mut NativeEngine::default(),
                &mut cache,
                &classes,
                &query,
                &cancel,
                &mut |_| {},
                &mut |_| {
                    callbacks += 1;
                    if callbacks == 2 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
                workers,
            );
            assert!(summary.cancelled);
            assert!(!summary.limited);
            assert_eq!(cache.source_fetches, 0);
            assert_eq!(summary.scanned, 0);
        }
    }
}
