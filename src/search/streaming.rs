//! Native in-process search. No worker protocol, source handoff files or JVM.
use super::*;
use std::time::{Duration, Instant};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Condvar, Mutex, mpsc},
};

/// A class outcome, resolved as soon as its source is available.
enum Outcome {
    Rejected,
    Skipped,
    Failed(String),
    Resolved {
        hits: Vec<SearchHit>,
        note: Option<String>,
    },
}

/// Outcomes resolve in completion order and report in class order, so hit
/// order and counters match a serial scan while a slow class never idles the
/// other workers. Documents leave memory at resolution unless they have hits.
/// Reporting stops at the result limit; a class cut off part-way stays
/// unreported so a continuation can report the rest of its matches.
struct Reporter {
    completed: BTreeMap<usize, Outcome>,
    next: usize,
    last_progress: Instant,
    limit: usize,
    /// Matches of the first class that a continuation has already reported.
    skip: usize,
    /// Matches reported from the class at `next` before the limit cut it off.
    partial: Option<usize>,
}
impl Reporter {
    fn new(limit: usize, skip: usize) -> Self {
        Self {
            completed: BTreeMap::new(),
            next: 0,
            last_progress: Instant::now(),
            limit,
            skip,
            partial: None,
        }
    }
    /// Reporting continues while the limit has room.
    fn wants_more(&self, summary: &SearchSummary) -> bool {
        self.partial.is_none() && summary.hits < self.limit
    }
    /// Matches worth collecting from one class: the limit, the ones a
    /// continuation skips in its first class, and one more to tell a class
    /// that ends at the limit from one the limit cuts off.
    fn room(&self, index: usize) -> usize {
        self.limit
            .saturating_add(if index == 0 { self.skip } else { 0 })
            .saturating_add(1)
    }
    fn resolve(
        &self,
        index: usize,
        name: &str,
        rendered: std::result::Result<CachedValue, String>,
        query: &CompiledQuery,
    ) -> Outcome {
        match rendered {
            Err(error) => Outcome::Failed(format!("{name}: {error}")),
            Ok(None) => Outcome::Skipped,
            Ok(Some((document, definitions, note))) => Outcome::Resolved {
                hits: matches(document, &definitions, query, self.room(index)),
                note,
            },
        }
    }
    fn buffer(&mut self, index: usize, outcome: Outcome) {
        self.completed.insert(index, outcome);
    }
    /// Classes with a known outcome, reported or still waiting for an earlier class.
    fn resolved(&self) -> usize {
        self.next + self.completed.len() + usize::from(self.partial.is_some())
    }
    /// Report contiguous outcomes in class order while the limit has room.
    fn drain(
        &mut self,
        summary: &mut SearchSummary,
        cache: &mut SearchCache,
        emit: &mut impl FnMut(SearchUpdate),
    ) {
        while self.wants_more(summary) {
            let Some(outcome) = self.completed.remove(&self.next) else {
                break;
            };
            match outcome {
                Outcome::Rejected => cache.index_rejections += 1,
                Outcome::Skipped => summary.skipped += 1,
                Outcome::Failed(error) => {
                    record_error(summary, error);
                    summary.skipped += 1;
                }
                Outcome::Resolved { mut hits, note } => {
                    let reported = if self.next == 0 {
                        self.skip.min(hits.len())
                    } else {
                        0
                    };
                    hits.drain(..reported);
                    let room = self.limit - summary.hits;
                    if hits.len() > room {
                        hits.truncate(room);
                        summary.hits += room;
                        emit(SearchUpdate::Batch(hits));
                        self.partial = Some(reported + room);
                        return;
                    }
                    if let Some(note) = note {
                        summary.skipped += 1;
                        record_error(summary, note);
                    }
                    if !hits.is_empty() {
                        summary.hits += hits.len();
                        emit(SearchUpdate::Batch(hits));
                    }
                }
            }
            self.next += 1;
            summary.scanned += 1;
        }
    }
    /// The classes a continuation scans once the limit stopped reporting.
    fn remaining(&self, classes: &[&String]) -> Option<SearchResume> {
        (self.next < classes.len()).then(|| SearchResume {
            classes: classes[self.next..]
                .iter()
                .map(|name| (*name).clone())
                .collect(),
            skip: self.partial.unwrap_or(0),
            resources_from: None,
        })
    }
    fn progress(
        &mut self,
        summary: &SearchSummary,
        emit: &mut impl FnMut(SearchUpdate),
        force: bool,
    ) {
        if force || self.last_progress.elapsed() >= Duration::from_millis(50) {
            emit(SearchUpdate::Progress {
                scanned: self.resolved(),
                total: summary.total,
                hits: summary.hits,
                skipped: summary.skipped,
            });
            self.last_progress = Instant::now();
        }
    }
}

// Reservations cover estimated in-progress work, the result channel and the
// coordinator's out-of-order map. The required head can exceed the target to
// guarantee progress; estimates cannot impose an exact allocator limit.
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
    fn finish(&self, index: usize, reserved: usize, actual: usize, cancel: &AtomicBool) -> bool {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.2 || cancel.load(Ordering::Relaxed) {
                state.0 = state.0.saturating_sub(reserved);
                self.wake.notify_all();
                return false;
            }
            let without = state.0.saturating_sub(reserved);
            if index == state.1 || actual <= self.limit.saturating_sub(without) {
                state.0 = without.saturating_add(actual);
                self.wake.notify_all();
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
/// Account for a finished render: free its budget, move the required head and
/// buffer its outcome for reporting in class order.
#[allow(clippy::too_many_arguments)]
fn collect(
    (index, result, bytes): (usize, Rendered, usize),
    classes: &[&String],
    query: &CompiledQuery,
    budget: &ResultBudget,
    in_flight: &mut BTreeSet<usize>,
    next: usize,
    cache: &mut SearchCache,
    reporter: &mut Reporter,
) {
    budget.release(bytes);
    in_flight.remove(&index);
    budget.head(in_flight.first().copied().unwrap_or(next));
    let name = classes[index];
    let outcome = match result {
        Ok((value, prepared)) => {
            cache.insert_prepared(
                SearchTarget::Class((*name).clone()),
                value.clone(),
                prepared,
            );
            reporter.resolve(index, name, Ok(value), query)
        }
        Err(error) => reporter.resolve(index, name, Err(error), query),
    };
    reporter.buffer(index, outcome);
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
    interactive: &mut impl FnMut(&mut NativeEngine, &mut SearchCache),
    workers: usize,
    limit: usize,
    skip: usize,
) -> SearchSummary {
    run_with_workers(
        engine,
        cache,
        classes,
        query,
        cancel,
        emit,
        interactive,
        resolve_workers(workers),
        limit,
        skip,
    )
}

/// Upper bound of the render pool; larger requests are clamped.
pub const MAX_WORKERS: usize = 16;

/// Pool size for a requested worker count. `0` is automatic: one fewer than
/// the machine's cores, at most 12, which leaves a core for the UI. Explicit
/// counts are kept within the pool bound.
pub fn resolve_workers(requested: usize) -> usize {
    if requested == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).clamp(1, 12))
    } else {
        requested.clamp(1, MAX_WORKERS)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_with_workers(
    engine: &mut NativeEngine,
    cache: &mut SearchCache,
    classes: &[&String],
    query: &CompiledQuery,
    cancel: &AtomicBool,
    emit: &mut impl FnMut(SearchUpdate),
    interactive: &mut impl FnMut(&mut NativeEngine, &mut SearchCache),
    workers: usize,
    limit: usize,
    skip: usize,
) -> SearchSummary {
    let workers = workers.clamp(1, MAX_WORKERS);
    // `0` is unlimited; `skip` is what a continuation's first class already reported.
    let limit = if limit == 0 { usize::MAX } else { limit };
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
                    let estimate = worker.search_work_bytes(classes[index]).min(budget.limit);
                    if !budget.reserve(index, estimate, cancel) {
                        break;
                    }
                    started.fetch_add(1, Ordering::Relaxed);
                    let result = render_job(&mut worker, classes[index], legacy, || {
                        stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed)
                    });
                    let bytes = rendered_bytes(&result);
                    if !budget.finish(index, estimate, bytes, cancel) {
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
        let mut reporter = Reporter::new(limit, skip);
        // Renders in flight, in class order; the oldest is the budget's required head.
        let mut in_flight = BTreeSet::new();
        let mut next = 0;
        'scan: loop {
            interactive(engine, cache);
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }
            // Admission resolves indexed classes inline and bounds only renders
            // in flight, so a slow render never starves the workers behind it.
            while next < classes.len() && in_flight.len() < window && reporter.wants_more(&summary)
            {
                interactive(engine, cache);
                if cancel.load(Ordering::Relaxed) {
                    summary.cancelled = true;
                    break 'scan;
                }
                let name = classes[next];
                let key = SearchTarget::Class((*name).clone());
                if cache.disk.rejects(
                    &key,
                    &query.query.text,
                    query.query.case_sensitive,
                    query.query.regex,
                    false,
                ) {
                    reporter.buffer(next, Outcome::Rejected);
                } else if let Some(value) = cache.get(&key).or_else(|| {
                    let value = cache.disk.load(&key);
                    if value.is_some() {
                        cache.disk_hits += 1;
                    }
                    value
                }) {
                    reporter.buffer(next, reporter.resolve(next, name, Ok(value), query));
                } else {
                    if jobs_tx.send(next).is_err() {
                        record_error(&mut summary, "Source workers stopped".into());
                        break 'scan;
                    }
                    in_flight.insert(next);
                    budget.head(*in_flight.first().unwrap());
                }
                next += 1;
                // Finished renders are collected during admission too, so a run
                // of indexed classes never delays reporting an earlier render.
                while let Ok(received) = results_rx.try_recv() {
                    collect(
                        received,
                        classes,
                        query,
                        &budget,
                        &mut in_flight,
                        next,
                        cache,
                        &mut reporter,
                    );
                }
                reporter.drain(&mut summary, cache, emit);
                reporter.progress(&summary, emit, false);
            }
            if !reporter.wants_more(&summary) {
                // The limit is reached; renders still in flight are kept below.
                break;
            }
            if in_flight.is_empty() {
                // Admission only stops with the window open once every class is in.
                break;
            }
            match results_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(received) => {
                    collect(
                        received,
                        classes,
                        query,
                        &budget,
                        &mut in_flight,
                        next,
                        cache,
                        &mut reporter,
                    );
                    reporter.drain(&mut summary, cache, emit);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    for index in std::mem::take(&mut in_flight) {
                        reporter.buffer(
                            index,
                            Outcome::Failed(format!("{}: Source workers stopped", classes[index])),
                        );
                    }
                    reporter.drain(&mut summary, cache, emit);
                    break;
                }
            }
            reporter.progress(&summary, emit, false);
        }
        summary.cancelled |= cancel.load(Ordering::Relaxed);
        if !summary.cancelled && !reporter.wants_more(&summary) {
            summary.resume = reporter.remaining(classes);
            summary.limited = summary.resume.is_some();
        }
        reporter.progress(&summary, emit, true);
        stop.store(true, Ordering::Relaxed);
        budget.stop();
        drop(results_tx);
        drop(jobs_tx);
        // A worker may be finishing resource decoration when cancellation lands.
        // Keep serving navigation during shutdown as well as during rendering.
        while handles.iter().any(|handle| !handle.is_finished()) {
            interactive(engine, cache);
            std::thread::sleep(Duration::from_millis(5));
        }
        // Renders that finished after the scan stopped serve the continuation
        // or the next search. The channel holds a whole window of results, so
        // no worker ever blocks on sending one.
        while let Ok((index, result, bytes)) = results_rx.try_recv() {
            budget.release(bytes);
            if let Ok((value, prepared)) = result {
                cache.insert_prepared(SearchTarget::Class(classes[index].clone()), value, prepared);
            }
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
    fn memory_admission_blocks_work_before_render_and_reconciles_finished_bytes() {
        let budget = ResultBudget::new(10);
        let cancel = AtomicBool::new(false);
        assert!(budget.reserve(0, 8, &cancel));
        std::thread::scope(|scope| {
            let (tx, rx) = mpsc::channel();
            let budget = &budget;
            let cancel = &cancel;
            scope.spawn(move || {
                assert!(budget.reserve(1, 8, cancel));
                tx.send(()).unwrap();
                assert!(budget.finish(1, 8, 2, cancel));
            });
            assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
            assert!(budget.finish(0, 8, 2, cancel));
            rx.recv_timeout(Duration::from_secs(1)).unwrap();
        });
        assert_eq!(budget.state.lock().unwrap().0, 4);
        budget.release(4);
        assert!(budget.reserve(0, 10, &cancel));
        cancel.store(true, Ordering::Relaxed);
        assert!(!budget.finish(0, 10, 20, &cancel));
        assert_eq!(budget.state.lock().unwrap().0, 0);
    }
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
    fn reporter_reports_in_class_order() {
        let query = CompiledQuery::new(SearchQuery {
            text: "needle".into(),
            ..Default::default()
        })
        .unwrap();
        let value = |name: &str| {
            Some((
                Arc::new(SearchDocument {
                    target: SearchTarget::Class(name.into()),
                    name: name.into(),
                    source: "needle needle".into(),
                    syntax: "java".into(),
                    links: vec![],
                    source_hash: None,
                    metadata_complete: true,
                }),
                Arc::new(vec![]),
                None,
            ))
        };
        let mut reporter = Reporter::new(usize::MAX, 0);
        let mut summary = SearchSummary {
            total: 3,
            ..Default::default()
        };
        let mut cache = SearchCache::default();
        let mut batches = Vec::new();
        let mut emit = |update| {
            if let SearchUpdate::Batch(hits) = update {
                batches.push(
                    hits.iter()
                        .map(|hit| hit.origin.name.clone())
                        .collect::<Vec<_>>(),
                );
            }
        };
        reporter.buffer(2, reporter.resolve(2, "C", Ok(value("C")), &query));
        reporter.buffer(1, Outcome::Rejected);
        reporter.drain(&mut summary, &mut cache, &mut emit);
        assert_eq!(summary.scanned, 0);
        assert_eq!(reporter.resolved(), 2);
        reporter.buffer(0, reporter.resolve(0, "A", Err("boom".into()), &query));
        reporter.drain(&mut summary, &mut cache, &mut emit);
        assert_eq!(summary.scanned, 3);
        assert_eq!(summary.skipped, 1);
        assert_eq!(cache.index_rejections, 1);
        assert_eq!(summary.hits, 2);
        assert_eq!(summary.errors, vec!["A: boom".to_string()]);
        assert_eq!(reporter.resolved(), 3);
        assert_eq!(batches, vec![vec!["C".to_string(), "C".to_string()]]);
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
                            .map(|hit| (hit.origin.name.clone(), hit.start, hit.end, hit.kind)),
                    );
                }
            },
            &mut |_, _| {},
            4,
            0,
            0,
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
                            .map(|hit| (hit.origin.name.clone(), hit.start, hit.end, hit.kind)),
                    );
                }
            },
            &mut |_, _| {
                callbacks += 1;
            },
            12,
            0,
            0,
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
                &mut |_, _| {},
                workers,
                0,
                0,
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
                &mut |_, _| {
                    callbacks += 1;
                    if callbacks == window + 2 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
                workers,
                0,
                0,
            );
            assert!(summary.cancelled);
            assert!(summary.scanned <= window);
            assert!(cache.source_fetches <= summary.scanned + window);
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
                &mut |_, _| {
                    callbacks += 1;
                    if callbacks == 2 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
                workers,
                0,
                0,
            );
            assert!(summary.cancelled);
            assert_eq!(cache.source_fetches, 0);
            assert_eq!(summary.scanned, 0);
        }
    }

    #[test]
    fn reporter_stops_at_the_limit_and_a_continuation_reports_the_rest() {
        let query = CompiledQuery::new(SearchQuery {
            text: "needle".into(),
            ..Default::default()
        })
        .unwrap();
        let value = |name: &str| {
            Some((
                Arc::new(SearchDocument {
                    target: SearchTarget::Class(name.into()),
                    name: name.into(),
                    source: "needle needle needle".into(),
                    syntax: "java".into(),
                    links: vec![],
                    source_hash: None,
                    metadata_complete: true,
                }),
                Arc::new(vec![]),
                Some("partial".to_string()),
            ))
        };
        let names: Vec<String> = ["A", "B", "C"].iter().map(|n| n.to_string()).collect();
        let classes: Vec<&String> = names.iter().collect();
        let mut cache = SearchCache::default();
        let batches = std::cell::RefCell::new(Vec::new());
        let mut emit = |update| {
            if let SearchUpdate::Batch(hits) = update {
                batches.borrow_mut().push(
                    hits.iter()
                        .map(|hit| (hit.origin.name.clone(), hit.start))
                        .collect::<Vec<_>>(),
                );
            }
        };
        let batch = |items: &[(&str, usize)]| -> Vec<(String, usize)> {
            items.iter().map(|(n, s)| (n.to_string(), *s)).collect()
        };
        // Four matches fit: all of A and the first of B. B stays unreported.
        let mut summary = SearchSummary {
            total: 3,
            ..Default::default()
        };
        let mut reporter = Reporter::new(4, 0);
        for (index, name) in names.iter().enumerate() {
            reporter.buffer(
                index,
                reporter.resolve(index, name, Ok(value(name)), &query),
            );
        }
        reporter.drain(&mut summary, &mut cache, &mut emit);
        assert_eq!(summary.hits, 4);
        assert_eq!(summary.scanned, 1);
        assert_eq!(
            summary.skipped, 1,
            "only the reported class counts its note"
        );
        assert!(!reporter.wants_more(&summary));
        assert_eq!(reporter.resolved(), 3);
        let resume = reporter.remaining(&classes).unwrap();
        assert_eq!(resume.classes, names[1..]);
        assert_eq!(resume.skip, 1);
        assert_eq!(resume.resources_from, None);
        assert_eq!(
            *batches.borrow(),
            vec![batch(&[("A", 0), ("A", 7), ("A", 14)]), batch(&[("B", 0)])]
        );
        // The continuation skips B's reported match and finishes the rest.
        let remaining: Vec<&String> = resume.classes.iter().collect();
        let mut summary = SearchSummary {
            total: 2,
            ..Default::default()
        };
        let mut reporter = Reporter::new(usize::MAX, resume.skip);
        for (index, name) in resume.classes.iter().enumerate() {
            reporter.buffer(
                index,
                reporter.resolve(index, name, Ok(value(name)), &query),
            );
        }
        reporter.drain(&mut summary, &mut cache, &mut emit);
        assert_eq!(summary.hits, 5);
        assert_eq!(summary.scanned, 2);
        assert_eq!(summary.skipped, 2);
        assert!(reporter.wants_more(&summary));
        assert!(reporter.remaining(&remaining).is_none());
        assert_eq!(
            batches.borrow()[2..],
            [
                batch(&[("B", 7), ("B", 14)]),
                batch(&[("C", 0), ("C", 7), ("C", 14)])
            ]
        );
        // A limit met exactly on a class boundary leaves whole classes.
        let mut summary = SearchSummary {
            total: 3,
            ..Default::default()
        };
        let mut reporter = Reporter::new(3, 0);
        for (index, name) in names.iter().enumerate() {
            reporter.buffer(
                index,
                reporter.resolve(index, name, Ok(value(name)), &query),
            );
        }
        reporter.drain(&mut summary, &mut cache, &mut |_| {});
        assert_eq!((summary.hits, summary.scanned), (3, 1));
        let resume = reporter.remaining(&classes).unwrap();
        assert_eq!((resume.classes.as_slice(), resume.skip), (&names[1..], 0));
    }

    #[test]
    fn limited_scan_stops_and_continuations_complete_the_same_results() {
        use rdx::engine::DecompilerEngine;
        fn scan(
            engine: &mut NativeEngine,
            cache: &mut SearchCache,
            classes: &[&String],
            query: &CompiledQuery,
            limit: usize,
            skip: usize,
            hits: &mut Vec<(String, usize, usize)>,
        ) -> SearchSummary {
            run_with_workers(
                engine,
                cache,
                classes,
                query,
                &AtomicBool::new(false),
                &mut |update| {
                    if let SearchUpdate::Batch(batch) = update {
                        hits.extend(
                            batch
                                .iter()
                                .map(|hit| (hit.origin.name.clone(), hit.start, hit.end)),
                        );
                    }
                },
                &mut |_, _| {},
                4,
                limit,
                skip,
            )
        }
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
        let mut expected = Vec::new();
        let full = scan(
            &mut engine,
            &mut SearchCache::default(),
            &classes,
            &query,
            0,
            0,
            &mut expected,
        );
        assert!(!full.limited && full.resume.is_none(), "{:?}", full.errors);
        assert!(expected.len() > 2, "{}", expected.len());
        for limit in [1, 2, expected.len() - 1] {
            let mut cache = SearchCache::default();
            let mut actual = Vec::new();
            let mut summary = scan(
                &mut engine,
                &mut cache,
                &classes,
                &query,
                limit,
                0,
                &mut actual,
            );
            assert!(summary.limited, "limit {limit}");
            assert_eq!(summary.hits, limit);
            assert_eq!(actual, expected[..limit], "limit {limit}");
            let mut scanned = summary.scanned;
            let mut runs = 0;
            while let Some(resume) = summary.resume.take() {
                assert!(summary.limited);
                let remaining: Vec<&String> = resume.classes.iter().collect();
                summary = scan(
                    &mut engine,
                    &mut cache,
                    &remaining,
                    &query,
                    limit,
                    resume.skip,
                    &mut actual,
                );
                scanned += summary.scanned;
                runs += 1;
                assert!(runs <= expected.len() + 1, "limit {limit}: no progress");
            }
            assert!(!summary.limited);
            assert_eq!(actual, expected, "limit {limit}");
            assert_eq!(scanned, classes.len(), "limit {limit}");
        }
    }

    #[test]
    fn requested_workers_resolve_to_auto_or_a_bounded_count() {
        let auto = resolve_workers(0);
        assert!((1..=12).contains(&auto));
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(auto, cores.saturating_sub(1).clamp(1, 12));
        assert_eq!(resolve_workers(1), 1);
        assert_eq!(resolve_workers(3), 3);
        assert_eq!(resolve_workers(MAX_WORKERS + 50), MAX_WORKERS);
    }
}
