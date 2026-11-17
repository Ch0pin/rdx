//! On-demand, project-owned source scans. Only one existing bounded page is buffered.
use super::*;
use std::{
    collections::BTreeMap,
    io::{Seek, SeekFrom},
};

pub(super) const DEFAULT_DISK_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_DISK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JOBS: usize = 4;
const MAX_QUERY_BYTES: usize = 32 * 1024;
const ERROR_EXAMPLES: usize = 100;

pub(super) fn is_control(name: &str) -> bool {
    matches!(
        name,
        "get_source_search_status"
            | "get_source_search_results"
            | "cancel_source_search"
            | "release_source_search"
    )
}
pub(super) fn validate_start(args: &Value) -> Result<()> {
    scan_arguments(args).map(|_| ())
}

#[derive(Clone)]
struct Control {
    id: String,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    cleaned: Arc<AtomicBool>,
}
#[derive(Clone)]
struct Job {
    control: Control,
    progress: Arc<Mutex<Value>>,
    spool: Arc<Mutex<Spool>>,
}
#[derive(Default)]
struct Registry {
    jobs: BTreeMap<String, Job>,
    // A released worker still owns its files until it exits, and still occupies admission.
    active: Option<Control>,
    released: Vec<Control>,
}
impl Registry {
    fn reap_released(&mut self) {
        self.released.retain(|control| {
            !control.cleaned.load(Ordering::Acquire) || !control.done.load(Ordering::Acquire)
        });
    }
}
#[derive(Default)]
pub(super) struct Controller {
    stopped: AtomicBool,
    registry: Mutex<Registry>,
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.cancel_all();
    }
}
impl Controller {
    pub(super) fn cancel_all(&self) {
        self.stopped.store(true, Ordering::Release);
        let registry = self.registry.lock().unwrap();
        if let Some(active) = &registry.active {
            active.cancel.store(true, Ordering::Relaxed);
        }
        for job in registry.jobs.values() {
            job.control.cancel.store(true, Ordering::Relaxed);
        }
        for control in &registry.released {
            control.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub(super) fn shutdown_and_drain(&self) {
        self.cancel_all();
        let (jobs, active, released) = {
            let mut registry = self.registry.lock().unwrap();
            (
                std::mem::take(&mut registry.jobs),
                registry.active.take(),
                std::mem::take(&mut registry.released),
            )
        };
        let mut controls: Vec<_> = jobs.values().map(|job| job.control.clone()).collect();
        controls.extend(active);
        controls.extend(released);
        drop(jobs);
        // Admission is stopped. Workers and bounded in-flight result readers are
        // the only remaining spool owners; no interactive engine lock is taken.
        for control in controls {
            while !control.done.load(Ordering::Acquire) || !control.cleaned.load(Ordering::Acquire)
            {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
    pub(super) fn summaries(&self) -> Value {
        let registry = self.registry.lock().unwrap();
        json!({"active_job_id":registry.active.as_ref().filter(|active| !active.done.load(Ordering::Acquire)).map(|active| &active.id),
            "retained_jobs":registry.jobs.iter().map(|(id, job)| {
                let state = job.progress.lock().unwrap();
                json!({"job_id":id,"status":state["status"],"committed_items":state["committed_items"],
                    "worker_finished":job.control.done.load(Ordering::Acquire)})
            }).collect::<Vec<_>>(),"released_jobs_pending_cleanup":registry.released.iter().filter(|control| !control.cleaned.load(Ordering::Acquire)).map(|control| &control.id).collect::<Vec<_>>(),"max_retained_jobs":MAX_JOBS})
    }
    fn job(&self, id: &str) -> Result<Job> {
        let registry = self.registry.lock().unwrap();
        ensure!(
            !self.stopped.load(Ordering::Acquire),
            "SOURCE_SEARCH_SERVICE_STOPPED"
        );
        registry.jobs.get(id).cloned()
            .context("SOURCE_SEARCH_JOB_NOT_FOUND: jobs belong to the pinned project session and may have been released")
    }
    pub(super) fn status(&self, id: &str, cancel: bool) -> Result<Value> {
        let job = self.job(id)?;
        if cancel {
            job.control.cancel.store(true, Ordering::Relaxed);
        }
        Ok(job.status())
    }
    pub(super) fn release(&self, id: &str) -> Result<Value> {
        let mut registry = self.registry.lock().unwrap();
        registry.reap_released();
        let job = registry
            .jobs
            .remove(id)
            .context("SOURCE_SEARCH_JOB_NOT_FOUND")?;
        job.control.cancel.store(true, Ordering::Relaxed);
        // In-flight readers may outlive lookup removal and the next active job.
        // Their spool remains charged to admission until its last handle drops.
        registry.released.push(job.control.clone());
        drop(registry);
        Ok(json!({"job_id":id,"released":true,"cancel_requested":true,
            "worker_finished":job.control.done.load(Ordering::Acquire),
            "cleanup":"Private result files are removed after the worker and in-flight readers drop their handles."}))
    }
    pub(super) fn results(&self, id: &str, args: &Value) -> Result<Value> {
        let job = self.job(id)?;
        let offset = args["offset"].as_u64().unwrap_or(0);
        let limit = args["limit"].as_u64().unwrap_or(100) as usize;
        let budget = budget::requested(args)?;
        let mut spool = job.spool.lock().unwrap();
        let state = job.status();
        ensure!(
            offset <= spool.rows,
            "Invalid result offset: committed_items={}",
            spool.rows
        );
        let mut result = json!({"job_id":id,"items":[],"offset":offset,"limit":limit,
            "committed_items":spool.rows,"status":state["status"],"scan_complete":state["scan_complete"],
            "coverage_complete":state["coverage_complete"],"coverage":state["coverage"],
            "worker_finished":state["worker_finished"],"has_more":false,"next_offset":null,
            "poll_offset":offset,"results_exhausted":false,"continuation_available":state["continuation"].is_object(),
            "response_byte_budget":budget,"response_budget_limited":false});
        let mut bytes = serde_json::to_vec(&result)?.len() + 256;
        ensure!(
            bytes <= budget,
            "RESULT_TOO_LARGE: increase max_output_bytes"
        );
        let mut items = Vec::new();
        for row in offset..spool.rows.min(offset.saturating_add(limit as u64)) {
            let (position, length) = spool.entry(row)?;
            if bytes.saturating_add(length as usize + 1) > budget {
                ensure!(
                    !items.is_empty(),
                    "RESULT_TOO_LARGE: one source hit needs more than max_output_bytes; increase the response budget"
                );
                result["response_budget_limited"] = json!(true);
                break;
            }
            ensure!(
                length <= MAX_DISK_BYTES,
                "Invalid source search record size"
            );
            let mut data = vec![0; length as usize];
            spool.data.seek(SeekFrom::Start(position))?;
            spool.data.read_exact(&mut data)?;
            items.push(serde_json::from_slice::<Value>(&data)?);
            bytes += length as usize + 1;
        }
        let end = offset + items.len() as u64;
        let finished = state["worker_finished"] == true;
        let more = end < spool.rows;
        result["items"] = json!(items);
        result["has_more"] = json!(more);
        result["next_offset"] = if more || !finished {
            json!(end)
        } else {
            Value::Null
        };
        result["poll_offset"] = json!(end);
        result["results_exhausted"] = json!(!more && finished);
        ensure!(
            serde_json::to_vec(&result)?.len() <= budget,
            "RESULT_TOO_LARGE: increase max_output_bytes"
        );
        Ok(result)
    }
    pub(super) fn start(&self, project: &LoadedProject, args: &Value) -> Result<Value> {
        let query = scan_arguments(args)?;
        let disk_budget = args["disk_budget_bytes"]
            .as_u64()
            .unwrap_or(DEFAULT_DISK_BYTES);
        ensure!(
            (16384..=MAX_DISK_BYTES).contains(&disk_budget),
            "disk_budget_bytes must be 16384..{MAX_DISK_BYTES}"
        );
        ensure!(
            !self.stopped.load(Ordering::Acquire),
            "SOURCE_SEARCH_SERVICE_STOPPED"
        );
        let mut registry = self.registry.lock().unwrap();
        registry.reap_released();
        ensure!(
            !self.stopped.load(Ordering::Acquire),
            "SOURCE_SEARCH_SERVICE_STOPPED"
        );
        ensure!(
            registry
                .active
                .as_ref()
                .is_none_or(|job| job.done.load(Ordering::Acquire)),
            "SOURCE_SEARCH_BUSY: active_job_id={}; poll or cancel it",
            registry.active.as_ref().unwrap().id
        );
        ensure!(
            registry.jobs.len() + registry.released.len() < MAX_JOBS,
            "SOURCE_SEARCH_JOB_LIMIT: retained_job_ids={:?}; release_source_search removes retained results; at most {MAX_JOBS} jobs per project",
            registry
                .jobs
                .keys()
                .chain(registry.released.iter().map(|control| &control.id))
                .collect::<Vec<_>>()
        );
        let id = random_id()?;
        let spool = Spool::new(disk_budget)?;
        let initial_cursor = query["resume_cursor"].clone();
        let from_start = initial_cursor.is_null()
            && query["offset"].as_u64().unwrap_or(0) == 0
            && query["source_offset"].as_u64().unwrap_or(0) == 0;
        let state = json!({"job_id":id,"status":"running","phase":"queued","query":query,
            "project_id":project.project_id,"apk_sha256":project.hash,"disk_budget_bytes":disk_budget,
            "stored_bytes":0,"committed_items":0,"committed_pages":0,"scan_complete":false,
            "coverage_complete":false,"started_from_beginning":from_start,"initial_cursor":initial_cursor,
            "next_cursor":query["resume_cursor"],"resume_arguments":query,"continuation":null,
            "committed_position":if initial_cursor.is_string() {Value::Null} else {json!(query["offset"].as_u64().unwrap_or(0))},
            "committed_source_offset":query["source_offset"].as_u64().unwrap_or(0),
            "coverage":{"page_error_count":0,"errors_truncated":false,"truncated_resources":0,
                "skipped_binary_resources":0,"scanned_class_visits":0,"scanned_resource_visits":0,
                "prior_coverage_unknown":!from_start},"errors":[],"error":null,
            "progress":{"position":query["offset"].as_u64().unwrap_or(0),"source_offset":query["source_offset"].as_u64().unwrap_or(0)},
            "result_retention":"Explicit release_source_search or project/service shutdown; retrieval never changes scan completion.",
            "completion_contract":"scan_complete means every cursor from this job's start was consumed. coverage_complete additionally requires start at beginning and no diagnostic gaps. Results can be read while running; retain earlier job diagnostics when resuming a suffix."});
        let job = Job {
            control: Control {
                id: id.clone(),
                cancel: Arc::new(AtomicBool::new(false)),
                done: Arc::new(AtomicBool::new(false)),
                cleaned: spool._cleanup.0.clone(),
            },
            progress: Arc::new(Mutex::new(state)),
            spool: Arc::new(Mutex::new(spool)),
        };
        let snapshot = job.status();
        let (engine, classes, path, metadata) = (
            project.analysis_snapshot.clone(),
            project.project.clone(),
            project.path.clone(),
            project.metadata.clone(),
        );
        let worker = job.clone();
        thread::Builder::new()
            .name("rdx-source-search".into())
            .spawn(move || {
                let mut engine = engine.as_ref().clone();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&worker, &mut engine, &classes, query, || {
                        let now = fs::metadata(&path)?;
                        ensure!(
                            now.len() == metadata.len()
                                && now.modified()? == metadata.modified()?,
                            "PROJECT_FILE_CHANGED"
                        );
                        Ok(())
                    })
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("SOURCE_SEARCH_WORKER_PANICKED")));
                if let Err(error) = outcome {
                    worker.finish(
                        if worker.control.cancel.load(Ordering::Relaxed) {
                            "cancelled"
                        } else {
                            "failed"
                        },
                        Some(format!("{error:#}")),
                    );
                }
                let done = worker.control.done.clone();
                drop(engine);
                drop(worker);
                done.store(true, Ordering::Release);
            })
            .context("Cannot start source search worker")?;
        registry.active = Some(job.control.clone());
        registry.jobs.insert(id, job);
        Ok(snapshot)
    }
}
fn scan_arguments(args: &Value) -> Result<Value> {
    let mut query = args.clone();
    let object = query
        .as_object_mut()
        .context("Arguments must be an object")?;
    object.remove("disk_budget_bytes");
    object.entry("limit").or_insert(json!(500));
    validate("search_code", &query)?;
    ensure!(
        query["mode"] != "identifiers",
        "Background source search accepts source text only"
    );
    ensure!(
        serde_json::to_vec(&query)?.len() <= MAX_QUERY_BYTES,
        "Source search arguments exceed 32 KiB"
    );
    crate::search::CompiledQuery::new(crate::search::SearchQuery {
        text: required(&query, "query")?.into(),
        regex: query["regex"].as_bool().unwrap_or(false),
        case_sensitive: query["case_sensitive"].as_bool().unwrap_or(true),
        ..Default::default()
    })
    .map_err(anyhow::Error::msg)?;
    Ok(query)
}
impl Job {
    fn status(&self) -> Value {
        let mut state = self.progress.lock().unwrap().clone();
        state["cancel_requested"] = json!(self.control.cancel.load(Ordering::Relaxed));
        state["worker_finished"] = json!(self.control.done.load(Ordering::Acquire));
        state
    }
    fn finish(&self, status: &str, error: Option<String>) {
        let mut state = self.progress.lock().unwrap();
        state["status"] = json!(status);
        state["phase"] = json!("finished");
        state["error"] = json!(error.map(|text| text.chars().take(2000).collect::<String>()));
        if state["scan_complete"] != true {
            let from_beginning = state["resume_arguments"]["resume_cursor"].is_null()
                && state["resume_arguments"]["offset"].as_u64().unwrap_or(0) == 0
                && state["resume_arguments"]["source_offset"]
                    .as_u64()
                    .unwrap_or(0)
                    == 0;
            state["continuation"] = json!({"tool":"start_source_search","arguments":state["resume_arguments"],
                "from_beginning":from_beginning,"checkpoint":"first_unpublished_page",
                "replay":"Starts at the first uncommitted page; a page interrupted by cancellation, a storage limit or failure is replayed in full. Preserve this job's earlier results and coverage diagnostics."});
        }
    }
    fn commit(&self, page: &Value) -> Result<bool> {
        let items = page["items"]
            .as_array()
            .context("Missing source page items")?;
        let mut spool = self.spool.lock().unwrap();
        if !spool.append(items)? {
            return Ok(false);
        }
        let mut state = self.progress.lock().unwrap();
        state["committed_items"] = json!(spool.rows);
        state["stored_bytes"] = json!(spool.bytes);
        state["committed_pages"] = json!(state["committed_pages"].as_u64().unwrap() + 1);
        for (destination, source) in [
            ("page_error_count", "page_error_count"),
            ("truncated_resources", "truncated_resources"),
            ("skipped_binary_resources", "skipped_binary_resources"),
            ("scanned_class_visits", "scanned_classes"),
            ("scanned_resource_visits", "scanned_resources"),
        ] {
            state["coverage"][destination] = json!(
                state["coverage"][destination].as_u64().unwrap()
                    + page[source].as_u64().unwrap_or(0)
            );
        }
        let errors = state["errors"].as_array_mut().unwrap();
        for error in page["errors"].as_array().into_iter().flatten() {
            if errors.len() == ERROR_EXAMPLES {
                break;
            }
            // Keep each example bounded even for maliciously long input names.
            let mut error = error.clone();
            if let Some(object) = error.as_object_mut() {
                for value in object.values_mut() {
                    if let Some(text) = value.as_str() {
                        *value = json!(text.chars().take(512).collect::<String>());
                    }
                }
            }
            errors.push(error);
        }
        state["coverage"]["errors_truncated"] = json!(
            state["coverage"]["errors_truncated"] == true
                || page["errors_truncated"] == true
                || state["coverage"]["page_error_count"].as_u64().unwrap()
                    > state["errors"].as_array().unwrap().len() as u64
        );
        let total_classes = page["total_classes"].as_u64().unwrap_or(0);
        let total_resources = page["total_resources"].as_u64().unwrap_or(0);
        let position = page["next_offset"]
            .as_u64()
            .unwrap_or(total_classes + total_resources);
        state["committed_position"] = json!(position);
        state["committed_source_offset"] = json!(page["next_source_offset"].as_u64().unwrap_or(0));
        state["coverage"]["completed_classes_frontier"] = json!(position.min(total_classes));
        state["coverage"]["completed_resources_frontier"] =
            json!(position.saturating_sub(total_classes));
        state["coverage"]["total_classes"] = json!(total_classes);
        state["coverage"]["total_resources"] = json!(total_resources);
        state["last_page_status"] = page["page_status"].clone();
        state["next_cursor"] = page["next_cursor"].clone();
        if let Some(cursor) = page["next_cursor"].as_str() {
            state["resume_arguments"]
                .as_object_mut()
                .unwrap()
                .remove("offset");
            state["resume_arguments"]
                .as_object_mut()
                .unwrap()
                .remove("source_offset");
            state["resume_arguments"]["resume_cursor"] = json!(cursor);
        } else {
            state["scan_complete"] = json!(true);
            state["coverage_complete"] = json!(
                state["started_from_beginning"] == true
                    && state["coverage"]["page_error_count"] == 0
                    && state["coverage"]["truncated_resources"] == 0
                    && state["coverage"]["errors_truncated"] == false
            );
        }
        Ok(true)
    }
}
fn run(
    job: &Job,
    engine: &mut NativeEngine,
    project: &Project,
    mut args: Value,
    check: impl Fn() -> Result<()>,
) -> Result<()> {
    loop {
        ensure!(!job.control.cancel.load(Ordering::Relaxed), "CANCELLED");
        check()?;
        let page = source_search::search_hits_with_progress(
            engine,
            project,
            &args,
            &job.control.cancel,
            &mut |progress| {
                let mut state = job.progress.lock().unwrap();
                state["phase"] = progress["phase"].clone();
                state["progress"] = progress;
                state["progress"]["scope"] = json!(
                    "Live preparation/matching for the current uncommitted page; exact committed_position is reported separately"
                );
            },
        )?;
        check()?;
        ensure!(!job.control.cancel.load(Ordering::Relaxed), "CANCELLED");
        if !job.commit(&page)? {
            job.finish("storage_limit", None);
            return Ok(());
        }
        if page["exhausted"] == true {
            job.finish("complete", None);
            return Ok(());
        }
        let cursor = page["next_cursor"]
            .as_str()
            .context("SOURCE_SEARCH_MISSING_CURSOR")?;
        ensure!(
            args["resume_cursor"].as_str() != Some(cursor),
            "SOURCE_SEARCH_CURSOR_STALLED"
        );
        args.as_object_mut().unwrap().remove("offset");
        args.as_object_mut().unwrap().remove("source_offset");
        args["resume_cursor"] = json!(cursor);
        // Page admission is released before this point; do not retain a request work slot.
        thread::yield_now();
    }
}
struct Spool {
    data: File,
    index: File,
    rows: u64,
    bytes: u64,
    budget: u64,
    // Close handles before removing the private directory (required on Windows).
    _directory: tempfile::TempDir,
    // Last field: signal cleanup only after File and TempDir destructors ran.
    _cleanup: Cleanup,
}
struct Cleanup(Arc<AtomicBool>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl Spool {
    fn new(budget: u64) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("rdx-source-search-")
            .tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        }
        fn create(path: &Path) -> Result<File> {
            let mut options = OpenOptions::new();
            options.create_new(true).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            Ok(options.open(path)?)
        }
        let data = create(&directory.path().join("hits.jsonl"))?;
        let index = create(&directory.path().join("offsets.bin"))?;
        Ok(Self {
            _directory: directory,
            data,
            index,
            rows: 0,
            bytes: 0,
            budget,
            _cleanup: Cleanup(Arc::new(AtomicBool::new(false))),
        })
    }
    /// Preflight a whole page; failed admission rewinds to its exact input cursor.
    fn append(&mut self, items: &[Value]) -> Result<bool> {
        let mut added = 0u64;
        for item in items {
            added = added
                .checked_add(serde_json::to_vec(item)?.len() as u64 + 17)
                .context("Source result size overflow")?;
            if self.bytes.saturating_add(added) > self.budget {
                return Ok(false);
            }
        }
        let old_data = self.data.metadata()?.len();
        let old_index = self.index.metadata()?.len();
        let result = (|| -> Result<()> {
            self.data.seek(SeekFrom::Start(old_data))?;
            self.index.seek(SeekFrom::Start(old_index))?;
            let mut position = old_data;
            for item in items {
                let bytes = serde_json::to_vec(item)?;
                self.index.write_all(&position.to_le_bytes())?;
                self.index.write_all(&(bytes.len() as u64).to_le_bytes())?;
                self.data.write_all(&bytes)?;
                self.data.write_all(b"\n")?;
                position += bytes.len() as u64 + 1;
            }
            self.data.flush()?;
            self.index.flush()?;
            Ok(())
        })();
        if let Err(error) = result {
            self.data
                .set_len(old_data)
                .context("Cannot rewind source result data after write failure")?;
            self.index
                .set_len(old_index)
                .context("Cannot rewind source result offsets after write failure")?;
            return Err(error);
        }
        self.rows += items.len() as u64;
        self.bytes += added;
        Ok(true)
    }
    fn entry(&mut self, row: u64) -> Result<(u64, u64)> {
        self.index.seek(SeekFrom::Start(
            row.checked_mul(16).context("Invalid result offset")?,
        ))?;
        let mut bytes = [0; 16];
        self.index.read_exact(&mut bytes)?;
        Ok((
            u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            u64::from_le_bytes(bytes[8..].try_into().unwrap()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn loaded(sources: &[(&str, Option<&str>)]) -> LoadedProject {
        let snapshot = ProjectSnapshot::open(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello.apk"),
        )
        .unwrap();
        let mut loaded =
            LoadedProject::from_snapshot(snapshot, Arc::new(AtomicBool::new(false))).unwrap();
        loaded.project = Arc::new(Project {
            classes: sources.iter().map(|(name, _)| (*name).into()).collect(),
            resources: vec![],
        });
        for (name, source) in sources {
            if let Some(source) = source {
                let target = crate::search::SearchTarget::Class((*name).into());
                loaded.analysis_snapshot.search_index().insert(
                    target.clone(),
                    &Some((
                        Arc::new(crate::search::SearchDocument {
                            target,
                            name: (*name).into(),
                            source: (*source).into(),
                            syntax: "java".into(),
                            links: vec![],
                            source_hash: Some(crate::engine::source_identity(source)),
                            metadata_complete: true,
                        }),
                        Arc::new(vec![]),
                        None,
                    )),
                );
            }
        }
        loaded
    }
    fn args(project: &LoadedProject, query: &str) -> Value {
        json!({"instance_id":"test-instance","project_id":project.project_id,"query":query,"limit":1})
    }
    fn wait(controller: &Controller, id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = controller.status(id, false).unwrap();
            if state["worker_finished"] == true {
                return state;
            }
            assert!(Instant::now() < deadline, "{state}");
            thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn background_auto_consumes_unicode_hit_cursors_and_reuses_shared_index() {
        let loaded = loaded(&[("app.A", Some("é api(); api(); api();"))]);
        let before = loaded.analysis_snapshot.search_index().renders();
        let started = loaded
            .source_jobs
            .start(&loaded, &args(&loaded, "api"))
            .unwrap();
        let id = started["job_id"].as_str().unwrap();
        let state = wait(&loaded.source_jobs, id);
        assert_eq!(state["status"], "complete");
        assert_eq!(state["coverage_complete"], true);
        assert_eq!(state["committed_items"], 3);
        assert_eq!(state["committed_pages"], 3);
        assert_eq!(state["committed_position"], 1);
        assert_eq!(state["coverage"]["completed_classes_frontier"], 1);
        let page = loaded
            .source_jobs
            .results(id, &json!({"limit":500}))
            .unwrap();
        let starts: Vec<_> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["start"].as_u64().unwrap())
            .collect();
        assert_eq!(starts, vec![2, 9, 16]);
        assert_eq!(page["results_exhausted"], true);
        assert_eq!(loaded.analysis_snapshot.search_index().renders(), before);
    }
    #[test]
    fn early_page_errors_survive_a_clean_empty_final_page() {
        let loaded = loaded(&[("missing.Bad", None), ("app.A", Some("present"))]);
        let mut args = args(&loaded, "absent");
        args["pagination"] = json!("classes");
        let started = loaded.source_jobs.start(&loaded, &args).unwrap();
        let state = wait(&loaded.source_jobs, started["job_id"].as_str().unwrap());
        assert_eq!(state["scan_complete"], true);
        assert_eq!(state["coverage_complete"], false);
        assert_eq!(state["coverage"]["page_error_count"], 1);
        assert_eq!(state["last_page_status"]["complete"], true);
        assert_eq!(state["committed_items"], 0);
        assert_eq!(state["errors"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn storage_limit_replays_first_unpublished_page_without_committing_hits() {
        let text = "api(); ".repeat(500);
        let loaded = loaded(&[("app.A", Some(&text))]);
        let mut args = args(&loaded, "api");
        args["limit"] = json!(500);
        args["disk_budget_bytes"] = json!(16384);
        let started = loaded.source_jobs.start(&loaded, &args).unwrap();
        let state = wait(&loaded.source_jobs, started["job_id"].as_str().unwrap());
        assert_eq!(state["status"], "storage_limit");
        assert_eq!(state["committed_items"], 0);
        assert_eq!(state["committed_pages"], 0);
        assert_eq!(state["stored_bytes"], 0);
        assert_eq!(state["scan_complete"], false);
        assert_eq!(state["continuation"]["from_beginning"], true);
        assert_eq!(state["continuation"]["arguments"]["query"], "api");
        assert_eq!(
            state["continuation"]["arguments"]["project_id"],
            loaded.project_id
        );
    }
    #[test]
    fn spool_admission_accounts_for_offsets_and_drop_closes_handles_before_cleanup() {
        let row = json!({"snippet":"é api();"});
        let bytes = serde_json::to_vec(&row).unwrap().len() as u64 + 17;
        let mut spool = Spool::new(bytes - 1).unwrap();
        assert!(!spool.append(std::slice::from_ref(&row)).unwrap());
        assert_eq!(spool.data.metadata().unwrap().len(), 0);
        assert_eq!(spool.index.metadata().unwrap().len(), 0);
        spool.budget = bytes;
        assert!(spool.append(&[row]).unwrap());
        assert_eq!(spool.bytes, bytes);
        assert_eq!(
            spool.data.metadata().unwrap().len() + spool.index.metadata().unwrap().len(),
            bytes
        );
        let directory = spool._directory.path().to_owned();
        drop(spool);
        assert!(!directory.exists());
    }
    #[test]
    fn cancellation_and_release_preserve_admission_and_clean_after_worker_exits() {
        let loaded = loaded(&[("app.A", Some("api();"))]);
        let index = loaded.analysis_snapshot.search_index();
        let permit = index.source_page(|| false).unwrap();
        let args = args(&loaded, "api");
        let started = loaded.source_jobs.start(&loaded, &args).unwrap();
        let id = started["job_id"].as_str().unwrap();
        let job = loaded.source_jobs.job(id).unwrap();
        let directory = job.spool.lock().unwrap()._directory.path().to_owned();
        assert!(
            loaded
                .source_jobs
                .start(&loaded, &args)
                .unwrap_err()
                .to_string()
                .contains(id)
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while loaded.source_jobs.status(id, false).unwrap()["phase"] != "waiting_for_source_page" {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        loaded.source_jobs.release(id).unwrap();
        assert!(loaded.source_jobs.status(id, false).is_err());
        while !job.control.done.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(job.status()["status"], "cancelled");
        assert_eq!(job.status()["continuation"]["from_beginning"], true);
        drop(job);
        drop(permit);
        assert!(!directory.exists());
    }
    #[test]
    fn shutdown_drains_released_readers_even_after_a_new_job_replaces_active_control() {
        let loaded = loaded(&[("app.A", Some("api();"))]);
        let query = args(&loaded, "api");
        let first = loaded.source_jobs.start(&loaded, &query).unwrap();
        let id = first["job_id"].as_str().unwrap();
        wait(&loaded.source_jobs, id);
        let old_reader = loaded.source_jobs.job(id).unwrap();
        let directory = old_reader
            .spool
            .lock()
            .unwrap()
            ._directory
            .path()
            .to_owned();
        loaded.source_jobs.release(id).unwrap();
        let next = loaded.source_jobs.start(&loaded, &query).unwrap();
        wait(&loaded.source_jobs, next["job_id"].as_str().unwrap());
        let controller = loaded.source_jobs.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = thread::spawn(move || {
            controller.shutdown_and_drain();
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "old reader must retain its spool"
        );
        drop(old_reader);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        thread.join().unwrap();
        assert!(!directory.exists());
    }

    #[test]
    fn retained_jobs_are_bounded_discoverable_project_scoped_and_stopped_controller_rejects_start()
    {
        let loaded = loaded(&[("app.A", Some("api();"))]);
        let args = args(&loaded, "absent");
        let mut ids = vec![];
        for _ in 0..MAX_JOBS {
            let started = loaded.source_jobs.start(&loaded, &args).unwrap();
            let id = started["job_id"].as_str().unwrap().to_owned();
            wait(&loaded.source_jobs, &id);
            ids.push(id);
        }
        let message = loaded
            .source_jobs
            .start(&loaded, &args)
            .unwrap_err()
            .to_string();
        assert!(message.contains("SOURCE_SEARCH_JOB_LIMIT") && message.contains(&ids[0]));
        assert_eq!(
            loaded.source_jobs.summaries()["retained_jobs"]
                .as_array()
                .unwrap()
                .len(),
            MAX_JOBS
        );
        assert!(Controller::default().status(&ids[0], false).is_err());
        loaded.source_jobs.release(&ids[0]).unwrap();
        loaded.source_jobs.cancel_all();
        assert!(
            loaded
                .source_jobs
                .start(&loaded, &args)
                .unwrap_err()
                .to_string()
                .contains("SOURCE_SEARCH_SERVICE_STOPPED")
        );
    }
}
