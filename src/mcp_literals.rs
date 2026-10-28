//! Bounded project-local literal references. Built once without Java rendering.
use super::{NativeEngine, Project, required};
use crate::{
    native_dex::{DexAnnotationSet, DexClass, DexSymbols, DexValue},
    native_engine::disassembly,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

type Scanner = dyn Fn(&Value, &AtomicBool) -> Result<Value> + Send + Sync;
type Builder = dyn Fn(&AtomicBool, &AtomicUsize) -> Result<Index> + Send + Sync;
struct HeldIndex {
    index: Index,
    _reservation: Arc<crate::retained_budget::Reservation>,
}
impl std::ops::Deref for HeldIndex {
    type Target = Index;
    fn deref(&self) -> &Index {
        &self.index
    }
}
struct BuildState {
    phase: &'static str,
    attempt: u64,
    waiters: usize,
    progress: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    error: String,
    index: Option<Arc<HeldIndex>>,
}
#[derive(Clone)]
pub(super) struct Controller {
    scanner: Option<Arc<Scanner>>,
    state: Arc<Mutex<BuildState>>,
    builder: Arc<Mutex<Arc<Builder>>>,
    total: usize,
    budget: Arc<crate::retained_budget::RetainedBudget>,
    stop: Arc<AtomicBool>,
}
impl Controller {
    pub(super) fn new(
        engine: Arc<NativeEngine>,
        project: Arc<Project>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        let budget = engine.retained_budget.clone();
        let total = project.classes.len();
        let scan_engine = engine.clone();
        let scan_project = project.clone();
        let mut controller = Self::with_builder(
            total,
            stop,
            Arc::new(move |cancel, progress| Index::build(&engine, &project, cancel, progress)),
        );
        controller.budget = budget;
        controller.scanner = Some(Arc::new(move |args, cancel| {
            super::scan::literals(&scan_engine, &scan_project, args, cancel)
        }));
        controller
    }
    fn with_builder(total: usize, stop: Arc<AtomicBool>, builder: Arc<Builder>) -> Self {
        Self {
            scanner: None,
            budget: Arc::default(),
            state: Arc::new(Mutex::new(BuildState {
                phase: "idle",
                attempt: 0,
                waiters: 0,
                progress: Arc::new(AtomicUsize::new(0)),
                cancel: Arc::new(AtomicBool::new(false)),
                started: Instant::now(),
                error: String::new(),
                index: None,
            })),
            builder: Arc::new(Mutex::new(builder)),
            total,
            stop,
        }
    }
    pub(super) fn cancel(&self) {
        self.state
            .lock()
            .unwrap()
            .cancel
            .store(true, Ordering::Relaxed);
    }
    pub(super) fn invalidate(&self) {
        let mut state = self.state.lock().unwrap();
        state.cancel.store(true, Ordering::Relaxed);
        state.attempt += 1;
        state.phase = "invalidated";
        state.waiters = 0;
        state.index = None;
        state.error = "PROJECT_CHANGED: APK changed on disk; restart the server".into();
    }
    pub(super) fn status(&self) -> Value {
        let state = self.state.lock().unwrap();
        let completed = state.progress.load(Ordering::Relaxed);
        json!({"state":if state.phase == "indexing" && state.cancel.load(Ordering::Relaxed) {"cancelling"} else {state.phase},
            "completed_classes":completed,"total_classes":self.total,
            "progress":if self.total == 0 {0.0} else {completed as f64 / self.total as f64},
            "attempt":state.attempt,"active_waiters":state.waiters,"error":if state.phase=="streaming" {""} else {&state.error},
            "fallback_reason":if state.phase=="streaming" {Some(state.error.as_str())} else {None},
            "retryable":(["failed","cancelled"].contains(&state.phase)),"index_complete":state.index.is_some()})
    }
    pub(super) fn search(&self, args: &Value, cancel: &AtomicBool) -> Result<Value> {
        let mut waiter = None;
        loop {
            ensure!(
                !cancel.load(Ordering::Relaxed) && !self.stop.load(Ordering::Relaxed),
                "CANCELLED"
            );
            let mut state = self.state.lock().unwrap();
            if state.phase == "streaming" {
                drop(state);
                return self
                    .scanner
                    .as_ref()
                    .context("Streaming scanner unavailable")?(args, cancel);
            }
            if state.phase == "failed"
                && state.error.contains("LITERAL_INDEX_LIMIT")
                && self.scanner.is_some()
            {
                state.phase = "streaming";
                continue;
            }
            if state.phase == "invalidated" {
                anyhow::bail!("{}", state.error);
            }
            if waiter
                .as_ref()
                .is_some_and(|waiter: &Waiter| waiter.attempt != state.attempt)
            {
                anyhow::bail!("INDEX_RETRY: the previous indexing attempt ended; retry the query");
            }
            if let Some(index) = &state.index {
                let index = index.clone();
                drop(state);
                return index.search(args, cancel);
            }
            if state.phase == "indexing" {
                if state.started.elapsed() >= Duration::from_secs(120) {
                    state.cancel.store(true, Ordering::Relaxed);
                    state.error =
                        "INDEX_TIMEOUT: literal indexing exceeded 120 seconds; retry the query"
                            .into();
                }
                ensure!(
                    !state.cancel.load(Ordering::Relaxed),
                    "{}",
                    if state.error.is_empty() {
                        "INDEX_CANCELLED: literal indexing is stopping; retry the query"
                    } else {
                        &state.error
                    }
                );
                if waiter.is_none() {
                    state.waiters += 1;
                    waiter = Some(Waiter {
                        state: self.state.clone(),
                        attempt: state.attempt,
                    });
                }
            } else {
                if waiter.is_some() {
                    anyhow::bail!("{}", state.error);
                }
                state.attempt += 1;
                let attempt = state.attempt;
                state.phase = "indexing";
                state.waiters = 1;
                state.progress = Arc::new(AtomicUsize::new(0));
                state.cancel = Arc::new(AtomicBool::new(false));
                state.started = Instant::now();
                state.error.clear();
                let (progress, job_cancel) = (state.progress.clone(), state.cancel.clone());
                waiter = Some(Waiter {
                    state: self.state.clone(),
                    attempt,
                });
                let (shared, builder, stop) = (
                    self.state.clone(),
                    self.builder.lock().unwrap().clone(),
                    self.stop.clone(),
                );
                let budget = self.budget.clone();
                std::thread::spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        builder(&job_cancel, &progress)
                    }))
                    .unwrap_or_else(|_| {
                        Err(anyhow::anyhow!(
                            "INDEX_BUILD_FAILED: literal index worker panicked; retry the query"
                        ))
                    });
                    let mut state = shared.lock().unwrap();
                    if state.attempt != attempt {
                        return;
                    }
                    if job_cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                        state.phase = "cancelled";
                        if state.error.is_empty() {
                            state.error =
                                "INDEX_CANCELLED: literal indexing cancelled; retry the query"
                                    .into();
                        }
                    } else {
                        match result {
                            Ok(index) => {
                                if let Some(reservation) = budget.reserve(index.estimated_bytes) {
                                    state.index = Some(Arc::new(HeldIndex {
                                        index,
                                        _reservation: reservation,
                                    }));
                                    state.phase = "ready";
                                } else {
                                    state.phase = "failed";
                                    state.error = "LITERAL_INDEX_LIMIT: shared project retention budget reached".into();
                                }
                            }
                            Err(error) => {
                                state.phase = "failed";
                                state.error =
                                    format!("INDEX_BUILD_FAILED: {error:#}; retry the query");
                            }
                        }
                    }
                });
            }
            drop(state);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[cfg(test)]
    pub(super) fn gate_for_test(&self, entered: Arc<AtomicBool>, release: Arc<AtomicBool>) {
        let original = self.builder.lock().unwrap().clone();
        *self.builder.lock().unwrap() = Arc::new(move |cancel, progress| {
            progress.store(1, Ordering::Relaxed);
            entered.store(true, Ordering::Relaxed);
            while !release.load(Ordering::Relaxed) {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                std::thread::sleep(Duration::from_millis(1));
            }
            original(cancel, progress)
        });
    }
}
struct Waiter {
    state: Arc<Mutex<BuildState>>,
    attempt: u64,
}
impl Drop for Waiter {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        if state.attempt == self.attempt {
            state.waiters = state.waiters.saturating_sub(1);
            if state.waiters == 0 && state.phase == "indexing" {
                state.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}

const MAX_INDEX_BYTES: usize = 128 * 1024 * 1024;
struct Owner {
    class_id: Arc<str>,
    method_id: Option<String>,
    field_id: Option<String>,
    kind: &'static str,
    parameter_index: Option<usize>,
}
#[derive(Clone, Copy)]
struct Occurrence {
    owner: u32,
    code_unit_offset: Option<u32>,
}
pub(super) struct Index {
    symbols: Vec<Arc<DexSymbols>>,
    anchors: Vec<String>,
    owners: Vec<Owner>,
    postings: BTreeMap<(usize, u32), Vec<Occurrence>>,
    estimated_bytes: usize,
    max_bytes: usize,
    classes: usize,
}
impl Index {
    pub(super) fn build(
        engine: &NativeEngine,
        project: &Project,
        cancel: &AtomicBool,
        progress: &AtomicUsize,
    ) -> Result<Self> {
        let mut index = Self {
            symbols: Vec::new(),
            anchors: Vec::new(),
            owners: Vec::new(),
            postings: BTreeMap::new(),
            estimated_bytes: 0,
            max_bytes: MAX_INDEX_BYTES,
            classes: 0,
        };
        if let Some(value) = std::env::var_os("RDX_LITERAL_INDEX_MIB") {
            let mib = value
                .to_str()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|n| (1..=4096).contains(n))
                .context("RDX_LITERAL_INDEX_MIB must be an integer from 1 to 4096")?;
            index.max_bytes = mib * 1024 * 1024;
        }
        let mut dexes = HashMap::new();
        for name in &project.classes {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let class = engine
                .dex_class(name)
                .context("Class missing while indexing literals")?;
            let next = dexes.len();
            let dex = *dexes
                .entry(Arc::as_ptr(&class.symbols) as usize)
                .or_insert(next);
            if dex == index.symbols.len() {
                index.charge(name.len() + 128)?;
                index.symbols.push(class.symbols.clone());
                index.anchors.push(name.clone());
            }
            index.add_class(name, class, dex, cancel)?;
            index.classes += 1;
            progress.store(index.classes, Ordering::Relaxed);
        }
        Ok(index)
    }
    fn charge(&mut self, n: usize) -> Result<()> {
        self.estimated_bytes = self
            .estimated_bytes
            .checked_add(n)
            .context("Literal index size overflow")?;
        ensure!(
            self.estimated_bytes <= self.max_bytes,
            "LITERAL_INDEX_LIMIT: index exceeds {} MiB budget; switching to bounded complete streaming search. No partial index was published.",
            self.max_bytes / (1024 * 1024)
        );
        Ok(())
    }
    fn add(&mut self, dex: usize, string: u32, owner: u32, pc: Option<u32>) -> Result<()> {
        ensure!(
            self.symbols[dex].strings.get(string as usize).is_some(),
            "Invalid literal string index"
        );
        // A tree entry is shared by every occurrence of a literal in a DEX.
        // Account for its node once, then budget the actual posting capacity.
        let key = (dex, string);
        if !self.postings.contains_key(&key) {
            self.charge(128)?;
        }
        let (len, capacity) = self
            .postings
            .get(&key)
            .map_or((0, 0), |hits| (hits.len(), hits.capacity()));
        if len == capacity {
            let next = capacity
                .checked_mul(2)
                .context("Posting capacity overflow")?
                .max(4);
            self.charge((next - capacity) * std::mem::size_of::<Occurrence>())?;
            self.postings
                .entry(key)
                .or_default()
                .try_reserve_exact(next - len)?;
            let allocated = self.postings[&key].capacity();
            self.charge((allocated - next) * std::mem::size_of::<Occurrence>())?;
        }
        self.postings.entry(key).or_default().push(Occurrence {
            owner,
            code_unit_offset: pc,
        });
        Ok(())
    }
    fn owner(&mut self, owner: Owner) -> Result<u32> {
        self.charge(
            256 + owner.class_id.len()
                + owner.method_id.as_ref().map_or(0, String::capacity)
                + owner.field_id.as_ref().map_or(0, String::capacity),
        )?;
        let id = u32::try_from(self.owners.len())?;
        self.owners.push(owner);
        Ok(id)
    }
    fn encoded(
        &mut self,
        dex: usize,
        value: &DexValue,
        owner: &mut (Option<Owner>, Option<u32>),
        cancel: &AtomicBool,
    ) -> Result<()> {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        match value {
            DexValue::String(i) => {
                let id = if let Some(id) = owner.1 {
                    id
                } else {
                    let id = self.owner(owner.0.take().context("Missing literal owner")?)?;
                    owner.1 = Some(id);
                    id
                };
                self.add(dex, *i, id, None)?;
            }
            DexValue::Array(values) => {
                for v in values {
                    self.encoded(dex, v, owner, cancel)?;
                }
            }
            DexValue::Annotation { elements, .. } => {
                for (_, v) in elements {
                    self.encoded(dex, v, owner, cancel)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn annotations(
        &mut self,
        dex: usize,
        set: Option<&DexAnnotationSet>,
        owner: Owner,
        cancel: &AtomicBool,
    ) -> Result<()> {
        if let Some(set) = set {
            let mut pending = (Some(owner), None);
            for annotation in set.iter() {
                for (_, value) in &annotation.elements {
                    self.encoded(dex, value, &mut pending, cancel)?;
                }
            }
        }
        Ok(())
    }
    fn add_class(
        &mut self,
        name: &str,
        class: &DexClass,
        dex: usize,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let name: Arc<str> = name.into();
        let owner = |method_id, field_id, kind, parameter_index| Owner {
            class_id: name.clone(),
            method_id,
            field_id,
            kind,
            parameter_index,
        };
        let directory = class.symbols.annotations.get(&class.annotations_offset);
        self.annotations(
            dex,
            directory.and_then(|d| d.class.as_ref()),
            owner(None, None, "class_annotation", None),
            cancel,
        )?;
        let mut static_index = 0;
        for (i, field) in class.fields.iter().enumerate() {
            let field_id = format!("{}.{}:{}", name, field.name, field.field_type);
            if field.is_static {
                if let Some(value) = class.static_values.get(static_index) {
                    let mut pending = (
                        Some(owner(None, Some(field_id.clone()), "static_value", None)),
                        None,
                    );
                    self.encoded(dex, value, &mut pending, cancel)?;
                }
                static_index += 1;
            }
            self.annotations(
                dex,
                directory
                    .and_then(|d| d.fields.get(i))
                    .and_then(Option::as_ref),
                owner(None, Some(field_id), "field_annotation", None),
                cancel,
            )?;
        }
        for (i, method) in class.methods.iter().enumerate() {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let method_id = disassembly::method_id(method);
            if let Some(code) = &method.code {
                let mut owner_id = None;
                let mut pc = 0;
                while pc < code.instructions.len() {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    let words = &code.instructions;
                    let width = disassembly::width(words, pc)
                        .context("Malformed instruction while indexing literals")?;
                    ensure!(
                        pc + width <= words.len(),
                        "Truncated instruction while indexing literals"
                    );
                    let string = match words[pc] as u8 {
                        0x1a => Some(u32::from(words[pc + 1])),
                        0x1b => Some(u32::from(words[pc + 1]) | (u32::from(words[pc + 2]) << 16)),
                        _ => None,
                    };
                    if let Some(string) = string {
                        let id = match owner_id {
                            Some(id) => id,
                            None => {
                                let id = self.owner(owner(
                                    Some(method_id.clone()),
                                    None,
                                    "instruction",
                                    None,
                                ))?;
                                owner_id = Some(id);
                                id
                            }
                        };
                        self.add(dex, string, id, Some(pc as u32))?;
                    }
                    pc += width;
                }
            }
            self.annotations(
                dex,
                directory
                    .and_then(|d| d.methods.get(i))
                    .and_then(Option::as_ref),
                owner(Some(method_id.clone()), None, "method_annotation", None),
                cancel,
            )?;
            if let Some(parameters) = directory.and_then(|d| d.parameters.get(i)) {
                for (parameter, set) in parameters.iter().enumerate() {
                    self.annotations(
                        dex,
                        set.as_ref(),
                        owner(
                            Some(method_id.clone()),
                            None,
                            "parameter_annotation",
                            Some(parameter),
                        ),
                        cancel,
                    )?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn search(&self, args: &Value, cancel: &AtomicBool) -> Result<Value> {
        let query = required(args, "query")?;
        ensure!(query.len() <= 4096, "Query exceeds 4096 bytes");
        let exact = args["match_mode"] == "exact";
        let prefix = args["package_prefix"]
            .as_str()
            .unwrap_or("")
            .trim_end_matches('.');
        let start = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().unwrap_or(100) as usize;
        ensure!((1..=500).contains(&limit), "Invalid limit");
        let mut total = 0;
        let mut items = Vec::new();
        let mut response_bytes = 0;
        let mut page_full = false;
        for (&(dex, string), hits) in &self.postings {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let value = &self.symbols[dex].strings[string as usize];
            if !(if exact {
                value == query
            } else {
                value.contains(query)
            }) {
                continue;
            }
            for hit in hits {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                let owner = &self.owners[hit.owner as usize];
                if !prefix.is_empty()
                    && owner.class_id.as_ref() != prefix
                    && !owner
                        .class_id
                        .strip_prefix(prefix)
                        .is_some_and(|s| s.starts_with('.'))
                {
                    continue;
                }
                if total >= start && items.len() < limit && !page_full {
                    let mut item = json!({"class_id":owner.class_id.as_ref(),"method_id":owner.method_id,"field_id":owner.field_id,"kind":owner.kind,"parameter_index":owner.parameter_index});
                    item["string_index"] = json!(string);
                    item["dex_class_id"] = json!(self.anchors[dex]);
                    item["value"] = json!(value.chars().take(4096).collect::<String>());
                    item["value_chars"] = json!(value.chars().count());
                    item["value_truncated"] = json!(value.chars().count() > 4096);
                    item["code_unit_offset"] = json!(hit.code_unit_offset);
                    let bytes = serde_json::to_vec(&item)?.len();
                    if response_bytes + bytes > 1024 * 1024 {
                        ensure!(
                            !items.is_empty(),
                            "LITERAL_RESULT_TOO_LARGE: one owner exceeds response budget"
                        );
                        page_full = true;
                    } else {
                        response_bytes += bytes;
                        items.push(item);
                    }
                }
                total += 1;
            }
        }
        ensure!(start <= total, "Invalid offset");
        let end = start.saturating_add(items.len());
        Ok(
            json!({"items":items,"offset":start,"total":total,"next_offset":(end<total).then_some(end),
            "index_complete":true,"page_byte_budget":1048576,"indexed_classes":self.classes,"indexed_dexes":self.symbols.len(),
            "estimated_index_bytes":self.estimated_bytes,
            "scope":"DEX const-string instructions and encoded static/annotation string values; no dynamic string reconstruction or unreferenced string-pool entries"}),
        )
    }
}

#[cfg(test)]
mod controller_tests {
    use super::*;
    #[test]
    fn memory_limit_switches_to_complete_scans_without_rebuilding() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let calls = attempts.clone();
        let mut controller = Controller::with_builder(
            2,
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |_, _| {
                calls.fetch_add(1, Ordering::Relaxed);
                anyhow::bail!("LITERAL_INDEX_LIMIT: test budget exceeded")
            }),
        );
        controller.scanner = Some(Arc::new(|args, cancel| {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            Ok(json!({"complete":true,"scan_complete":true,"query":args["query"]}))
        }));
        for query in ["first", "second"] {
            let result = controller
                .search(&json!({"query":query}), &AtomicBool::new(false))
                .unwrap();
            assert_eq!(result["scan_complete"], true);
            assert_eq!(result["query"], query);
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert_eq!(controller.status()["state"], "streaming");
        assert_eq!(controller.status()["error"], "");
        assert!(
            controller
                .search(&json!({"query":"third"}), &AtomicBool::new(true))
                .is_err()
        );
        controller.invalidate();
        assert!(
            controller
                .search(&json!({"query":"fourth"}), &AtomicBool::new(false))
                .is_err()
        );
    }
    fn empty() -> Index {
        Index {
            symbols: vec![],
            anchors: vec![],
            owners: vec![],
            postings: BTreeMap::new(),
            estimated_bytes: 0,
            max_bytes: MAX_INDEX_BYTES,
            classes: 0,
        }
    }
    fn until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn spawn(
        controller: Controller,
        cancel: Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<Result<Value>> {
        std::thread::spawn(move || controller.search(&json!({"query":"needle"}), &cancel))
    }

    #[test]
    fn concurrent_waiters_share_one_build_and_independent_cancellation() {
        let release = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        let (gate, calls) = (release.clone(), attempts.clone());
        let controller = Controller::with_builder(
            2,
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |cancel, progress| {
                calls.fetch_add(1, Ordering::Relaxed);
                progress.store(1, Ordering::Relaxed);
                while !gate.load(Ordering::Relaxed) {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(empty())
            }),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let first = spawn(controller.clone(), cancel.clone());
        let second = spawn(controller.clone(), Arc::new(AtomicBool::new(false)));
        until(|| controller.status()["active_waiters"] == 2);
        assert_eq!(controller.status()["completed_classes"], 1);
        cancel.store(true, Ordering::Relaxed);
        assert!(first.join().unwrap().is_err());
        assert_eq!(controller.status()["active_waiters"], 1);
        release.store(true, Ordering::Relaxed);
        assert_eq!(second.join().unwrap().unwrap()["index_complete"], true);
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn failed_build_can_retry_while_prior_waiters_finish() {
        let first_release = Arc::new(AtomicBool::new(false));
        let retry_release = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        let (first_gate, retry_gate, calls) = (
            first_release.clone(),
            retry_release.clone(),
            attempts.clone(),
        );
        let controller = Controller::with_builder(
            1,
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |cancel, _| {
                let attempt = calls.fetch_add(1, Ordering::Relaxed);
                let gate = if attempt == 0 {
                    &first_gate
                } else {
                    &retry_gate
                };
                while !gate.load(Ordering::Relaxed) {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    std::thread::sleep(Duration::from_millis(1));
                }
                if attempt == 0 {
                    anyhow::bail!("synthetic recoverable failure");
                }
                Ok(empty())
            }),
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let previous = controller.clone();
        let first = std::thread::spawn(move || {
            tx.send(previous.search(&json!({"query":"needle"}), &AtomicBool::new(false)))
                .unwrap();
        });
        until(|| attempts.load(Ordering::Relaxed) == 1);
        first_release.store(true, Ordering::Relaxed);
        until(|| controller.status()["state"] == "failed");
        let retry = spawn(controller.clone(), Arc::new(AtomicBool::new(false)));
        until(|| attempts.load(Ordering::Relaxed) == 2);
        let old_result = rx.recv_timeout(Duration::from_millis(500));
        retry_release.store(true, Ordering::Relaxed);
        assert!(retry.join().unwrap().is_ok());
        first.join().unwrap();
        assert!(
            old_result.unwrap().is_err(),
            "a prior waiter followed the replacement attempt"
        );
        assert_eq!(controller.status()["attempt"], 2);
        assert_eq!(controller.status()["active_waiters"], 0);
    }

    #[test]
    fn last_waiter_cancels_worker_and_retry_publishes_only_complete_index() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let calls = attempts.clone();
        let controller = Controller::with_builder(
            1,
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |cancel, _| {
                if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    while !cancel.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    anyhow::bail!("CANCELLED");
                }
                Ok(empty())
            }),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let first = spawn(controller.clone(), cancel.clone());
        until(|| attempts.load(Ordering::Relaxed) == 1);
        cancel.store(true, Ordering::Relaxed);
        assert!(first.join().unwrap().is_err());
        until(|| controller.status()["state"] == "cancelled");
        assert_eq!(controller.status()["index_complete"], false);
        assert!(
            controller
                .search(&json!({"query":"needle"}), &AtomicBool::new(false))
                .is_ok()
        );
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn deadline_cancels_build_and_project_invalidation_prevents_stale_publication() {
        let stop = Arc::new(AtomicBool::new(false));
        let controller = Controller::with_builder(
            1,
            stop.clone(),
            Arc::new(move |cancel, _| {
                while !cancel.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(empty()) // even a late successful result cannot publish after cancellation
            }),
        );
        let query = spawn(controller.clone(), Arc::new(AtomicBool::new(false)));
        until(|| controller.status()["state"] == "indexing");
        controller.state.lock().unwrap().started = Instant::now() - Duration::from_secs(121);
        assert!(
            query
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("INDEX_TIMEOUT")
        );
        until(|| controller.status()["state"] == "cancelled");
        assert_eq!(controller.status()["index_complete"], false);
        let query = spawn(controller.clone(), Arc::new(AtomicBool::new(false)));
        until(|| controller.status()["state"] == "indexing");
        controller.invalidate();
        assert!(
            query
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("PROJECT_CHANGED")
        );
        assert_eq!(controller.status()["state"], "invalidated");
        assert_eq!(controller.status()["index_complete"], false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexAnnotation, DexAnnotationDirectory, DexCode, DexField, DexMethod};
    fn fixture(label: &str, literal: &str) -> DexClass {
        let annotation: DexAnnotationSet = Arc::from(vec![Arc::new(DexAnnotation {
            visibility: 1,
            type_idx: 0,
            elements: vec![(
                0,
                DexValue::Array(vec![DexValue::Annotation {
                    type_idx: 0,
                    elements: vec![(0, DexValue::String(0))],
                }]),
            )],
        })]);
        let symbols = Arc::new(DexSymbols {
            strings: vec![literal.into(), "unused".into()],
            annotations: BTreeMap::from([(
                1,
                Arc::new(DexAnnotationDirectory {
                    class: Some(annotation.clone()),
                    fields: vec![Some(annotation.clone())],
                    methods: vec![Some(annotation.clone())],
                    parameters: vec![vec![Some(annotation)]],
                }),
            )]),
            ..Default::default()
        });
        DexClass {
            symbols,
            descriptor: format!("L{label};").into(),
            superclass: None,
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 1,
            static_values_offset: 1,
            static_values: vec![DexValue::String(0)],
            fields: vec![DexField {
                declaring_type: format!("L{label};").into(),
                name: "field".into(),
                field_type: "Ljava/lang/String;".into(),
                access_flags: 9,
                is_static: true,
            }],
            methods: vec![DexMethod {
                declaring_type: format!("L{label};").into(),
                name: "read".into(),
                return_type: "V".into(),
                parameters: vec![],
                thrown_types: vec![],
                access_flags: 9,
                code: Some(DexCode {
                    registers: 1,
                    ins: 0,
                    outs: 0,
                    tries: 0,
                    try_regions: vec![],
                    offset: 1,
                    // String, jumbo string, return, then array payload containing fake const-string.
                    instructions: vec![0x001a, 0, 0x001b, 0, 0, 0x000e, 0x0300, 2, 2, 0, 0x001a, 1],
                }),
            }],
        }
    }
    fn index(classes: &[DexClass]) -> Index {
        let mut index = Index {
            symbols: vec![],
            anchors: vec![],
            owners: vec![],
            postings: BTreeMap::new(),
            estimated_bytes: 0,
            max_bytes: MAX_INDEX_BYTES,
            classes: classes.len(),
        };
        for (dex, class) in classes.iter().enumerate() {
            let name = disassembly::type_name(&class.descriptor);
            index.symbols.push(class.symbols.clone());
            index.anchors.push(name.clone());
            index
                .add_class(&name, class, dex, &AtomicBool::new(false))
                .unwrap();
        }
        index
    }
    #[test]
    fn memory_shared_cache_pressure_preserves_literal_results_via_streaming() {
        let mut controller = Controller::with_builder(
            1,
            Arc::new(AtomicBool::new(false)),
            Arc::new(|_, _| Ok(index(&[fixture("sample/One", "needle")]))),
        );
        controller.budget = Arc::new(crate::retained_budget::RetainedBudget::new(0));
        controller.scanner = Some(Arc::new(|args, cancel| {
            index(&[fixture("sample/One", "needle")]).search(args, cancel)
        }));
        let args = json!({"query":"needle"});
        let cancel = AtomicBool::new(false);
        let expected = index(&[fixture("sample/One", "needle")])
            .search(&args, &cancel)
            .unwrap();
        let actual = controller.search(&args, &cancel).unwrap();
        assert_eq!(actual["items"], expected["items"]);
        assert_eq!(actual["total"], expected["total"]);
        assert_eq!(controller.status()["state"], "streaming");
    }
    #[test]
    fn owners_include_instructions_static_and_nested_annotation_values() {
        let idx = index(&[fixture("sample/One", "needle")]);
        let page = idx
            .search(
                &json!({"query":"needle","match_mode":"exact"}),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(page["total"], 7);
        let items = page["items"].as_array().unwrap();
        let kinds: std::collections::BTreeSet<_> =
            items.iter().map(|v| v["kind"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            [
                "class_annotation",
                "field_annotation",
                "method_annotation",
                "parameter_annotation",
                "static_value",
                "instruction"
            ]
            .into_iter()
            .collect()
        );
        let offsets: Vec<_> = items
            .iter()
            .filter_map(|v| v["code_unit_offset"].as_u64())
            .collect();
        assert_eq!(offsets, vec![0, 2]);
        assert!(
            items
                .iter()
                .filter(|v| v["kind"] == "instruction")
                .all(|v| v["method_id"] == "sample.One.read()V")
        );
        assert_eq!(
            idx.search(&json!({"query":"unused"}), &AtomicBool::new(false))
                .unwrap()["total"],
            0
        );
    }
    #[test]
    fn multidex_indexes_and_occurrence_pages_do_not_cross_owners() {
        let idx = index(&[
            fixture("sample/One", "needle"),
            fixture("other/Two", "second needle"),
        ]);
        let all = idx
            .search(&json!({"query":"needle"}), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(all["total"], 14);
        let mut found = vec![];
        for offset in (0..14).step_by(3) {
            let page = idx
                .search(
                    &json!({"query":"needle","offset":offset,"limit":3}),
                    &AtomicBool::new(false),
                )
                .unwrap();
            found.extend(page["items"].as_array().unwrap().iter().cloned());
        }
        assert_eq!(found, all["items"].as_array().unwrap().clone());
        assert!(found.iter().all(
            |v| (v["class_id"] == "sample.One" && v["value"] == "needle")
                || (v["class_id"] == "other.Two" && v["value"] == "second needle")
        ));
        assert_eq!(
            idx.search(
                &json!({"query":"needle","match_mode":"exact"}),
                &AtomicBool::new(false)
            )
            .unwrap()["total"],
            7
        );
        assert_eq!(
            idx.search(
                &json!({"query":"needle","package_prefix":"sam"}),
                &AtomicBool::new(false)
            )
            .unwrap()["total"],
            0
        );
        assert_eq!(
            idx.search(
                &json!({"query":"needle","package_prefix":"sample"}),
                &AtomicBool::new(false)
            )
            .unwrap()["total"],
            7
        );
        assert!(
            idx.search(
                &json!({"query":"needle","offset":15}),
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }
    #[test]
    fn budget_cancellation_and_truncation_are_explicit() {
        let mut idx = index(&[fixture("sample/One", &"λ".repeat(5000))]);
        let page = idx
            .search(&json!({"query":"λ","limit":1}), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(page["items"][0]["value_chars"], 5000);
        assert_eq!(page["items"][0]["value_truncated"], true);
        assert_eq!(
            page["items"][0]["value"].as_str().unwrap().chars().count(),
            4096
        );
        assert!(
            idx.search(&json!({"query":"λ"}), &AtomicBool::new(true))
                .is_err()
        );
        idx.estimated_bytes = MAX_INDEX_BYTES;
        assert!(
            idx.add(0, 1, 0, None)
                .unwrap_err()
                .to_string()
                .contains("LITERAL_INDEX_LIMIT")
        );
    }
    #[test]
    fn invalid_string_and_instruction_abort_the_index() {
        let mut c = fixture("sample/One", "needle");
        c.methods[0].code.as_mut().unwrap().instructions = vec![0x001a, 99, 0x000e];
        let mut idx = index(&[]);
        idx.symbols.push(c.symbols.clone());
        assert!(
            idx.add_class("sample.One", &c, 0, &AtomicBool::new(false))
                .is_err()
        );
        c.methods[0].code.as_mut().unwrap().instructions = vec![0x001b, 0];
        assert!(
            idx.add_class("sample.One", &c, 0, &AtomicBool::new(false))
                .is_err()
        );
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;
    #[test]
    fn numeric_metadata_allocates_no_literal_owners() {
        let mut index = Index {
            symbols: vec![],
            anchors: vec![],
            owners: vec![],
            postings: BTreeMap::new(),
            estimated_bytes: 0,
            max_bytes: MAX_INDEX_BYTES,
            classes: 0,
        };
        let mut pending = (
            Some(Owner {
                class_id: "sample.Test".into(),
                method_id: None,
                field_id: None,
                kind: "static_value",
                parameter_index: None,
            }),
            None,
        );
        index
            .encoded(
                0,
                &DexValue::Array(vec![DexValue::Int(1); 10000]),
                &mut pending,
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(index.owners.is_empty());
        assert_eq!(index.estimated_bytes, 0);
    }
    #[test]
    fn escaped_large_values_paginate_under_final_frame_budget() {
        let symbols = Arc::new(DexSymbols {
            strings: vec!["\u{0001}".repeat(4096)],
            ..Default::default()
        });
        let mut index = Index {
            symbols: vec![symbols],
            anchors: vec!["sample.Test".into()],
            owners: vec![],
            postings: BTreeMap::new(),
            estimated_bytes: 0,
            max_bytes: MAX_INDEX_BYTES,
            classes: 1,
        };
        let owner = index
            .owner(Owner {
                class_id: "sample.Test".into(),
                method_id: Some("sample.Test.m()V".into()),
                field_id: None,
                kind: "instruction",
                parameter_index: None,
            })
            .unwrap();
        for pc in 0..500 {
            index.add(0, 0, owner, Some(pc)).unwrap();
        }
        let mut offset = 0;
        let mut found = vec![];
        loop {
            let page = index
                .search(
                    &json!({"query":"\u{0001}","limit":500,"offset":offset}),
                    &AtomicBool::new(false),
                )
                .unwrap();
            let envelope = json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":page.to_string()}],"structuredContent":page,"isError":false}});
            assert!(
                serde_json::to_vec(&envelope).unwrap().len() < super::super::MAX_FRAME as usize
            );
            found.extend(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v["code_unit_offset"].as_u64().unwrap()),
            );
            match page["next_offset"].as_u64() {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(found, (0..500).collect::<Vec<_>>());
    }
}
