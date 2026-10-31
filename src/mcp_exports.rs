//! Project-owned export jobs with durable, validated continuation checkpoints.
use super::*;
use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom};

#[derive(Clone)]
struct Job {
    progress: Arc<Mutex<Value>>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}
#[derive(Default)]
pub(super) struct Controller {
    stopped: AtomicBool,
    jobs: Mutex<BTreeMap<String, Job>>,
}
impl Controller {
    pub(super) fn cancel_all(&self) {
        self.stopped.store(true, Ordering::Release);
        for job in self.jobs.lock().unwrap().values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub(super) fn status(&self, id: &str, cancel: bool) -> Result<Value> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs.get(id).context("EXPORT_JOB_NOT_FOUND: jobs belong to this project session; resume an interrupted export using its out_path")?;
        if cancel {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let mut result = job.progress.lock().unwrap().clone();
        result["cancel_requested"] = json!(job.cancel.load(Ordering::Relaxed));
        result["worker_finished"] = json!(job.done.load(Ordering::Acquire));
        Ok(result)
    }
    pub(super) fn start(&self, project: &LoadedProject, args: &Value) -> Result<Value> {
        let mut jobs = self.jobs.lock().unwrap();
        ensure!(
            !self.stopped.load(Ordering::Acquire),
            "EXPORT_SERVICE_STOPPED"
        );
        ensure!(
            jobs.values().all(|j| j.done.load(Ordering::Acquire)),
            "EXPORT_BUSY: one export per project; poll or cancel the active job"
        );
        let mut session = Session::open(&project.project, args, &project.hash)?;
        let progress = Arc::new(Mutex::new(session.state.clone()));
        let job = Job {
            progress: progress.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(false)),
        };
        let id = session.state["job_id"].as_str().unwrap().to_owned();
        // Retain only bounded metadata; workers share the existing parsed project.
        while jobs.len() >= 8 {
            let key = jobs.keys().next().unwrap().clone();
            jobs.remove(&key);
        }
        let engine = project.analysis_snapshot.clone();
        let classes = project.project.clone();
        let path = project.path.clone();
        let metadata = project.metadata.clone();
        let worker = job.clone();
        let snapshot = session.state.clone();
        let started = thread::Builder::new()
            .name("rdx-source-export".into())
            .spawn(move || {
                let mut engine = engine.as_ref().clone();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    session.run(
                        &mut engine,
                        &classes,
                        &worker.cancel,
                        Some(&worker.progress),
                        || {
                            let now = fs::metadata(&path)?;
                            ensure!(
                                now.len() == metadata.len()
                                    && now.modified()? == metadata.modified()?,
                                "PROJECT_FILE_CHANGED"
                            );
                            Ok(())
                        },
                    )
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("EXPORT_WORKER_PANICKED")));
                session.finish(outcome, &worker.cancel, Some(&worker.progress));
                drop(session);
                worker.done.store(true, Ordering::Release);
            });
        if let Err(error) = started {
            let mut failure = snapshot.clone();
            failure["status"] = json!("incomplete");
            failure["error"] = json!(format!("Cannot start export worker: {error}"));
            let out = Path::new(required(&failure, "out_path")?);
            save(&out.join("export-status.json"), &failure)?;
            save(&out.join("completion.json"), &failure)?;
            bail!("Cannot start export worker: {error}");
        }
        jobs.insert(id, job);
        Ok(snapshot)
    }
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
}
fn regular(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "Expected a regular export file: {}",
        path.display()
    );
    Ok(())
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing export parent")?)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn selected<'a>(project: &'a Project, args: &Value) -> Vec<&'a String> {
    project
        .classes
        .iter()
        .filter(|n| scan::package_matches(n, args))
        .collect()
}
struct Session {
    out: PathBuf,
    _lock: File,
    state: Value,
}
impl Session {
    fn open(project: &Project, args: &Value, apk_hash: &str) -> Result<Self> {
        let out = PathBuf::from(required(args, "out_path")?);
        ensure!(out.is_absolute(), "out_path must be absolute");
        let names = selected(project, args);
        ensure!(!names.is_empty(), "No classes match package_prefix");
        let prefix = args["package_prefix"]
            .as_str()
            .unwrap_or("")
            .trim_end_matches('.');
        let selection_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&names)?));
        let renderer = build_identity()["executable_sha256"].clone();
        ensure!(
            renderer.as_str().is_some_and(|s| !s.is_empty()),
            "Cannot identify the renderer executable for resumable export"
        );
        let resume = args["resume"] == true;
        if resume {
            ensure!(
                fs::symlink_metadata(&out)?.file_type().is_dir(),
                "Resume path must be a regular directory"
            );
        } else {
            discovery::check_export_parent(&out)?;
            fs::create_dir(&out)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&out, fs::Permissions::from_mode(0o700))?;
            }
        }
        let lock = options()
            .create(true)
            .truncate(false)
            .open(out.join(".export.lock"))?;
        lock.try_lock()
            .context("EXPORT_LOCKED: another worker owns this directory")?;
        let mut state = if resume {
            regular(&out.join("export-status.json"))?;
            let file = File::open(out.join("export-status.json"))?;
            ensure!(
                file.metadata()?.len() <= 65536,
                "Invalid export status size"
            );
            let state: Value = serde_json::from_reader(file)?;
            ensure!(
                state["checkpoint_version"] == 1
                    && state["apk_sha256"] == apk_hash
                    && state["renderer_sha256"] == renderer
                    && state["package_prefix"] == prefix
                    && state["selection_hash"] == selection_hash
                    && state["total"] == names.len(),
                "EXPORT_IDENTITY_MISMATCH: resume requires the same APK, renderer and package selection"
            );
            ensure!(state["status"] != "complete", "EXPORT_ALREADY_COMPLETE");
            for key in ["next_class", "manifest_bytes", "exported", "failed"] {
                ensure!(
                    state[key].as_u64().is_some(),
                    "Invalid export checkpoint: {key}"
                );
            }
            ensure!(
                state["next_class"].as_u64().unwrap() <= names.len() as u64,
                "Invalid export cursor"
            );
            state
        } else {
            let manifest = options()
                .create_new(true)
                .open(out.join("manifest.jsonl"))?;
            manifest.sync_all()?;
            json!({"checkpoint_version":1,"apk_sha256":apk_hash,"renderer_sha256":renderer,
                "package_prefix":prefix,"selection_hash":selection_hash,"total":names.len(),"selected":names.len(),
                "next_class":0,"manifest_bytes":0,"exported":0,"failed":0})
        };
        state["job_id"] = json!(random_id()?);
        state["instance_id"] = args["instance_id"].clone();
        state["project_id"] = args["project_id"].clone();
        state["out_path"] = json!(out);
        state["manifest_path"] = json!(out.join("manifest.jsonl"));
        state["status_path"] = json!(out.join("export-status.json"));
        state["completion_path"] = json!(out.join("completion.json"));
        state["status"] = json!("running");
        state["complete"] = json!(false);
        state["error"] = Value::Null;
        state["persistence_error"] = Value::Null;
        state["manifest_sha256"] = Value::Null;
        state["updated_at_unix_ms"] = json!(unix_ms());
        state["recovery"] = json!(
            "Resume with export_package_source(resume=true) or rdx --export-source INPUT OUTPUT --resume, keeping the APK, executable and package prefix unchanged. A persisted running status without a live job means interrupted work, not completion."
        );
        save(&out.join("export-status.json"), &state)?;
        if resume {
            // Clear a previous terminal marker for this continuation attempt.
            save(&out.join("completion.json"), &state)?;
        }
        Ok(Self {
            out,
            _lock: lock,
            state,
        })
    }
    fn publish(&mut self, progress: Option<&Mutex<Value>>) -> Result<()> {
        self.state["updated_at_unix_ms"] = json!(unix_ms());
        save(&self.out.join("export-status.json"), &self.state)?;
        if let Some(progress) = progress {
            *progress.lock().unwrap() = self.state.clone();
        }
        Ok(())
    }
    fn run(
        &mut self,
        engine: &mut NativeEngine,
        project: &Project,
        cancel: &AtomicBool,
        progress: Option<&Mutex<Value>>,
        verify: impl Fn() -> Result<()>,
    ) -> Result<()> {
        let names = selected(project, &self.state);
        let next = self.state["next_class"]
            .as_u64()
            .context("Invalid export cursor")? as usize;
        let manifest_path = self.out.join("manifest.jsonl");
        regular(&manifest_path)?;
        let mut manifest = options().open(&manifest_path)?;
        let checkpoint = self.state["manifest_bytes"].as_u64().unwrap();
        ensure!(
            manifest.metadata()?.len() >= checkpoint,
            "EXPORT_CHECKPOINT_CORRUPT: manifest is shorter than checkpoint"
        );
        let mut reader = BufReader::new((&mut manifest).take(checkpoint));
        let mut exported = 0;
        let mut failed = 0;
        for (i, name) in names.iter().take(next).enumerate() {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let mut line = String::new();
            reader.by_ref().take(MAX_FRAME).read_line(&mut line)?;
            ensure!(
                line.ends_with('\n'),
                "EXPORT_CHECKPOINT_CORRUPT: incomplete manifest record"
            );
            let row: Value = serde_json::from_str(&line)?;
            ensure!(
                row["class_id"] == name.as_str(),
                "EXPORT_CHECKPOINT_CORRUPT: class order differs"
            );
            if row.get("error").is_some() {
                failed += 1;
            } else {
                let path = self.out.join(format!("class-{i:08}.txt"));
                regular(&path)?;
                ensure!(
                    row["source_path"] == json!(path) && row["sha256"] == digest(&path)?,
                    "EXPORT_CHECKPOINT_CORRUPT: source file changed"
                );
                exported += 1;
            }
        }
        let mut extra = [0u8; 1];
        ensure!(
            reader.read(&mut extra)? == 0,
            "EXPORT_CHECKPOINT_CORRUPT: extra committed records"
        );
        ensure!(
            self.state["exported"] == exported && self.state["failed"] == failed,
            "EXPORT_CHECKPOINT_CORRUPT: counts differ"
        );
        // Discard only the uncommitted journal tail; committed files were verified.
        manifest.set_len(checkpoint)?;
        manifest.seek(SeekFrom::Start(checkpoint))?;
        for (index, name) in names.iter().enumerate().skip(next) {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            verify()?;
            let path = self.out.join(format!("class-{index:08}.txt"));
            let record = match engine.decompile_class_source_report(name, cancel) {
                Ok((code, coverage)) => {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    let hash = format!("{:x}", Sha256::digest(code.source.as_bytes()));
                    if path.symlink_metadata().is_ok() {
                        // A crash can publish source before its journal record. Re-render
                        // and verify exactly; never overwrite an existing source file.
                        regular(&path)?;
                        ensure!(
                            digest(&path)? == hash,
                            "EXPORT_FILE_CONFLICT: {}",
                            path.display()
                        );
                    } else {
                        let mut file = tempfile::NamedTempFile::new_in(&self.out)?;
                        file.write_all(code.source.as_bytes())?;
                        file.as_file().sync_all()?;
                        file.persist_noclobber(&path).map_err(|e| e.error)?;
                    }
                    json!({"class_id":name,"descriptor":format!("L{};",name.replace('.',"/")),"source_path":path,
                        "sha256":hash,"size_bytes":code.source.len(),"coverage":coverage})
                }
                Err(error) => {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    json!({"class_id":name,"error":format!("{error:#}")})
                }
            };
            let serialized = serde_json::to_vec(&record)?;
            ensure!(
                serialized.len() < MAX_FRAME as usize - 1,
                "EXPORT_RECORD_TOO_LARGE: class coverage exceeds the resumable record budget"
            );
            manifest.write_all(&serialized)?;
            manifest.write_all(b"\n")?;
            manifest.sync_all()?;
            let key = if record.get("error").is_some() {
                "failed"
            } else {
                "exported"
            };
            self.state[key] = json!(self.state[key].as_u64().unwrap() + 1);
            self.state["next_class"] = json!(index + 1);
            self.state["manifest_bytes"] = json!(manifest.stream_position()?);
            self.publish(progress)?;
        }
        verify()?;
        self.state["manifest_sha256"] = json!(digest(&manifest_path)?);
        Ok(())
    }
    fn finish(&mut self, result: Result<()>, cancel: &AtomicBool, progress: Option<&Mutex<Value>>) {
        let complete = result.is_ok() && self.state["failed"] == 0;
        self.state["status"] = json!(if complete {
            "complete"
        } else if cancel.load(Ordering::Relaxed) {
            "cancelled"
        } else {
            "incomplete"
        });
        self.state["complete"] = json!(complete);
        self.state["error"] = result
            .err()
            .map(|e| json!(format!("{e:#}")))
            .unwrap_or_else(|| {
                if complete {
                    Value::Null
                } else {
                    json!("Some classes could not be exported; inspect manifest error records")
                }
            });
        if let Err(error) = self
            .publish(progress)
            .and_then(|()| save(&self.out.join("completion.json"), &self.state))
        {
            self.state["status"] = json!("incomplete");
            self.state["complete"] = json!(false);
            self.state["persistence_error"] = json!(format!("{error:#}"));
        }
        if let Some(progress) = progress {
            *progress.lock().unwrap() = self.state.clone();
        }
    }
}

pub(super) fn synchronous(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let hash = digest(engine.opened_path().context("No loaded project path")?)?;
    let mut session = Session::open(project, args, &hash)?;
    let result = session.run(engine, project, cancel, None, || Ok(()));
    session.finish(result, cancel, None);
    Ok(session.state)
}

/// Export directly from a CLI/library caller, with no GUI, socket or registration.
/// The same job worker and checkpoints serve both this entry point and MCP.
pub fn export_directory(
    input: &Path,
    output: &Path,
    package_prefix: &str,
    resume: bool,
    mut progress: impl FnMut(&Value),
) -> Result<Value> {
    ensure!(output.is_absolute(), "Output directory must be absolute");
    if !resume {
        discovery::check_export_parent(output)?;
    }
    let snapshot = ProjectSnapshot::open(input)?;
    let stop = Arc::new(AtomicBool::new(false));
    let loaded = LoadedProject::from_snapshot(snapshot, stop)?;
    check_project_file(&loaded)?;
    let args = json!({"out_path":output,"package_prefix":package_prefix,"resume":resume,"project_id":loaded.project_id});
    let started = loaded.exports.start(&loaded, &args)?;
    progress(&started);
    let id = required(&started, "job_id")?;
    loop {
        let state = loaded.exports.status(id, false)?;
        progress(&state);
        if state["worker_finished"] == true {
            return Ok(state);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (NativeEngine, Project, String) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        let mut engine = NativeEngine::default();
        let project = engine.open(&path).unwrap();
        (engine, project, digest(&path).unwrap())
    }
    #[test]
    fn cancelled_export_resumes_after_uncommitted_tail_and_orphan_source() {
        let (mut engine, project, hash) = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let args = json!({"out_path":tmp.path().join("export"),"package_prefix":"","instance_id":"i","project_id":"p"});
        validate("export_package_source", &args).unwrap();
        let mut session = Session::open(&project, &args, &hash).unwrap();
        let cancel = AtomicBool::new(false);
        let visits = AtomicUsize::new(0);
        let result = session.run(&mut engine, &project, &cancel, None, || {
            if visits.fetch_add(1, Ordering::Relaxed) == 1 {
                cancel.store(true, Ordering::Relaxed);
                bail!("CANCELLED");
            }
            Ok(())
        });
        session.finish(result, &cancel, None);
        assert_eq!(session.state["status"], "cancelled");
        assert_eq!(session.state["next_class"], 1);
        let out = session.out.clone();
        let first = fs::read(out.join("class-00000000.txt")).unwrap();
        let checkpoint = session.state["manifest_bytes"].as_u64().unwrap();
        let code = engine
            .decompile_class_source_report(&project.classes[1], &AtomicBool::new(false))
            .unwrap()
            .0;
        fs::write(out.join("class-00000001.txt"), &code.source).unwrap();
        OpenOptions::new()
            .append(true)
            .open(out.join("manifest.jsonl"))
            .unwrap()
            .write_all(b"{broken uncommitted tail")
            .unwrap();
        let mut resume = args.clone();
        resume["resume"] = json!(true);
        assert!(
            Session::open(&project, &resume, &hash)
                .err()
                .unwrap()
                .to_string()
                .contains("EXPORT_LOCKED")
        );
        drop(session);
        assert!(
            Session::open(&project, &resume, "wrong-hash")
                .err()
                .unwrap()
                .to_string()
                .contains("EXPORT_IDENTITY_MISMATCH")
        );
        let mut session = Session::open(&project, &resume, &hash).unwrap();
        let cancel = AtomicBool::new(false);
        let result = session.run(&mut engine, &project, &cancel, None, || Ok(()));
        session.finish(result, &cancel, None);
        assert_eq!(session.state["status"], "complete", "{}", session.state);
        assert_eq!(session.state["exported"], project.classes.len());
        assert_eq!(fs::read(out.join("class-00000000.txt")).unwrap(), first);
        let manifest = fs::read_to_string(out.join("manifest.jsonl")).unwrap();
        assert_eq!(manifest.lines().count(), project.classes.len());
        assert!(manifest.len() as u64 > checkpoint);
        assert_eq!(
            digest(&out.join("manifest.jsonl")).unwrap(),
            session.state["manifest_sha256"]
        );
        drop(session);
        assert!(
            Session::open(&project, &resume, &hash)
                .err()
                .unwrap()
                .to_string()
                .contains("EXPORT_ALREADY_COMPLETE")
        );
    }
    #[test]
    fn resume_rejects_modified_committed_source_and_records_terminal_error() {
        let (mut engine, project, hash) = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let args = json!({"out_path":tmp.path().join("export"),"package_prefix":"sample"});
        let mut session = Session::open(&project, &args, &hash).unwrap();
        let cancel = AtomicBool::new(false);
        let visits = AtomicUsize::new(0);
        let result = session.run(&mut engine, &project, &cancel, None, || {
            ensure!(
                visits.fetch_add(1, Ordering::Relaxed) < 1,
                "simulate interrupted export"
            );
            Ok(())
        });
        session.finish(result, &cancel, None);
        fs::write(session.out.join("class-00000000.txt"), "user content").unwrap();
        let out = session.out.clone();
        drop(session);
        let mut resume = args;
        resume["resume"] = json!(true);
        let result = synchronous(&mut engine, &project, &resume, &cancel).unwrap();
        assert_eq!(result["status"], "incomplete");
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("source file changed")
        );
        assert_eq!(
            fs::read_to_string(out.join("class-00000000.txt")).unwrap(),
            "user content"
        );
        let terminal: Value =
            serde_json::from_slice(&fs::read(out.join("completion.json")).unwrap()).unwrap();
        assert_eq!(terminal["status"], "incomplete");
    }
    #[test]
    fn background_job_releases_request_and_interactive_mutex() {
        let tmp = tempfile::tempdir().unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        let stop = Arc::new(AtomicBool::new(false));
        let loaded =
            LoadedProject::from_snapshot(ProjectSnapshot::open(&path).unwrap(), stop).unwrap();
        let _interactive = loaded.engine.lock().unwrap();
        let args = json!({"out_path":tmp.path().join("export"),"package_prefix":"sample","project_id":loaded.project_id,"instance_id":"i"});
        let request_cancel = AtomicBool::new(false);
        let started =
            project_request(&loaded, "export_package_source", &args, &request_cancel).unwrap();
        // Returning or timing out the initiating request must not cancel the job.
        request_cancel.store(true, Ordering::Relaxed);
        assert!(started["job_id"].is_string());
        let id = started["job_id"].as_str().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let result = project_request(
                &loaded,
                "get_export_status",
                &json!({"job_id":id}),
                &AtomicBool::new(false),
            )
            .unwrap();
            if result["worker_finished"] == true {
                assert_eq!(result["status"], "complete", "{result}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "export blocked on interactive mutex"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(loaded.exports.status("missing", false).is_err());
        loaded.exports.cancel_all();
        assert!(
            loaded
                .exports
                .start(&loaded, &args)
                .unwrap_err()
                .to_string()
                .contains("EXPORT_SERVICE_STOPPED")
        );
    }
    #[test]
    fn export_job_and_manifest_filters_roundtrip_over_service() {
        let tmp = tempfile::tempdir().unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        let server = Server::start_in(path, tmp.path().to_owned(), random_id().unwrap()).unwrap();
        let registration: Registration =
            serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while server.status().state != "Running" {
            assert_ne!(server.status().state, "Failed");
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let project = server.status().project_id;
        let args = json!({"instance_id":server.instance_id,"project_id":project,"out_path":tmp.path().join("export"),"package_prefix":"sample"});
        validate("export_package_source", &args).unwrap();
        let started = remote(&registration, "export_package_source", &args).unwrap();
        let poll = json!({"instance_id":server.instance_id,"project_id":project,"job_id":started["data"]["job_id"]});
        validate("get_export_status", &poll).unwrap();
        loop {
            let state = remote(&registration, "get_export_status", &poll).unwrap();
            if state["data"]["worker_finished"] == true {
                assert_eq!(state["data"]["status"], "complete", "{state}");
                break;
            }
            assert!(Instant::now() < deadline);
            assert_eq!(
                remote(&registration, "get_instance_info", &json!({})).unwrap()["state"],
                "Running"
            );
            thread::sleep(Duration::from_millis(10));
        }
        validate("cancel_export", &poll).unwrap();
        assert_eq!(
            remote(&registration, "cancel_export", &poll).unwrap()["data"]["cancel_requested"],
            true
        );
        for exported in ["true", "false", "unresolved"] {
            let args = json!({"instance_id":server.instance_id,"project_id":project,"exported_state":exported,"max_bytes":1024,"fields":"name,exported_state"});
            validate("get_manifest_components", &args).unwrap();
            let result = remote(&registration, "get_manifest_components", &args).unwrap();
            assert!(serde_json::to_vec(&result["data"]["items"]).unwrap().len() <= 1024);
            for record in result["data"]["items"].as_array().unwrap() {
                assert_eq!(
                    record["exported_state"]["effective"],
                    match exported {
                        "true" => json!(true),
                        "false" => json!(false),
                        _ => Value::Null,
                    }
                );
            }
        }
    }
}
