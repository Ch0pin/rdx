//! Local authenticated GUI service and stdio MCP gateway.
use crate::engine::{DecompilerEngine, NativeEngine, Project};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[path = "mcp_chooser.rs"]
mod chooser;
#[path = "mcp_discovery.rs"]
mod discovery;
#[path = "mcp_registry.rs"]
mod instance_registry;
#[path = "mcp_literals.rs"]
mod literals;
#[path = "mcp_navigation.rs"]
mod navigation;
#[path = "mcp_review.rs"]
mod review;
#[path = "mcp_scan.rs"]
mod scan;
#[path = "mcp_socket.rs"]
mod socket;
#[path = "mcp_source_search.rs"]
mod source_search;
#[path = "mcp_xml.rs"]
mod xml_discovery;

const MAX_FRAME: u64 = 8 * 1024 * 1024;

fn random_id() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("Random source: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn registry() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("HOME"))
        .context("Cannot locate user directory")?;
    let dir = PathBuf::from(base).join(".rdx/mcp");
    fs::create_dir_all(&dir)?;
    ensure!(
        !fs::symlink_metadata(&dir)?.file_type().is_symlink(),
        "Registry must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}
fn private_write(path: &Path, value: &Value) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    Ok(())
}
#[derive(Clone, Serialize, Deserialize)]
struct Registration {
    instance_id: String,
    address: SocketAddr,
    token: String,
    #[serde(default)]
    owner_pid: Option<u32>,
    #[serde(default)]
    apk_path: Option<PathBuf>,
    #[serde(default)]
    registered_at_unix_ms: Option<u64>,
}
#[derive(Default, Clone)]
pub struct Status {
    pub state: String,
    pub project_id: String,
    pub apk_sha256: String,
    pub package_selection: String,
    pub package_name: String,
    pub requests: u64,
    pub active: String,
    pub last_error: String,
    pub last_error_tool: String,
    pub last_error_class_id: String,
    pub active_requests: usize,
    pub literal_index: Value,
}
struct RequestWatch {
    stream: TcpStream,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    parsed: Arc<AtomicBool>,
    started: Instant,
}
struct LoadedProject {
    engine: Mutex<NativeEngine>,
    analysis_snapshot: Arc<NativeEngine>,
    project: Arc<Project>,
    path: PathBuf,
    hash: String,
    project_id: String,
    metadata: fs::Metadata,
    package_name: String,
    package_selection: String,
    literals: literals::Controller,
}
struct CompletedRequest(Arc<AtomicBool>);
impl Drop for CompletedRequest {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
struct WorkSlot {
    slots: Arc<AtomicUsize>,
    status: Arc<Mutex<Status>>,
}
impl Drop for WorkSlot {
    fn drop(&mut self) {
        self.slots.fetch_sub(1, Ordering::Relaxed);
        let mut status = self.status.lock().unwrap();
        status.active_requests = status.active_requests.saturating_sub(1);
        status.active = if status.active_requests == 0 {
            String::new()
        } else {
            format!("{} active requests", status.active_requests)
        };
    }
}
pub struct Server {
    pub instance_id: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<RequestWatch>>>,
    literals: Arc<Mutex<Option<literals::Controller>>>,
    status: Arc<Mutex<Status>>,
    registration: PathBuf,
}
impl Server {
    pub fn start(path: PathBuf) -> Result<Self> {
        let instance_id = std::env::var("RDX_MCP_INSTANCE")
            .ok()
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .unwrap_or(random_id()?);
        Self::start_in(path, registry()?, instance_id)
    }
    fn start_in(path: PathBuf, directory: PathBuf, instance_id: String) -> Result<Self> {
        let path = path.canonicalize().context("Cannot resolve APK path")?;
        let build = build_identity();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let registration = directory.join(format!("{instance_id}.json"));
        let entry = Registration {
            instance_id: instance_id.clone(),
            address: listener.local_addr()?,
            token: random_id()?,
            owner_pid: Some(std::process::id()),
            apk_path: Some(path.clone()),
            registered_at_unix_ms: Some(unix_ms()),
        };
        private_write(&registration, &serde_json::to_value(&entry)?)?;
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::<RequestWatch>::new()));
        let index = Arc::new(Mutex::new(None::<literals::Controller>));
        let status = Arc::new(Mutex::new(Status {
            state: "Loading".into(),
            ..Default::default()
        }));
        let (stopped, shared, watched, index_handle) = (
            stop.clone(),
            status.clone(),
            requests.clone(),
            index.clone(),
        );
        thread::spawn(move || {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let loading_path = path.clone();
            let loading_stop = stopped.clone();
            thread::spawn(move || {
                let loaded = (|| -> Result<_> {
                    let path = loading_path.canonicalize()?;
                    let hash = digest(&path)?;
                    let mut engine = NativeEngine::start()?;
                    let project = Arc::new(engine.open(&path)?);
                    ensure!(hash == digest(&path)?, "APK changed while opening");
                    let metadata = fs::metadata(&path)?;
                    let package_name = engine
                        .read_resource_with_metadata("AndroidManifest.xml")
                        .ok()
                        .and_then(|r| crate::manifest_summary::package_name(&r.source).ok())
                        .unwrap_or_default();
                    let package_selection = engine
                        .prepared_container()
                        .and_then(|source| source.selection_summary().map(str::to_owned))
                        .unwrap_or_default();
                    // NativeEngine clones share immutable DEX/resource/source documents.
                    let analysis_snapshot = Arc::new(engine.clone());
                    let literals = literals::Controller::new(
                        analysis_snapshot.clone(),
                        project.clone(),
                        loading_stop,
                    );
                    Ok(Arc::new(LoadedProject {
                        engine: Mutex::new(engine),
                        analysis_snapshot,
                        project,
                        path,
                        hash,
                        project_id: random_id()?,
                        metadata,
                        package_name,
                        package_selection,
                        literals,
                    }))
                })();
                let _ = tx.send(loaded);
            });
            let mut loaded: Option<Arc<LoadedProject>> = None;
            let slots = Arc::new(AtomicUsize::new(0));
            let entry = Arc::new(entry);
            while !stopped.load(Ordering::Relaxed) {
                if let Ok(result) = rx.try_recv() {
                    match result {
                        Ok(data) => {
                            let mut status = shared.lock().unwrap();
                            status.state = "Running".into();
                            status.package_name = data.package_name.clone();
                            status.project_id = data.project_id.clone();
                            status.apk_sha256 = data.hash.clone();
                            status.package_selection = data.package_selection.clone();
                            *index_handle.lock().unwrap() = Some(data.literals.clone());
                            loaded = Some(data);
                        }
                        Err(error) => {
                            let mut status = shared.lock().unwrap();
                            status.state = "Failed".into();
                            status.last_error = format!("{error:#}");
                        }
                    }
                }
                poll_requests(&watched);
                if let Some(project) = &loaded {
                    shared.lock().unwrap().literal_index = project.literals.status();
                }
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                // Parsing also runs outside the listener: a partial frame cannot
                // prevent health traffic from reaching its reserved fast path.
                if watched.lock().unwrap().len() >= 32 {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let peer = match stream.try_clone() {
                    Ok(peer) => peer,
                    Err(_) => continue,
                };
                let (cancel, done, parsed) = (
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicBool::new(false)),
                );
                watched.lock().unwrap().push(RequestWatch {
                    stream: peer,
                    cancel: cancel.clone(),
                    done: done.clone(),
                    parsed: parsed.clone(),
                    started: Instant::now(),
                });
                let (entry, loaded, status, stop, slots, build, path) = (
                    entry.clone(),
                    loaded.clone(),
                    shared.clone(),
                    stopped.clone(),
                    slots.clone(),
                    build.clone(),
                    path.clone(),
                );
                thread::spawn(move || {
                    let _completed = CompletedRequest(done);
                    let mut context = None;
                    let response = (|| -> Result<Value> {
                        configure_stream(&stream)?;
                        let request = read_frame(&mut BufReader::new(&mut stream))?
                            .context("Empty request")?;
                        ensure!(
                            request["token"].as_str() == Some(&entry.token),
                            "Unauthorized"
                        );
                        ensure!(
                            request["instance_id"].as_str() == Some(&entry.instance_id),
                            "INSTANCE_NOT_FOUND"
                        );
                        parsed.store(true, Ordering::Relaxed);
                        ensure!(
                            !stop.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed),
                            "CANCELLED"
                        );
                        let name = request["name"].as_str().context("Missing tool")?;
                        if name == "get_instance_info" {
                            let s = status.lock().unwrap();
                            return Ok(
                                json!({"instance_id":entry.instance_id,"project_id":s.project_id,"state":s.state,
                                "apk_path":path,"apk_sha256":s.apk_sha256,"package_selection":s.package_selection,"package_name":s.package_name,
                                "invoke_index":loaded.as_ref().map(|project|project.analysis_snapshot.invoke_index.status()),
                                "literal_index":loaded.as_ref().map(|project| project.literals.status()).unwrap_or_else(|| s.literal_index.clone()),"active_requests":s.active_requests,
                                "error":if s.state == "Failed" {s.last_error.as_str()} else {""},
                                "last_request_error":if s.last_error.is_empty() || s.state == "Failed" {Value::Null} else {json!({"message":s.last_error,"tool":s.last_error_tool,"requested_class_id":s.last_error_class_id})},"build":build}),
                            );
                        }
                        let project = loaded.as_ref().context("PROJECT_NOT_READY")?;
                        let args = &request["arguments"];
                        context = Some((
                            name.to_owned(),
                            args["class_id"]
                                .as_str()
                                .or_else(|| args["class_name"].as_str())
                                .unwrap_or("")
                                .to_owned(),
                        ));
                        ensure!(
                            args["project_id"].as_str() == Some(&project.project_id),
                            "PROJECT_CHANGED"
                        );
                        validate(name, args)?;
                        let mut active = slots.load(Ordering::Relaxed);
                        loop {
                            ensure!(
                                active < 8,
                                "INSTANCE_BUSY: 8 active operations; retry the query"
                            );
                            match slots.compare_exchange_weak(
                                active,
                                active + 1,
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                            ) {
                                Ok(_) => break,
                                Err(current) => active = current,
                            }
                        }
                        {
                            let mut s = status.lock().unwrap();
                            s.active_requests += 1;
                            s.active = name.into();
                            s.requests += 1;
                        }
                        let _slot = WorkSlot {
                            slots,
                            status: status.clone(),
                        };
                        let data = project_request(project, name, args, &cancel)?;
                        ensure!(
                            !stop.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed),
                            "CANCELLED"
                        );
                        Ok(
                            json!({"identity":{"instance_id":entry.instance_id,"project_id":project.project_id,"apk_path":project.path,
                            "apk_sha256":project.hash,"package_selection":project.package_selection},"data":data}),
                        )
                    })();
                    {
                        let mut status = status.lock().unwrap();
                        if let Err(error) = &response {
                            status.last_error = format!("{error:#}");
                            let (tool, class) = context.clone().unwrap_or_default();
                            status.last_error_tool = tool;
                            status.last_error_class_id = class;
                        } else if context.is_some() {
                            status.last_error.clear();
                            status.last_error_tool.clear();
                            status.last_error_class_id.clear();
                        }
                    }
                    let response = match response {
                        Ok(result) => json!({"result":result}),
                        Err(error) => json!({"error":format!("{error:#}")}),
                    };
                    if let Err(error) = write_response(&mut stream, &response) {
                        status.lock().unwrap().last_error = format!("Transport write: {error:#}");
                    }
                });
            }
            for request in watched.lock().unwrap().iter() {
                request.cancel.store(true, Ordering::Relaxed);
            }
            if let Some(project) = loaded {
                project.literals.cancel();
            }
        });
        Ok(Self {
            instance_id,
            stop,
            requests,
            literals: index,
            status,
            registration,
        })
    }
    pub fn cancel_request(&self) {
        for request in self.requests.lock().unwrap().iter() {
            request.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(index) = self.literals.lock().unwrap().as_ref() {
            index.cancel();
        }
    }
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for request in self.requests.lock().unwrap().iter() {
            request.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(index) = self.literals.lock().unwrap().as_ref() {
            index.cancel();
        }
        let _ = fs::remove_file(&self.registration);
    }
}
fn poll_requests(requests: &Mutex<Vec<RequestWatch>>) {
    requests.lock().unwrap().retain(|request| {
        if request.done.load(Ordering::Relaxed) {
            return false;
        }
        if request.started.elapsed() >= Duration::from_secs(120) {
            request.cancel.store(true, Ordering::Relaxed);
        }
        if request.parsed.load(Ordering::Relaxed) {
            // Never wait for an idle peer on the listener thread. Cloned
            // sockets retain blocking mode so response writes can finish.
            match socket::readable(&request.stream) {
                Ok(false) => return true,
                Ok(true) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => return true,
                Err(_) => {
                    request.cancel.store(true, Ordering::Relaxed);
                    return true;
                }
            }
            let mut byte = [0];
            match request.stream.peek(&mut byte) {
                Ok(0) => request.cancel.store(true, Ordering::Relaxed),
                Err(error)
                    if !matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    request.cancel.store(true, Ordering::Relaxed)
                }
                _ => {}
            }
        }
        true
    });
}

fn project_request(
    project: &LoadedProject,
    name: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    if matches!(
        name,
        "get_android_manifest"
            | "export_source"
            | "get_resource_file"
            | "get_manifest_summary"
            | "get_manifest_components"
            | "find_string_references"
            | "find_field_references"
            | "get_lambda_branch"
            | "resolve_switch_case_callers"
            | "find_network_stacks"
            | "find_config_key_consumers"
            | "find_instantiations"
            | "find_integer_references"
            | "find_annotation_usages"
            | "get_resource_value"
            | "find_type_references"
            | "search_kotlin_metadata"
            | "export_package_source"
            | "search_resource_xml"
            | "list_nav_deeplinks"
            | "find_layout_references"
            | "find_webview_chooser_flows"
            | "search_resources"
    ) {
        check_project_file(project)?;
    }
    if name == "find_string_references" {
        if args["search_mode"] == "streaming" {
            let result = scan::literals(&project.analysis_snapshot, &project.project, args, cancel);
            check_project_file(project)?;
            return result;
        }
        let result = project.literals.search(args, cancel);
        check_project_file(project)?;
        return result;
    }
    if name == "search_code" && args["mode"] == "identifiers" {
        check_project_file(project)?;
        let result = project.analysis_snapshot.search_identifiers(args, cancel)?;
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        check_project_file(project)?;
        return Ok(result);
    }
    // Immutable DEX queries bypass the interactive engine mutex entirely.
    if matches!(
        name,
        "get_call_graph"
            | "get_call_graph_batch"
            | "resolve_reference"
            | "find_field_references"
            | "get_lambda_branch"
            | "resolve_switch_case_callers"
            | "find_network_stacks"
            | "find_config_key_consumers"
            | "find_instantiations"
            | "find_integer_references"
            | "find_annotation_usages"
            | "get_resource_value"
            | "find_type_references"
            | "search_kotlin_metadata"
            | "export_package_source"
            | "search_resource_xml"
            | "list_nav_deeplinks"
            | "find_layout_references"
            | "find_webview_chooser_flows"
            | "search_resources"
            | "find_implementations"
            | "find_direct_subclasses"
            | "get_class_hierarchy"
    ) {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        check_project_file(project)?;
        let mut snapshot = project.analysis_snapshot.as_ref().clone();
        let result = dispatch(&mut snapshot, &project.project, name, args, cancel)?;
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        check_project_file(project)?;
        return Ok(result);
    }
    let mut engine = loop {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        match project.engine.try_lock() {
            Ok(engine) => break engine,
            Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(5)),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                bail!("ENGINE_UNAVAILABLE: restart the server")
            }
        }
    };
    if name == "export_source" {
        let result = review::export(
            &mut engine,
            &project.project,
            args,
            cancel,
            Some(json!({"apk_path":project.path,"apk_sha256":project.hash})),
        )?;
        check_project_file(project)?;
        return Ok(result);
    }
    if name == "get_manifest_components" {
        let xml = engine
            .read_resource_with_metadata("AndroidManifest.xml")?
            .source;
        let mut records =
            crate::manifest_summary::query_resolved(&xml, args, engine.resource_table(), true)
                .map_err(anyhow::Error::msg)?;
        crate::manifest_summary::project_records(&mut records, args);
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        check_project_file(project)?;
        let hash = crate::engine::source_identity(&serde_json::to_string(&records)?);
        let mut result = page(records, args)?;
        result["format"] = json!("records");
        result["source_hash"] = json!(hash);
        result["offset_unit"] = json!("matching component records");
        result["coverage"] = crate::manifest_summary::component_coverage(
            &crate::manifest_summary::resolved_xml(&xml, engine.resource_table())
                .map_err(anyhow::Error::msg)?
                .0,
        )
        .map_err(anyhow::Error::msg)?;
        result["coverage"]["excludes_non_exported"] = json!(false);
        result["coverage"]["filters"] = component_filters(args);
        result["coverage"]["matching_components"] = result["total"].clone();
        result["coverage"]["page_complete"] = json!(result["next_offset"].is_null());
        result["analysis"] = json!(
            "Decoded manifest declarations; exported state and intent filters describe configuration, not runtime authorization or callback reachability."
        );
        Ok(result)
    } else if name == "get_manifest_summary" {
        let xml = engine
            .read_resource_with_metadata("AndroidManifest.xml")?
            .source;
        let mut result = if manifest_filtered(args) {
            let mut records =
                crate::manifest_summary::query_resolved(&xml, args, engine.resource_table(), false)
                    .map_err(anyhow::Error::msg)?;
            crate::manifest_summary::project_records(&mut records, args);
            let hash = crate::engine::source_identity(&serde_json::to_string(&records)?);
            let mut result = page(records, args)?;
            result["format"] = json!("records");
            result["source_hash"] = json!(hash);
            result["offset_unit"] = json!("matching records");
            result
        } else {
            let mut summary = crate::manifest_summary::render(
                &xml,
                &project.path,
                &project.hash,
                engine.resource_table(),
            )
            .map_err(anyhow::Error::msg)?;
            if args["format"] == "exposure" {
                summary =
                    crate::manifest_summary::exposure(&xml, engine.resource_table(), &summary)
                        .map_err(anyhow::Error::msg)?;
            }
            text_page(
                summary,
                args,
                if args["format"] == "exposure" {
                    "exposure"
                } else {
                    "markdown"
                },
            )?
        };
        result["analysis"] = json!(
            "Manifest declarations only; exported does not establish runtime callback reachability or authorization."
        );
        result["coverage"] = crate::manifest_summary::component_coverage(
            &crate::manifest_summary::resolved_xml(&xml, engine.resource_table())
                .map_err(anyhow::Error::msg)?
                .0,
        )
        .map_err(anyhow::Error::msg)?;
        result["coverage"]["excludes_non_exported"] = json!(false);
        result["coverage"]["filters"] = component_filters(args);
        Ok(result)
    } else {
        dispatch(&mut engine, &project.project, name, args, cancel)
    }
}
fn component_filters(args: &Value) -> Value {
    let mut filters = serde_json::Map::new();
    for key in [
        "class_name",
        "match_mode",
        "component",
        "component_type",
        "permission",
        "query",
        "host",
        "scheme",
        "path",
    ] {
        if let Some(value) = args.get(key) {
            filters.insert(key.into(), value.clone());
        }
    }
    Value::Object(filters)
}

fn check_project_file(project: &LoadedProject) -> Result<()> {
    let identity = fs::metadata(&project.path).and_then(|current| {
        Ok(current.len() == project.metadata.len()
            && current.modified()? == project.metadata.modified()?)
    });
    match identity {
        Ok(true) => Ok(()),
        Ok(false) => {
            project.literals.invalidate();
            bail!("PROJECT_CHANGED: APK changed on disk; restart the server");
        }
        Err(error) => {
            project.literals.invalidate();
            bail!(
                "PROJECT_CHANGED: APK identity is unavailable on disk; restart the server: {error}"
            );
        }
    }
}

fn configure_stream(stream: &TcpStream) -> Result<()> {
    // BSD sockets can inherit the listener's nonblocking mode. Framed I/O uses
    // blocking reads/writes with deadlines, including responses larger than the
    // socket send buffer.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(())
}
fn read_frame(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut line = String::new();
    let n = reader.take(MAX_FRAME + 1).read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    ensure!(
        n as u64 <= MAX_FRAME && line.ends_with('\n'),
        "Invalid or oversized frame: received {n} bytes (limit {MAX_FRAME}), newline terminated: {}",
        line.ends_with('\n')
    );
    Ok(Some(serde_json::from_str(&line)?))
}
fn write_frame(writer: &mut impl Write, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() as u64 > MAX_FRAME {
        bail!("Response exceeds 8 MiB; request a smaller page");
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
/// Keep both GUI and stdio transports alive when a result is too large.
fn write_response(writer: &mut impl Write, value: &Value) -> Result<()> {
    if serde_json::to_vec(value)?.len() as u64 + 1 > MAX_FRAME {
        let message = "RESPONSE_TOO_LARGE: request a smaller page or narrower filter";
        let error = if value.get("jsonrpc").is_some() {
            json!({"jsonrpc":"2.0","id":value["id"],"result":{
                "content":[{"type":"text","text":message}],"isError":true}})
        } else {
            json!({"error":message})
        };
        return write_frame(writer, &error);
    }
    write_frame(writer, value)
}

fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("Missing {k}"))
}
fn page(items: Vec<Value>, args: &Value) -> Result<Value> {
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(100) as usize;
    ensure!((1..=500).contains(&limit), "limit must be 1..500");
    ensure!(offset <= items.len(), "Invalid offset");
    let end = offset.saturating_add(limit).min(items.len());
    Ok(
        json!({"items":items[offset..end],"total":items.len(),"next_offset":(end<items.len()).then_some(end)}),
    )
}
fn text_page(text: String, args: &Value, format: &str) -> Result<Value> {
    let start = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(32000) as usize;
    ensure!(
        (1..=128000).contains(&limit),
        "Text limit must be 1..128000 characters"
    );
    let total = text.chars().count();
    ensure!(start <= total, "Invalid offset");
    let end = start.saturating_add(limit).min(total);
    let hash = crate::engine::source_identity(&text);
    Ok(
        json!({"text":text.chars().skip(start).take(limit).collect::<String>(),"format":format,"source_hash":hash,"offset":start,"total_chars":total,"next_offset":(end<total).then_some(end),
            "returned_chars":end-start,"page_end":end,"has_more":end<total,"full_text_in_response":start==0 && end==total,
            "offset_unit":"Unicode scalar characters","pagination_note":if start!=0 || end<total {Some("This is a source slice. Follow next_offset with the same source_hash; absence from this page does not establish absence from the class.")} else {None}}),
    )
}
/// Pin executable identity before an on-disk application update can replace it.
pub fn capture_build_identity() {
    let _ = build_identity();
}
fn build_identity() -> Value {
    static BUILD: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    BUILD.get_or_init(|| {
        let executable = std::env::current_exe().ok();
        json!({"version": env!("CARGO_PKG_VERSION"), "pid":std::process::id(),
            "executable":executable,"executable_sha256":executable.as_deref().and_then(|p|digest(p).ok())})
    }).clone()
}

fn manifest_filtered(args: &Value) -> bool {
    args["format"] == "records"
        || [
            "component_type",
            "component",
            "host",
            "scheme",
            "path",
            "permission",
            "query",
        ]
        .iter()
        .any(|k| args.get(*k).is_some())
}
fn instance_matches(info: &Value, args: &Value) -> bool {
    if ["apk_sha256", "package_name"].iter().any(|key| {
        args[*key]
            .as_str()
            .is_some_and(|filter| info[*key].as_str() != Some(filter))
    }) {
        return false;
    }
    let Some(filter) = args["apk_path"].as_str() else {
        return true;
    };
    let Some(value) = info["apk_path"].as_str() else {
        return false;
    };
    let requested = Path::new(filter);
    let registered = Path::new(value);
    // Bare names retain the documented substring search. Paths use identity.
    if requested.components().count() == 1 && !requested.is_absolute() {
        return registered
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(filter));
    }
    if let (Ok(a), Ok(b)) = (requested.canonicalize(), registered.canonicalize()) {
        return a == b;
    }
    if requested.is_absolute() && registered.is_absolute() {
        return requested == registered;
    }
    // Legacy registrations may contain only a basename. Require an exact hash too.
    requested.is_absolute()
        && registered.components().count() == 1
        && requested.file_name() == registered.file_name()
        && args["apk_sha256"]
            .as_str()
            .is_some_and(|h| !h.is_empty() && info["apk_sha256"] == h)
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn discover(entries: &[Registration], args: &Value) -> Result<Value> {
    let start = args["registration_offset"].as_u64().unwrap_or(0) as usize;
    ensure!(start <= entries.len(), "Invalid registration_offset");
    let end = start.saturating_add(16).min(entries.len());
    let mut items = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in &entries[start..end] {
        match remote(entry, "get_instance_info", &json!({})) {
            Ok(info) => { let mut info = normalize_instance_info(info);
                info["reachability"] = json!("reachable");
                info["last_seen_unix_ms"] = json!(unix_ms());
                info["owner_pid"] = json!(entry.owner_pid);
                info["registered_at_unix_ms"] = json!(entry.registered_at_unix_ms);
                if instance_matches(&info,args) { items.push(info); } },
            Err(error) => diagnostics.push(json!({"instance_id":entry.instance_id,
                "status":"instance_unreachable", "error":format!("{error:#}"),
                "checked_at_unix_ms":unix_ms(),"last_seen_unix_ms":Value::Null,
                "owner_pid":entry.owner_pid,"apk_path":entry.apk_path,"registered_at_unix_ms":entry.registered_at_unix_ms,
                "recovery":{"tool":"open_apk","arguments":{"path":entry.apk_path,"request_key":format!("recovery-{}",entry.instance_id),"headless":true},"requires_explicit_call":true},
                "action":"Check the GUI MCP status and retry. If the GUI has closed, reopen it; an unreachable registration alone does not prove a stale process."})),
        }
    }
    let mut result = page(items, args)?;
    result["diagnostics"] = json!(diagnostics);
    result["discovery_complete"] = json!(end == entries.len());
    result["next_registration_offset"] = json!((end < entries.len()).then_some(end));
    result["registered_instances"] = json!(entries.len());
    let pinned = args["apk_sha256"].as_str().is_some_and(|s| !s.is_empty())
        || args["apk_path"]
            .as_str()
            .is_some_and(|s| Path::new(s).is_absolute());
    let candidates: Vec<_> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| {
            i["state"] == "Running" && i["project_id"].as_str().is_some_and(|s| !s.is_empty())
        })
        .map(|i| json!({"instance_id":i["instance_id"],"project_id":i["project_id"]}))
        .collect();
    result["selection"] = if pinned
        && start == 0
        && end == entries.len()
        && result["next_offset"].is_null()
        && args["offset"].as_u64().unwrap_or(0) == 0
        && candidates.len() == 1
    {
        json!({"status":"unique_healthy_match","selected":candidates[0],"routing":"Pass these explicit IDs on subsequent calls"})
    } else {
        json!({"status":if !pinned {"identity_filter_required"} else if candidates.len()>1 {"ambiguous"} else {"no_unique_complete_match"},"selected":Value::Null,"hint":"Filter by absolute apk_path or exact apk_sha256 and complete discovery; select an explicit instance/project when ambiguous"})
    };
    Ok(result)
}

fn normalize_instance_info(mut info: Value) -> Value {
    // Older GUI builds put a failed class selection into the project error.
    // Preserve the diagnosis while keeping a usable project's health explicit.
    if info["state"] == "Running"
        && let Some(message) = info["error"].as_str()
        && let Some(class_id) = message.strip_prefix("Unknown class: ")
    {
        info["selection_diagnostic"] = json!({
            "requested_class_id":class_id,"message":message,
            "source":"legacy_gui","action":"Reload the GUI with the current RDX build to reset its stale selection."
        });
        info["error"] = json!("");
    }
    info
}

fn search_code(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    if args["mode"] == "identifiers" {
        return engine.search_identifiers(args, cancel);
    }
    if args["pagination"] == "hits" {
        return source_search::search_hits(engine, project, args, cancel);
    }
    let query = required(args, "query")?;
    ensure!(
        !query.is_empty() && query.len() <= 4096,
        "Query must contain 1..4096 bytes"
    );
    let package = args["package_prefix"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.');
    let classes: Vec<_> = project
        .classes
        .iter()
        .filter(|c| {
            package.is_empty()
                || c.as_str() == package
                || c.strip_prefix(package)
                    .is_some_and(|tail| tail.starts_with('.'))
        })
        .collect();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(25) as usize;
    let source_offset = args["source_offset"].as_u64().unwrap_or(0) as usize;
    ensure!(
        (1..=500).contains(&limit) && offset <= classes.len(),
        "Invalid search page"
    );
    ensure!(
        offset < classes.len() || source_offset == 0,
        "Source offset without a class"
    );
    let end = offset.saturating_add(limit).min(classes.len());
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut scanned = 0;
    let mut next = (end < classes.len()).then_some(end);
    let mut next_source = 0;
    'classes: for (index, class) in classes.iter().enumerate().take(end).skip(offset) {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        scanned += 1;
        let code = match engine.decompile_search_cancellable(class, cancel) {
            Ok(code) => code,
            Err(error) => {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                errors.push(json!({"class_id":class,"error":format!("{error:#}")}));
                continue;
            }
        };
        let start_char = if index == offset { source_offset } else { 0 };
        let start_byte = if start_char == code.source.chars().count() {
            code.source.len()
        } else {
            code.source
                .char_indices()
                .nth(start_char)
                .map(|(i, _)| i)
                .context("Invalid source_offset")?
        };
        for matched in crate::resource_search::literal_ranges(&code.source, &code.links, query)
            .filter(|range| range.start >= start_byte)
        {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let byte = matched.start;
            let start = code.source[..byte].chars().count();
            let end = start + code.source[matched].chars().count();
            let line = code.source[..byte].bytes().filter(|b| *b == b'\n').count() + 1;
            let snippet: String = code.source[byte..]
                .chars()
                .take(200)
                .take_while(|c| *c != '\n')
                .collect();
            items.push(json!({"class_id":class,"source_hash":code.source_hash,"start":start,"end":end,"line":line,"snippet":snippet,"format":"java-or-mixed-dex"}));
            if items.len() == 100 {
                next = Some(index);
                next_source = end;
                break 'classes;
            }
        }
    }
    Ok(
        json!({"items":items,"scope":"generated Java and mixed DEX; case-sensitive literal plus exact numeric aliases for resolved resource links; resource files excluded","offset":offset,"scanned_classes":scanned,"total_classes":classes.len(),"page_status":source_search::page_status(items.is_empty(),next.is_some(),!errors.is_empty()),"errors":errors,"next_offset":next,"next_source_offset":next.map(|_|next_source),"exhausted":next.is_none()}),
    )
}

fn resolve_class_id<'a>(project: &'a Project, requested: &str) -> Result<&'a str> {
    let canonical = requested
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
        .map(|s| s.replace('/', "."));
    let requested = canonical.as_deref().unwrap_or(requested);
    if let Some(exact) = project.classes.iter().find(|c| c.as_str() == requested) {
        return Ok(exact);
    }
    if !requested.contains('.') {
        let mut matches = project
            .classes
            .iter()
            .filter(|c| c.rsplit('.').next() == Some(requested));
        if let Some(found) = matches.next() {
            ensure!(
                matches.next().is_none(),
                "AMBIGUOUS_CLASS: requested_class_id={requested}; use a fully qualified class_id"
            );
            return Ok(found);
        }
    }
    // Suggestions are declaration-backed and never change exact FQCN lookup.
    let simple = requested.rsplit('.').next().unwrap_or(requested);
    let candidates: std::collections::BTreeSet<_> = project
        .classes
        .iter()
        .filter(|c| {
            c.rsplit('.')
                .next()
                .is_some_and(|n| n.eq_ignore_ascii_case(simple))
        })
        .collect();
    let total = candidates.len();
    let mut bytes = 0;
    let suggestions: Vec<_> = candidates
        .into_iter()
        .take(20)
        .take_while(|id| {
            bytes += id.len();
            bytes <= 16384
        })
        .collect();
    let detail = json!({"candidates":suggestions,"candidate_count":total,
        "candidates_truncated":suggestions.len()<total,"scope":"loaded classes with the same simple name ignoring case","absence":"Requested identity is absent from the loaded DEX set; dynamic loading is one possible explanation",
        "hint":"Did you mean one of these classes? Retry with an exact candidate class_id; get_all_classes lists loaded definitions"});
    bail!("SYMBOL_NOT_FOUND: requested_class_id={requested}; {detail}")
}
// Hierarchy roots may be external, but only exact app-declared type edges qualify.
fn resolve_hierarchy_class(
    engine: &NativeEngine,
    project: &Project,
    requested: &str,
) -> Result<String> {
    let canonical = requested
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
        .map(|s| s.replace('/', "."));
    let name = canonical.as_deref().unwrap_or(requested);
    if project.classes.iter().any(|loaded| loaded == name)
        || engine.references_supertype(name)
        || engine.references_type(name)
    {
        return Ok(name.to_owned());
    }
    if !name.contains('.') && canonical.is_none() {
        let candidates = engine.hierarchy_name_candidates(name);
        if candidates.len() == 1 {
            return Ok(candidates[0].clone());
        }
        ensure!(
            candidates.is_empty(),
            "AMBIGUOUS_CLASS: {requested}; use an exact FQCN or descriptor; candidates={:?}",
            &candidates[..candidates.len().min(20)]
        );
    }
    resolve_class_id(project, name).map(str::to_owned).with_context(|| format!(
        "Hierarchy root unavailable: {requested}; framework definitions may be absent from the APK. Use the exact FQCN or DEX descriptor of a superclass/interface declared by an app class; inspect get_all_classes for loaded definitions"
    ))
}
fn dispatch(
    engine: &mut NativeEngine,
    project: &Project,
    name: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut normalized = args.clone();
    if class_scoped(name)
        && (name != "find_instantiations"
            || args.get("class_id").is_some()
            || args.get("class_name").is_some())
    {
        let requested = args["class_id"].as_str().or_else(|| args["class_name"].as_str())
            .context("Missing class selector: accepted parameters are class_id or class_name (FQCN or unique short name)")?;
        let hierarchy = matches!(
            name,
            "find_implementations" | "find_direct_subclasses" | "get_class_hierarchy"
        );
        let resolve = |selector: &str| -> Result<String> {
            if name == "find_instantiations" {
                crate::allocation_references::resolve_target(engine, project, selector, cancel)
            } else if hierarchy {
                resolve_hierarchy_class(engine, project, selector)
            } else {
                resolve_class_id(project, selector).map(str::to_owned)
            }
        };
        let resolved = resolve(requested)?;
        if let Some(alias) = args["class_name"].as_str() {
            ensure!(
                resolve(alias)? == resolved,
                "CONFLICTING_CLASS_SELECTORS: class_id and class_name resolve to different classes"
            );
        }
        normalized["class_id"] = json!(resolved);
    }
    let mut result = dispatch_resolved(engine, project, name, &normalized, cancel)?;
    if name == "get_call_graph" && args["include_const_args"] == true {
        review::call_arguments(engine, &mut result, cancel)?;
    }
    if name == "get_call_graph" {
        navigation::enrich(engine, &mut result, args, cancel)?;
    }
    if class_scoped(name) {
        result["class_id"] = normalized["class_id"].clone();
        result["requested_class_id"] = args
            .get("class_id")
            .or_else(|| args.get("class_name"))
            .cloned()
            .unwrap_or(Value::Null);
    }
    Ok(result)
}
fn class_scoped(name: &str) -> bool {
    matches!(
        name,
        "resolve_reference"
            | "get_class_source"
            | "get_class_disassembly"
            | "get_methods_of_class"
            | "get_fields_of_class"
            | "find_implementations"
            | "find_direct_subclasses"
            | "get_class_hierarchy"
            | "get_dex_strings"
            | "find_instantiations"
            | "resolve_switch_case_callers"
    )
}
fn method_source_page(text: String, args: &Value, format: &str) -> Result<Value> {
    if args.get("limit").is_some() {
        return text_page(text, args, format);
    }
    let total = text.chars().count();
    let start = args["offset"].as_u64().unwrap_or(0) as usize;
    ensure!(start <= total, "Invalid method-local offset");
    let hash = crate::engine::source_identity(&text);
    let result = json!({"text":text.chars().skip(start).collect::<String>(),"format":format,"source_hash":hash,"offset":start,"total_chars":total,"next_offset":Value::Null});
    ensure!(
        serde_json::to_vec(&result)?.len() < MAX_FRAME as usize - 65536,
        "METHOD_SOURCE_TOO_LARGE: use explicit limit and offset pagination"
    );
    Ok(result)
}
fn dispatch_resolved(
    engine: &mut NativeEngine,
    project: &Project,
    name: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let query = args["query"].as_str().unwrap_or("");
    if name == "get_strings" {
        ensure!(
            engine.resource_error().is_none(),
            "Resource table unavailable: {}",
            engine.resource_error().unwrap_or("")
        );
    }
    match name {
        "find_annotation_usages" => review::annotations(engine,project,args,cancel),
        "get_resource_value" => review::resources(engine,args,false,cancel),
        "search_resources" if args["scope"] == "assets" => scan::assets(engine,project,args,cancel),
        "search_resources" => review::resources(engine,args,true,cancel),
        "export_source" => review::export(engine,project,args,cancel,None),
        "find_integer_references" => crate::integer_references::search(engine, project, args, cancel),
        "find_instantiations" => {
            let allocating_class = args["allocating_class"].as_str().map(|n| resolve_class_id(project,n)).transpose()?;
            let allocating_method = args["allocating_method_id"].as_str().map(|id| review::member_selector(engine,project,&json!({"method_id":id}),true)).transpose()?;
            if let (Some(owner),Some(method)) = (allocating_class,allocating_method.as_deref()) {
                ensure!(crate::method_lookup::owner(method)? == owner,"Allocating method does not belong to allocating_class");
            }
            let mut result = crate::allocation_references::search_filtered(engine, project, args["class_id"].as_str(), allocating_class, allocating_method.as_deref(),
                args["offset"].as_u64().unwrap_or(0) as usize,args["limit"].as_u64().unwrap_or(100) as usize,cancel)?;
            result["allocating_class"]=json!(allocating_class);
            result["allocating_method_id"]=json!(allocating_method);
            Ok(result)
        },
        "get_class_hierarchy" => {
            let name = required(args,"class_id")?;
            let class = engine.dex_class(name);
            let entry = |descriptor: &str| {
                let id = crate::native_engine::disassembly::type_name(descriptor);
                json!({"class_id":id,"descriptor":descriptor,"definition_available":engine.dex_class(&id).is_some()})
            };
            Ok(json!({"descriptor":format!("L{};",name.replace('.',"/")),"definition_available":class.is_some(),
                "superclass":class.and_then(|c|c.superclass.as_deref()).map(entry),
                "interfaces":class.map(|c|c.interfaces.iter().map(|d|entry(d)).collect::<Vec<_>>()),
                "scope":"Immediate DEX declarations; missing external definitions have unknown parents/interfaces"}))
        },
        "find_config_key_consumers" => crate::config_consumers::search(engine,project,required(args,"key")?,args["offset"].as_u64().unwrap_or(0) as usize,args["limit"].as_u64().unwrap_or(20) as usize,cancel),
        "find_field_references" => crate::field_references::search(
            engine, project, &review::member_selector(engine,project,args,false)?,
            args["access"].as_str().unwrap_or("all"),
            args["offset"].as_u64().unwrap_or(0) as usize,
            args["limit"].as_u64().unwrap_or(100) as usize, cancel),
        "resolve_switch_case_callers" => crate::lambda_case_callers::search(engine, project, args, cancel),
        "find_network_stacks" => crate::network_stacks::search(engine, project, args, cancel),
        "get_lambda_branch" => {
            let classes: Vec<_> = project.classes.iter().filter_map(|name| engine.dex_class(name)).collect();
            ensure!(classes.len() == project.classes.len(), "INCOMPLETE_PROJECT: missing DEX classes");
            let result = crate::lambda_branch::analyze(&classes,
                required(args, "caller_method_id")?,
                args["call_site_offset"].as_u64().context("Missing call_site_offset (DEX code units)")? as usize,
                &|| cancel.load(Ordering::Relaxed));
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            Ok(result)
        },
        "get_call_graph" => engine.call_graph_query_by_id(required(args,"method_id")?,args,cancel),
        "get_call_graph_batch" => navigation::batch(engine,project,args,cancel),
        "resolve_reference" => navigation::resolve(engine,project,args,cancel),
        "get_method_source" => {
            let requested = review::member_selector(engine,project,args,true)?;
            let id = crate::method_lookup::normalize(&requested)?;
            let requested_format = crate::engine::MethodSourceFormat::parse(
                match args.get("format") {
                    None => "auto",
                    Some(value) => value.as_str().context("format must be a string")?,
                },
            )?;
            let report = engine.method_source(&id, requested_format, cancel)?;
            let mut result = method_source_page(report.code.source, args, report.format)?;
            result["method_id"] = json!(id);
            result["requested_method_id"] = json!(requested);
            result["offset_unit"] = json!("Unicode scalar characters relative to this method source");
            result["requested_format"] = json!(requested_format.as_str());
            result["reconstruction_attempted"] = json!(report.reconstruction_attempted);
            result["fallback_reason"] = json!(report.fallback_reason);
            result["scope"] = json!("Selected method; enclosing imports and generated class helpers are available from get_class_source");
            Ok(result)
        },
        "search_code" => search_code(engine, project, args, cancel),
        "get_all_classes" | "search_classes_by_keyword" => discovery::classes(project,args),
        "find_type_references" => discovery::types(engine,project,args,cancel),
        "search_kotlin_metadata" => discovery::metadata(engine,project,args,cancel),
        "export_package_source" => discovery::export_package(engine,project,args,cancel),
        "search_resource_xml" | "list_nav_deeplinks" | "find_layout_references" => xml_discovery::search(engine,project,name,args,cancel),
        "find_webview_chooser_flows" => chooser::search(engine,project,args,cancel),
        "get_class_source" => {
            let (code, coverage) = engine.decompile_class_source_report(required(args,"class_id")?, cancel)?;
            let mut result = text_page(code.source, args, "java-or-mixed-dex")?;
            result["coverage"] = coverage;
            Ok(result)
        },
        "get_class_disassembly"=> {let name=required(args,"class_id")?; let class=engine.dex_class(name).context("SYMBOL_NOT_FOUND")?; text_page(crate::native_engine::disassembly::render(name,class).source,args,"rdx-dex")},
        "get_methods_of_class"|"get_fields_of_class"=>{
            let owner=required(args,"class_id")?;
            let class=engine.dex_class(owner).context("SYMBOL_NOT_FOUND")?;
            metadata(class,owner,args,name=="get_fields_of_class")
        },
        "find_implementations"=> {
            let root = required(args,"class_id")?;
            let mut result = page(engine.implementation_names(root,cancel)?.into_iter().map(|c|json!({"class_id":c})).collect(),args)?;
            let mut external = std::collections::BTreeSet::new();
            for name in &project.classes {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                if let Some(class) = engine.dex_class(name) {
                    for parent in class.superclass.iter().chain(&class.interfaces) {
                        let name = crate::native_engine::disassembly::type_name(parent);
                        if name != "java.lang.Object" && engine.dex_class(&name).is_none() { external.insert(name); }
                    }
                }
            }
            result["root_class_id"] = json!(root);
            result["definition_available"] = json!(engine.dex_class(root).is_some());
            result["referenced_type"] = json!(engine.references_type(root));
            result["declared_hierarchy_edge"] = json!(engine.references_supertype(root));
            result["coverage"] = json!({"loaded_classes_scanned":project.classes.len(),"declared_edge_scan_complete":true,"external_ancestry_complete":external.is_empty(),"unavailable_parent_definitions":external.len(),"scope":"Known DEX superclass/interface edges. Zero results means zero known implementations; references in signatures or instructions alone do not establish inheritance. External and dynamically loaded ancestry may be unavailable."});
            Ok(result)
        },
        "find_direct_subclasses"=>page(engine.direct_subclass_names(required(args,"class_id")?).iter().map(|c|json!({"class_id":c})).collect(),args),
        "get_all_resource_file_names"=>page(project.resources.iter().filter(|r|r.contains(query)).map(|r|json!({"path":r})).collect(),args),
        "get_android_manifest"=>text_page(engine.read_resource_with_metadata("AndroidManifest.xml")?.source,args,"xml"),
        "get_resource_file"=> {let path=required(args,"resource_path")?; ensure!(project.resources.iter().any(|r|r==path),"RESOURCE_NOT_FOUND"); text_page(engine.read_resource_with_metadata(path)?.source,args,"resource")},
        "get_strings"=>page(engine.resource_table().entries.values().filter(|r|r.kind=="string").flat_map(|r|r.variants.iter().filter(move |v|r.name.contains(query)||v.value.contains(query)).map(move |v|json!({"id":format!("0x{:08x}",r.id),"name":r.name,"configuration":v.configuration,"value":v.value}))).collect(),args),
        "get_dex_strings"=>{let class=engine.dex_class(required(args,"class_id")?).context("SYMBOL_NOT_FOUND")?; page(class.symbols.strings.iter().enumerate().filter(|(_,s)|s.contains(query)).map(|(i,s)|json!({"string_index":i,"value":s})).collect(),args)},
        _=>bail!("Unknown tool: {name}"),
    }
}
fn metadata(
    class: &crate::native_dex::DexClass,
    _owner: &str,
    args: &Value,
    fields: bool,
) -> Result<Value> {
    if fields {
        page(
            class
                .fields
                .iter()
                .map(|f| json!({"field_id":format!("{}.{}:{}", _owner, f.name, f.field_type),"name":f.name.as_ref(),"type":f.field_type.as_ref(),"access_flags":f.access_flags}))
                .collect(),
            args,
        )
    } else {
        page(class.methods.iter().map(|m|json!({"method_id":crate::native_engine::disassembly::method_id(m),"name":m.name.as_ref(),"access_flags":m.access_flags})).collect(),args)
    }
}

fn registrations() -> Result<Vec<Registration>> {
    registrations_in(&registry()?)
}
fn registrations_in(directory: &Path) -> Result<Vec<Registration>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(directory)?.take(1024) {
        let path = entry?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        if let Ok(file) = File::open(&path)
            && file.metadata().is_ok_and(|m| m.len() <= 8192)
        {
            let mut bytes = Vec::new();
            if file.take(8193).read_to_end(&mut bytes).is_ok()
                && let Ok(record) = serde_json::from_slice::<Registration>(&bytes)
                && record.address.ip().is_loopback()
                && !instance_registry::prune_if_dead(&path, &bytes, record.owner_pid)
            {
                out.push(record);
            }
        }
    }
    out.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
    Ok(out)
}
fn forget_instance_in(directory: &Path, id: &str) -> Result<Value> {
    ensure!(
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Expected exact 64-character instance_id from discovery"
    );
    let path = directory.join(format!("{id}.json"));
    let metadata = fs::symlink_metadata(&path).context("INSTANCE_NOT_FOUND")?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= 8192,
        "Invalid registration file"
    );
    let bytes = fs::read(&path)?;
    let entry: Registration = serde_json::from_slice(&bytes)?;
    ensure!(
        entry.instance_id == id && entry.address.ip().is_loopback(),
        "Registration identity mismatch"
    );
    ensure!(
        remote(&entry, "get_instance_info", &json!({"instance_id":id})).is_err(),
        "INSTANCE_REACHABLE: keep the live registration; stop its MCP server in the GUI first"
    );
    instance_registry::forget(&path, &bytes)?;
    Ok(json!({"instance_id":id,"forgotten":true,"scope":"discovery registration only"}))
}
fn remote(entry: &Registration, name: &str, args: &Value) -> Result<Value> {
    remote_controlled(entry, name, args, None)
}

#[derive(Debug)]
struct InstanceToolUnavailable {
    tool: String,
    instance_id: String,
    gui_error: String,
}
impl std::fmt::Display for InstanceToolUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The gateway advertises {}, but GUI instance {} rejected it: {}. Check get_health for gateway and GUI build identities, then reopen the GUI and reconnect the MCP client using the same RDX build.",
            self.tool, self.instance_id, self.gui_error
        )
    }
}
impl std::error::Error for InstanceToolUnavailable {}

fn remote_controlled(
    entry: &Registration,
    name: &str,
    args: &Value,
    control: Option<&gateway::RequestControl>,
) -> Result<Value> {
    let mut stream = TcpStream::connect_timeout(&entry.address, Duration::from_millis(300))
        .context("INSTANCE_UNAVAILABLE: cannot connect to registered GUI; check GUI MCP status")?;
    stream.set_read_timeout(Some(Duration::from_secs(if name == "get_instance_info" {
        2
    } else {
        120
    })))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    if let Some(control) = control {
        control.attach(&stream)?;
    }
    write_frame(
        &mut stream,
        &json!({"token":entry.token,"instance_id":entry.instance_id,"name":name,"arguments":args}),
    )?;
    let reply = match control {
        Some(control) => control.read_response(stream),
        None => read_frame(&mut BufReader::new(stream)),
    }
    .context("INSTANCE_TRANSPORT_ERROR: GUI read failed; retry the read-only query")?
    .context("INSTANCE_DISCONNECTED: GUI closed the connection; rediscover instances")?;
    if let Some(error) = reply["error"].as_str() {
        if (error == "Unknown tool" || error == format!("Unknown tool: {name}"))
            && tools_list()["tools"]
                .as_array()
                .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == name))
        {
            return Err(InstanceToolUnavailable {
                tool: name.to_owned(),
                instance_id: entry.instance_id.clone(),
                gui_error: error.to_owned(),
            }
            .into());
        }
        bail!("{error}")
    }
    Ok(if name == "get_instance_info" {
        normalize_instance_info(reply["result"].clone())
    } else {
        reply["result"].clone()
    })
}
pub fn client_config() -> Result<String> {
    Ok(serde_json::to_string_pretty(
        &json!({"mcpServers":{"rdx":{"command":std::env::current_exe()?,"args":["mcp"]}}}),
    )?)
}
fn tools_list() -> Value {
    let definitions = [
        (
            "find_type_references",
            "Paged original DEX type operands and declaration types. Includes const-class, check-cast, instance-of, new-instance and array creation. Reuses identifier scanning; XML references use find_layout_references.",
            vec!["class_name"],
            true,
        ),
        (
            "search_kotlin_metadata",
            "Search kotlin.Metadata d2 evidence, preserving string-table index and owning DEX class. Matches are metadata strings, not verified original declaration aliases.",
            vec!["query"],
            true,
        ),
        (
            "search_resource_xml",
            "Search decoded XML resource lines across archive files, with stable occurrence paging and decode-error coverage. Case-sensitive by default.",
            vec!["query"],
            true,
        ),
        (
            "list_nav_deeplinks",
            "Inventory navigation XML deep links and Hotwire destination URI annotations. Includes destination arguments, graph includes, resource configurations, and bounded ordered class-array evidence. Candidate array indexes and conditional route strings are distinct from proven runtime registration.",
            vec![],
            true,
        ),
        (
            "find_layout_references",
            "Find declarative layout class references. Possible inflation sites, distinct from DEX allocations; returns archive path, element and class name.",
            vec!["class_name"],
            true,
        ),
        (
            "export_package_source",
            "Export all matching package classes to a NEW absolute directory with collision-safe numeric paths and a descriptor-to-path manifest.jsonl including source hashes and reconstruction coverage. Never overwrites; errors/cancellation retain explicitly incomplete manifest.",
            vec!["out_path", "package_prefix"],
            true,
        ),
        (
            "find_webview_chooser_flows",
            "Find static WebView chooser candidates and activity-result URI-to-ValueCallback flows. Returns DEX evidence, callback-field associations, validation calls and explicit unresolved interprocedural boundaries. Candidates require runtime validation.",
            vec![],
            true,
        ),
        (
            "find_annotation_usages",
            "Find explicit DEX annotation usages on classes, fields, methods and parameters, with decoded values and original IDs. annotation_type accepts FQCN or descriptor. Pagination counts matching annotations; parameter_index excludes receiver.",
            vec!["annotation_type"],
            true,
        ),
        (
            "get_resource_value",
            "Read decoded ARSC resource variants by resource_id (decimal/hex string) or resource_name ([package:]type/name). config filters exact stored configuration; no device best-match selection. Scalar references include resolution status.",
            vec![],
            true,
        ),
        (
            "search_resources",
            "Search decoded resource names and values across configurations. Pages count variants. Case-sensitive substring query; optional exact stored config.",
            vec!["query"],
            true,
        ),
        (
            "export_source",
            "Export complete class or method source plus coverage.json into a NEW absolute directory supplied as out_path. Returns paths, hashes, byte size and project identity. Never overwrites existing paths. Select class_name, method_id, or class_name plus method_name/descriptor. format=dex is available for method export.",
            vec!["out_path"],
            true,
        ),
        (
            "get_health",
            "Check this gateway and registered GUI connections, including executable identity and reconnect guidance. A closed client transport requires client-side reconnection before this tool can run.",
            vec![],
            false,
        ),
        (
            "find_string_references",
            "Search DEX literal owners without Java decompilation. Index memory overflow automatically uses a complete bounded streaming scan; search_mode=streaming bypasses construction. Includes const-string/jumbo instructions, encoded static values and annotation values. Excludes unreferenced strings and dynamically constructed values. Returns exact class/member IDs and code-unit offsets; literal previews are capped at 4096 characters. Pagination is by matching occurrences. The first query shares a cancellable background build; get_instance_info.literal_index reports progress. Failed or cancelled builds retry on the next query.",
            vec!["query"],
            true,
        ),
        (
            "find_field_references",
            "Find DEX iget/iput/sget/sput accesses by field_id or field (owner.name:descriptor or Lowner;->name:descriptor), or class_name plus field_name and optional descriptor. A subclass owner is resolved to its declaring field when provable; returns requested_field_id and resolved_field_id, preserving shadowing and unresolved status. Includes inherited operand aliases. access=all/read/write; returns method IDs, opcodes, read/write counts and code-unit offsets. Scans original DEX without decompilation; complete=false identifies unresolved relevant hierarchy accesses. No inference about reflection/native consumers. Pagination limit <=500.",
            vec!["field_id"],
            true,
        ),
        (
            "resolve_switch_case_callers",
            "Find known constructor call sites selecting a merged lambda discriminator case. Supply class_name, signed integer case, optional exact dispatcher method_id. Uses get_lambda_branch proofs; reports unresolved sites separately. limit counts candidate constructor sites (default 25, max 100); resume next_cursor as resume_cursor with unchanged selectors. Original DEX offsets and possible default-case selection remain explicit.",
            vec!["class_id", "case"],
            true,
        ),
        (
            "find_network_stacks",
            "Find OkHttp-compatible structural candidates across relocated/obfuscated namespaces. Returns linked request/response/URL/headers/chain roles, exact interceptor/proceed member selectors and an observer redaction policy. Static metadata only: no runtime values, hook installation, version certainty or runtime verification. Unsupported/inherited-only layouts may be missed; coverage and unresolved roles are explicit. package_prefix filters interceptor owners while retaining cross-package dependencies. offset/limit page candidates; max limit 100.",
            vec![],
            true,
        ),
        (
            "get_lambda_branch",
            "Resolve an R8 merged lambda constructor call site to its discriminator switch branches. Supply exact caller_method_id and call_site_offset in 16-bit DEX code units at invoke-direct. Returns proven discriminator, dispatcher method IDs, reachable DEX blocks including shared continuations and exception edges, and helper call IDs. Unsupported or ambiguous dataflow returns unresolved.",
            vec!["caller_method_id"],
            true,
        ),
        (
            "list_instances",
            "Discover GUI instances. Bare APK names use filename substring; full paths use canonical identity. Legacy basename registrations require apk_sha256 with a full-path query. Includes connection diagnostics. Discovery scans fixed batches of 16 registrations. First exhaust next_offset within a batch, then use next_registration_offset with offset=0. Diagnostics distinguish unreachable registrations; total counts matches in the current batch.",
            vec![],
            false,
        ),
        (
            "get_instance_info",
            "Read instance state, project identity and literal_index build state/progress. Available while literal indexing and other work slots are busy.",
            vec!["instance_id"],
            false,
        ),
        (
            "open_apk",
            "Open a local APK, XAPK, or APKS in a new RDX GUI instance, or headless=true in this gateway until its connection closes. Poll get_instance_info for readiness and pin the returned project_id. Required request_key identifies one launch within this gateway connection: reuse it to retry the same canonical path; use a new unique key for another window.",
            vec!["path", "request_key"],
            false,
        ),
        (
            "forget_instance",
            "Remove one unreachable instance registration by exact instance_id. Refuses reachable instances. Removes discovery metadata only; does not stop a GUI or delete project files. Use for legacy stale records without owner process metadata.",
            vec!["instance_id"],
            false,
        ),
        (
            "get_call_graph_batch",
            "Query 1..16 methods against a shared project-owned invoke index. Defaults to callers/call_sites/depth 1. Options apply to each method; per-method next_offset pages call sites and next_method_offset pages input methods if the response budget is reached. Per-method lookup errors are explicit. Cancellation stops the batch; get_instance_info.invoke_index reports build and active-batch progress.",
            vec!["method_ids"],
            true,
        ),
        (
            "resolve_reference",
            "Resolve linked source tokens to exact DEX target IDs. Requires class_id/class_name and source_hash from current source; source_offset is a Unicode scalar offset, or line/column are one-based. Line without column lists all references on that line. Optional method_id selects the method-local get_method_source document (format auto/dex). Includes verified static-forwarder metadata and separately labelled cross-namespace calls in small static helpers; never infers semantic ownership from package names.",
            vec!["class_id", "source_hash"],
            true,
        ),
        (
            "get_call_graph",
            "Query static DEX calls. Direction: callers, callees (default), or both. Accepts pkg.Class.method(desc)ret or Lpkg/Class;->method(desc)ret. Incomplete method selectors return bounded overload candidates; retry with an exact signature. direct_only selects depth 1. projection=call_sites defaults to depth 1 (explicit depth includes transitive sites) and returns paginated exact caller IDs/signatures and DEX code-unit offsets, raw invoke words/target, DEX entry and traversal depth without graph payload or graph-size caps (limit <=500). Optional package_prefix, exact class_name, class/method annotation and component_type filters select neighboring declarations after traversal. Caller queries reuse a project-owned disk invoke index; get_instance_info reports build progress. Optional include_source_locations adds method-local locations with explicit precision and reconstruction status; include_forwarders adds verified static delegates and separately labelled exact cross-namespace helper calls. Default graph has component-path highlights and a 5000-node/20000-edge cap.",
            vec!["method_id"],
            true,
        ),
        (
            "get_all_classes",
            "List class names and IDs, optionally filtered by query.",
            vec![],
            true,
        ),
        (
            "search_classes_by_keyword",
            "Search class names by literal substring (not method bodies).",
            vec!["query"],
            true,
        ),
        (
            "search_code",
            "mode=identifiers searches original DEX identifiers without decompilation. Method-reference queries use a direct per-DEX method-table filter without building the global index. search_mode=scan and compact projections return bounded resumable pages: pass next_cursor as resume_cursor with unchanged filters; total is null until coverage_complete. projection=summary returns counts and callers returns unique enclosing methods. Package-scoped queries stream only matching classes; global index overflow automatically uses a complete bounded streaming scan. coverage_complete describes requested-scope coverage independently of index_complete. Offset-based index/streaming pages use next_offset; resumable scan pages use next_cursor and expose progressive counts. query matches class/method/field names or full IDs, with exact/contains, case_sensitive, symbol_kind and occurrence filters. Full projection retains instruction evidence. Excludes generated names, locals, comments, strings and runtime dispatch. mode=source (default) searches generated Java and mixed DEX by case-sensitive literal substring. No resources. pagination=hits scans across classes until limit matches (default 100, max 500) or a 20-second between-class time budget; follow cursors until exhausted. Default pagination=classes scans 25 classes (max 500), at most 100 matches per response. Resume using next_offset AND next_source_offset with the same query/package_prefix; no-match is conclusive only after all pages with no errors.",
            vec!["query"],
            true,
        ),
        (
            "get_manifest_summary",
            "Read manifest Markdown (character offsets), or format=records for structured components and permissions (record offsets, limit <=500). Any filter selects records. component and permission are literal substrings; component_type is exact. host/scheme are exact declarations, path is a substring of declared path attributes. Route filters must match the same intent-filter. query is a case-insensitive substring. Declarations do not prove runtime reachability.",
            vec![],
            true,
        ),
        (
            "get_manifest_components",
            "Look up components in the decoded manifest. class_name uses match_mode=contains (default) or exact (relative/simple names normalized against manifest package). Includes matching aliases, declared/effective exported state and permissions, and original intent filters. component_type is exact; permission is a substring; query is case-insensitive. host/scheme are exact declarations and path is a declared-attribute substring, within one intent-filter. Paginated matching records, limit <=500.",
            vec![],
            true,
        ),
        (
            "get_method_source",
            "Read a method selected by class_name plus method_name and optional descriptor (ambiguous names return candidates), or an exact DEX method_id, accepting pkg.Class.method(desc)ret or Lpkg/Class;->method(desc)ret. Missing/wrong signatures return bounded same-name candidate IDs, with matching parameters ranked first; no candidate is selected automatically. Returns the complete method by default; optional limit/offset paginates Unicode characters with a stable hash. format=auto (default) attempts Java with explicit DEX fallback; format=dex directly returns native RDX DEX disassembly of this method. The response distinguishes requested disassembly from reconstruction failure. Includes pinned project/APK identity; disassembly is not assemblable Smali.",
            vec!["method_id"],
            true,
        ),
        (
            "get_class_source",
            "Read Java or mixed Java/DEX source with per-method decompiled/dex-fallback/omitted status and reasons. class_id or class_name accepts FQCN or unique short name. Follow next_offset until null; full_text_in_response distinguishes a whole document from a slice. Coverage describes the entire class, not just this page; decompiled status is not a proof of semantic equivalence.",
            vec!["class_id"],
            true,
        ),
        (
            "get_class_disassembly",
            "Read native RDX DEX disassembly, not assemblable Smali.",
            vec!["class_id"],
            true,
        ),
        (
            "get_methods_of_class",
            "List methods with exact DEX signatures.",
            vec!["class_id"],
            true,
        ),
        (
            "get_fields_of_class",
            "List fields and their types.",
            vec!["class_id"],
            true,
        ),
        (
            "find_integer_references",
            "Find exact DEX constant loads for 1..64 values in a single scan. values accepts signed decimal integers or decimal/hex strings; bit_width is 32 (default) or 64. Positive hex is a raw bit pattern, e.g. 0xffffffff equals -1 at 32 bits. Returns paginated method IDs, DEX files, instruction offsets, raw words and per-value totals. Decimal strings preserve 64-bit precision. Raw bits may encode integer, float, double or null values; no type/usage inference. Excludes computed values, arithmetic literals, field initializers and payloads. Supports cancellation.",
            vec!["values"],
            true,
        ),
        (
            "find_instantiations",
            "Find exact new-instance DEX instructions allocating class_id or class_name. For reverse lookup supply allocating_class or allocating_method_id to list the types constructed there; an optional allocated class selector intersects that scope. Accepts loaded or referenced FQCNs/descriptors and unique short names. Returns paginated owning method IDs, DEX files, allocation offsets, destination registers, raw type indices/instruction words and exact allocated descriptors. Scans all loaded classes without Java reconstruction. Counts allocation instructions, including unreachable code; arrays, reflection and native allocations are excluded. Construction success is not inferred.",
            vec!["class_id"],
            true,
        ),
        (
            "find_config_key_consumers",
            "Find exact literal config-key consumer candidates without Java reconstruction. Returns key/default retrieval calls, packaged default JSON, JSON-pointer array paths and DEX-proven String/regex uses through bounded static/direct helpers. Reader contract is inferred; dynamic keys, field flows and runtime overrides remain unknown. Pagination is by candidate method; limit <=100. Unresolved uses and partial analysis are explicit.",
            vec!["key"],
            true,
        ),
        (
            "get_class_hierarchy",
            "Read immediate superclass and interfaces with exact FQCNs, DEX descriptors and definition availability. Accepts FQCN, Lpackage/Class; or a unique short hierarchy name, including referenced external parents. Ambiguous names list candidates.",
            vec!["class_id"],
            true,
        ),
        (
            "find_implementations",
            "List concrete implementations, including transitive app-declared superclass/interface edges. class_id or class_name accepts loaded classes or FQCN/DEX descriptors or unique short names of referenced external supertypes; framework definitions need not be bundled.",
            vec!["class_id"],
            true,
        ),
        (
            "find_direct_subclasses",
            "List immediate superclass children from app DEX declarations, including abstract children. class_id or class_name accepts loaded classes or FQCN/DEX descriptors or unique short names of referenced external supertypes. Interface implementors use find_implementations.",
            vec!["class_id"],
            true,
        ),
        (
            "get_all_resource_file_names",
            "List Android package resource paths, including selected split paths.",
            vec![],
            true,
        ),
        (
            "get_android_manifest",
            "Read decoded AndroidManifest.xml.",
            vec![],
            true,
        ),
        (
            "get_resource_file",
            "Read a decoded text resource by archive path.",
            vec!["resource_path"],
            true,
        ),
        (
            "get_strings",
            "List resource string values and configurations.",
            vec![],
            true,
        ),
        (
            "get_dex_strings",
            "List strings in the DEX containing the supplied class.",
            vec!["class_id"],
            true,
        ),
    ];
    json!({"tools":definitions.into_iter().map(|(name,description,mut required,scoped)|{
        if scoped{required.extend(["instance_id","project_id"])}
        let mut properties=serde_json::Map::new();
        for key in &required{properties.insert((*key).into(),json!({"type":"string","minLength":1}));}
        if class_scoped(name) {
            required.retain(|key| *key != "class_id");
            properties.insert("class_name".into(),json!({"type":"string","minLength":1,"description":"Alias for class_id: exact FQCN, DEX descriptor or unique short name"}));
        }
        if name=="open_apk" {
            properties.insert("headless".into(),json!({"type":"boolean","default":false,"description":"Host the project in this MCP gateway without a GUI. Closing the gateway stops this service and cancels work. Poll get_instance_info for Running and project_id."}));
            properties.insert("request_key".into(),json!({"type":"string","minLength":1,"description":"Unique launch key within this gateway connection, e.g. review-temu-1. Reuse for retries of the same canonical path; a new key opens another window. Reusing it for a different path is rejected."}));
        }
        if ["get_method_source","find_field_references","export_source"].contains(&name) {
            let id = if name=="find_field_references" {"field_id"} else {"method_id"};
            let member = if name=="find_field_references" {"field_name"} else {"method_name"};
            required.retain(|k| *k!=id);
            for key in [id,"class_name",member,"descriptor"] { properties.insert(key.into(),json!({"type":"string","minLength":1})); }
        }
        if ["find_type_references","search_kotlin_metadata","find_webview_chooser_flows","find_network_stacks","list_nav_deeplinks"].contains(&name) { properties.insert("package_prefix".into(),json!({"type":"string"})); }
        if ["get_all_classes","search_classes_by_keyword","search_kotlin_metadata","search_resource_xml"].contains(&name) { properties.insert("case_sensitive".into(),json!({"type":"boolean","default":true})); }
        if ["search_resource_xml","list_nav_deeplinks","find_layout_references"].contains(&name) { properties.insert("resource_prefix".into(),json!({"type":"string"})); }
        if name=="find_annotation_usages" {
            properties.insert("element".into(),json!({"type":"string","enum":["all","class","field","method","parameter"],"default":"all"}));
            properties.insert("package_prefix".into(),json!({"type":"string"}));
        }
        if name=="search_resources" {
            properties.insert("scope".into(),json!({"type":"string","enum":["resource_table","assets"],"default":"resource_table","description":"assets searches UTF-8 asset contents and paths, one paginated record per matching file; skipped binary/oversized files are explicit"}));
            properties.insert("asset_prefix".into(),json!({"type":"string"}));
            properties.insert("case_sensitive".into(),json!({"type":"boolean","default":true}));
        }
        if ["get_resource_value","search_resources"].contains(&name) {
            properties.insert("config".into(),json!({"type":"string","minLength":1}));
            if name=="get_resource_value" {for key in ["resource_id","resource_name"] {properties.insert(key.into(),json!({"type":"string","minLength":1}));}}
        }
        if ["get_method_source","export_source"].contains(&name) {
            properties.insert("format".into(),json!({"type":"string","enum":["auto","dex"],"default":"auto","description":"auto attempts Java with explicit DEX fallback; dex requests native RDX DEX disassembly directly"}));
        }
        if name=="find_field_references" {
            properties.insert("field".into(),json!({"type":"string","minLength":1,"description":"Alias for field_id; accepts Lpkg/Class;->name:type and pkg.Class.name:type"}));
            properties.insert("access".into(),json!({"type":"string","enum":["all","read","write"],"default":"all"}));
        }
        if name=="resolve_switch_case_callers" {
            properties.insert("case".into(),json!({"type":"integer","minimum":-2147483648i64,"maximum":2147483647}));
            properties.insert("method_id".into(),json!({"type":"string","minLength":1}));
            properties.insert("resume_cursor".into(),json!({"type":"string","minLength":1}));
        }
        if name=="get_lambda_branch" {
            required.push("call_site_offset");
            properties.insert("call_site_offset".into(),json!({"type":"integer","minimum":0,"description":"Offset of invoke-direct in caller, in 16-bit DEX code units"}));
        }
        if (scoped && !["get_lambda_branch","export_source","export_package_source"].contains(&name))||["list_instances","get_health"].contains(&name){
            properties.insert("offset".into(),json!({"type":"integer","minimum":0}));
            properties.insert("limit".into(),json!({"type":"integer","minimum":1,"maximum":if ["get_method_source","get_class_source","get_class_disassembly","get_android_manifest","get_resource_file","get_manifest_summary"].contains(&name){128000}else{500}}));
        }
        if ["list_instances","get_health"].contains(&name) {
            properties.insert("registration_offset".into(),json!({"type":"integer","minimum":0}));
            for key in ["apk_path", "apk_sha256", "package_name"] { properties.insert(key.into(),json!({"type":"string","minLength":1})); }
        }
        if name=="resolve_switch_case_callers" { properties["limit"]["maximum"] = json!(100); properties.remove("offset"); }
        if name=="find_network_stacks" { properties["limit"]["maximum"] = json!(100); }
        if name=="find_instantiations" { for key in ["allocating_class","allocating_method_id"] { properties.insert(key.into(),json!({"type":"string","minLength":1})); } }
        if name=="find_integer_references" {
            properties.insert("values".into(),json!({"type":"array","minItems":1,"maxItems":64,"items":{"anyOf":[{"type":"integer"},{"type":"string","minLength":1,"maxLength":32}]},"description":"Signed decimals or decimal/0x hex strings. Hex patterns are interpreted at bit_width; duplicate normalized values are merged. Prefer strings for 64-bit precision."}));
            properties.insert("bit_width".into(),json!({"type":"integer","enum":[32,64],"minimum":32,"maximum":64,"default":32}));
            properties.insert("limit".into(),json!({"type":"integer","minimum":1,"maximum":500,"default":100}));
        }
        if name=="find_config_key_consumers" {
            properties.insert("limit".into(),json!({"type":"integer","minimum":1,"maximum":100,"default":20}));
        }
        if name=="get_manifest_summary" {
            for key in ["component", "host", "scheme", "path", "permission", "query"] { properties.insert(key.into(),json!({"type":"string","minLength":1})); }
            properties.insert("component_type".into(),json!({"type":"string","enum":["activity","activity-alias","service","receiver","provider"]}));
            properties.insert("format".into(),json!({"type":"string","enum":["markdown","records","exposure"]}));
        }
        if ["get_manifest_summary", "get_manifest_components"].contains(&name) {
            properties.insert("projection".into(),json!({"type":"string","enum":["full","compact"],"default":"full"}));
            properties.insert("fields".into(),json!({"type":"string","minLength":1,"description":"Comma-separated record field names; overrides projection. Validated against manifest record fields."}));
        }
        if name=="get_manifest_components" {
            for key in ["class_name", "host", "scheme", "path", "permission", "query"] { properties.insert(key.into(),json!({"type":"string","minLength":1})); }
            properties.insert("match_mode".into(),json!({"type":"string","enum":["exact","contains"],"default":"contains"}));
            properties.insert("component_type".into(),json!({"type":"string","enum":["activity","activity-alias","service","receiver","provider"]}));
        }
        if name=="find_string_references" {
            properties.insert("search_mode".into(),json!({"type":"string","enum":["auto","streaming"],"default":"auto","description":"auto builds/reuses an index and falls back to streaming on its memory limit; streaming scans directly with bounded retained results"}));
            properties.insert("match_mode".into(),json!({"type":"string","enum":["exact","contains"],"default":"contains"}));
            properties.insert("package_prefix".into(),json!({"type":"string"}));
        }
        if name=="search_code" {
            properties.insert("search_mode".into(),json!({"type":"string","enum":["auto","index","scan"],"default":"auto","description":"Identifiers only: auto directly scans method references; scan supports bounded cursor pages for every identifier kind; index explicitly requests the historical index path"}));
            properties.insert("projection".into(),json!({"type":"string","enum":["full","summary","callers"],"default":"full","description":"Identifiers only: summary returns cumulative counts; callers pages unique enclosing methods; full retains instruction evidence"}));
            properties.insert("resume_cursor".into(),json!({"type":"string","description":"Opaque next_cursor from a previous identifier scan. Keep query filters and projection unchanged; omit offset."}));
            properties.insert("mode".into(),json!({"type":"string","enum":["source","identifiers"],"default":"source"}));
            properties.insert("match_mode".into(),json!({"type":"string","enum":["exact","contains"],"default":"contains","description":"Identifier mode only"}));
            properties.insert("case_sensitive".into(),json!({"type":"boolean","default":true,"description":"Identifier mode only"}));
            properties.insert("symbol_kind".into(),json!({"type":"string","enum":["all","class","method","field"],"default":"all","description":"Identifier mode only"}));
            properties.insert("occurrence".into(),json!({"type":"string","enum":["all","declaration","reference"],"default":"all","description":"Identifier mode only"}));

            properties.insert("pagination".into(),json!({"type":"string","enum":["classes","hits"],"description":"Source mode defaults to classes; hits pages source occurrences across classes. Identifier mode defaults to hits and accepts only hits."}));
            properties.insert("package_prefix".into(),json!({"type":"string"}));
            properties.insert("source_offset".into(),json!({"type":"integer","minimum":0}));
        }
        if ["get_call_graph","get_call_graph_batch"].contains(&name) {
            properties.insert("dispatch_through_superclasses".into(),json!({"type":"boolean","default":false,"description":"Direct callers with call_sites only: include virtual invokes to overridden superclass declarations as possible dispatch; preserve raw target and separate counts"}));
            properties.insert("dispatch_through_interfaces".into(),json!({"type":"boolean","default":false,"description":"Direct callers with call_sites only: include interface-typed invokes as possible dispatch, preserving raw target and separate counts"}));
            for key in ["include_source_locations","include_forwarders"] {properties.insert(key.into(),json!({"type":"boolean","default":false,"description":"call_sites only: optional bounded source mapping or verified static-forwarder metadata"}));}
            properties.insert("include_const_args".into(),json!({"type":"boolean","default":false,"description":"call_sites only: conservative CFG constant argument analysis for the returned page; unresolved values are explicit"}));
            properties.insert("direct_only".into(),json!({"type":"boolean","default":false}));
            properties.insert("projection".into(),json!({"type":"string","enum":["graph","call_sites"],"default":"graph"}));
            for key in ["package_prefix", "class_name", "annotation"] { properties.insert(key.into(),json!({"type":"string","minLength":1})); }
            properties.insert("component_type".into(),json!({"type":"string","enum":["activity","service","receiver","provider"]}));
            properties.insert("depth".into(),json!({"type":"integer","minimum":1,"maximum":100,"description":"Defaults to 1 for call_sites and 20 for graph; explicit values include transitive sites"}));
            properties.insert("direction".into(),json!({"type":"string","enum":["callers","callees","both"],"default":"callees"}));
        }
        if name=="get_call_graph_batch" {
            properties.insert("method_ids".into(),json!({"type":"array","minItems":1,"maxItems":16,"items":{"type":"string","minLength":1}}));
            properties.insert("method_offset".into(),json!({"type":"integer","minimum":0,"maximum":16}));
            properties["projection"]["default"]=json!("call_sites");
            properties["direction"]["default"]=json!("callers");
        }
        if name=="resolve_reference" {
            for key in ["line","column"] {properties.insert(key.into(),json!({"type":"integer","minimum":1}));}
            properties.insert("source_offset".into(),json!({"type":"integer","minimum":0}));
            properties.insert("method_id".into(),json!({"type":"string","minLength":1}));
            properties.insert("format".into(),json!({"type":"string","enum":["auto","dex"]}));
            properties.remove("offset");properties.remove("limit");
        }
        if ["get_all_classes","get_all_resource_file_names","get_strings","get_dex_strings"].contains(&name){properties.insert("query".into(),json!({"type":"string"}));}
        let mut schema = json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
        if class_scoped(name) { schema["anyOf"] = json!([{"required":["class_id"]},{"required":["class_name"]}]); }
        if name=="find_instantiations" { schema["anyOf"]=json!([{"required":["class_id"]},{"required":["class_name"]},{"required":["allocating_class"]},{"required":["allocating_method_id"]}]); }
        if ["get_method_source","find_field_references"].contains(&name) {
            let (id,member)=if name=="get_method_source" {("method_id","method_name")}else{("field_id","field_name")};
            schema["anyOf"]=json!([{"required":[id]},{"required":["class_name",member]}]);
        }
        if name=="find_field_references" { schema["anyOf"].as_array_mut().unwrap().push(json!({"required":["field"]})); }
        if name=="export_source" {schema["anyOf"]=json!([{"required":["class_name"]},{"required":["method_id"]}]);}
        if name=="get_resource_value" {schema["anyOf"]=json!([{"required":["resource_id"]},{"required":["resource_name"]}]);}
        json!({"name":name,"description":description,"inputSchema":schema,"annotations":{"readOnlyHint":!["open_apk","forget_instance","export_source","export_package_source"].contains(&name),"destructiveHint":name=="forget_instance","openWorldHint":false}})
    }).collect::<Vec<_>>()})
}
#[derive(Debug)]
struct ArgumentError {
    message: String,
    details: Value,
}
impl std::fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ArgumentError {}

fn validate(name: &str, args: &Value) -> Result<()> {
    validate_arguments(name, args).map_err(|error| {
        if error.is::<ArgumentError>() {
            error
        } else {
            ArgumentError {
                message: format!("{error:#}"),
                details: json!({"tool":name,"accepted_parameters":tools_list()["tools"].as_array().and_then(|t|t.iter().find(|t|t["name"]==name)).map(|t|t["inputSchema"].clone()),"hint":"Use the named parameters and one required selector combination in accepted_parameters"}),
            }
            .into()
        }
    })
}

fn validate_arguments(name: &str, args: &Value) -> Result<()> {
    let tools = tools_list();
    let tool = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .context("Unknown tool")?;
    let schema = &tool["inputSchema"];
    let object = args.as_object().context("Arguments must be an object")?;
    for key in schema["required"].as_array().unwrap() {
        let key = key.as_str().unwrap();
        ensure!(object.contains_key(key), "Missing {key}");
    }
    if class_scoped(name) && name != "find_instantiations" {
        ensure!(
            object.contains_key("class_id") || object.contains_key("class_name"),
            "Missing class selector: accepted parameters are class_id or class_name (FQCN or unique short name)"
        );
    }
    if let Some(alternatives) = schema["anyOf"].as_array() {
        ensure!(
            alternatives.iter().any(|a| a["required"]
                .as_array()
                .unwrap()
                .iter()
                .all(|k| object.contains_key(k.as_str().unwrap()))),
            "Missing selector; accepted combinations: {}",
            schema["anyOf"]
        );
    }
    for (key, value) in object {
        let property = &schema["properties"][key];
        ensure!(!property.is_null(), "Unknown argument: {key}");
        if name == "get_call_graph_batch" && key == "method_ids" {
            let methods = value.as_array().context("method_ids must be an array")?;
            ensure!(
                (1..=16).contains(&methods.len())
                    && methods
                        .iter()
                        .all(|m| m.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 4096)),
                "method_ids must contain 1..16 nonempty method selectors of at most 4096 bytes"
            );
        } else if name == "find_integer_references" && key == "values" {
            ensure!(value.is_array(), "values must be an array");
        } else if name == "resolve_switch_case_callers" && key == "case" {
            ensure!(
                value.as_i64().is_some_and(|v| i32::try_from(v).is_ok()),
                "case must be a signed 32-bit integer"
            );
        } else if property["type"] == "string" {
            ensure!(value.is_string(), "{key} must be a string");
            if let Some(choices) = property["enum"].as_array() {
                ensure!(choices.contains(value), "Invalid {key}");
            }
            if property["minLength"] == 1 {
                ensure!(
                    !value.as_str().unwrap().is_empty(),
                    "{key} must not be empty"
                );
            }
        } else if property["type"] == "boolean" {
            ensure!(value.is_boolean(), "{key} must be a boolean");
        } else {
            let minimum = property["minimum"].as_u64().unwrap_or(0);
            let maximum = property["maximum"].as_u64().unwrap_or(usize::MAX as u64);
            if !value
                .as_u64()
                .is_some_and(|number| (minimum..=maximum).contains(&number))
            {
                return Err(ArgumentError {
                    message: format!(
                        "Invalid {key}: expected an integer from {minimum} to {maximum}"
                    ),
                    details: json!({"tool":name,"parameter":key,"received":value,
                        "expected_type":"integer","minimum":minimum,"maximum":maximum,
                        "hint":format!("Set {key} to an integer between {minimum} and {maximum}")}),
                }
                .into());
            }
        }
    }
    if name == "get_call_graph_batch" {
        let mut query = args.clone();
        let object = query.as_object_mut().unwrap();
        let methods = object.remove("method_ids").unwrap();
        object.remove("method_offset");
        ensure!(
            args["method_offset"].as_u64().unwrap_or(0) <= methods.as_array().unwrap().len() as u64,
            "method_offset exceeds method_ids length"
        );
        object.entry("projection").or_insert(json!("call_sites"));
        object.entry("direction").or_insert(json!("callers"));
        for method in methods.as_array().unwrap() {
            query["method_id"] = method.clone();
            validate("get_call_graph", &query)?;
        }
    }
    if name == "resolve_reference" {
        ensure!(
            args.get("source_offset").is_some() != args.get("line").is_some(),
            "Supply source_offset or line (optionally column)"
        );
        ensure!(
            args.get("column").is_none() || args.get("line").is_some(),
            "column requires line"
        );
        ensure!(
            args.get("format").is_none() || args.get("method_id").is_some(),
            "format requires method_id"
        );
    }
    if name == "get_call_graph"
        && (args["include_source_locations"] == true || args["include_forwarders"] == true)
    {
        ensure!(
            args["projection"] == "call_sites",
            "Source locations and forwarders require projection=call_sites"
        );
    }
    if name == "get_call_graph" && args["include_const_args"] == true {
        ensure!(
            args["projection"] == "call_sites",
            "include_const_args requires projection=call_sites"
        );
    }
    if name == "find_integer_references" {
        crate::integer_references::validate(args)?;
    }
    if name == "search_code" {
        if args["mode"] == "identifiers" {
            crate::identifier_search::validate(args)?;
        } else {
            ensure!(
                [
                    "match_mode",
                    "case_sensitive",
                    "symbol_kind",
                    "occurrence",
                    "projection",
                    "search_mode",
                    "resume_cursor"
                ]
                .iter()
                .all(|key| args.get(key).is_none()),
                "Identifier filters require mode=identifiers"
            );
        }
    }

    if ["get_manifest_summary", "get_manifest_components"].contains(&name) {
        if let Some(fields) = args["fields"].as_str() {
            let allowed = [
                "record_type",
                "component_type",
                "name",
                "target",
                "exported",
                "exported_state",
                "enabled",
                "launcher",
                "permission",
                "read_permission",
                "write_permission",
                "aliases",
                "details",
                "intent_filters",
                "matching_filter_indexes",
                "declaration",
                "attributes",
                "resource_resolution",
                "authorities",
            ];
            ensure!(
                fields
                    .split(',')
                    .all(|field| allowed.contains(&field.trim())),
                "Unknown manifest record field"
            );
        }
        if name == "get_manifest_summary" {
            ensure!(
                args["format"] != "exposure" || !manifest_filtered(args),
                "exposure format requires an unfiltered manifest query"
            );
            ensure!(
                (args.get("projection").is_none() && args.get("fields").is_none())
                    || manifest_filtered(args),
                "projection and fields require records format or a manifest filter"
            );
        }
    }
    if name == "get_manifest_summary" && manifest_filtered(args) {
        ensure!(
            args["limit"].as_u64().unwrap_or(100) <= 500,
            "Filtered manifest record limit must be <=500"
        );
    }
    if name == "get_call_graph" {
        ensure!(
            args["direct_only"] != true || args["depth"].as_u64().unwrap_or(1) == 1,
            "direct_only requires depth=1 when depth is supplied"
        );
        ensure!(
            args["projection"] == "call_sites"
                || (args.get("offset").is_none() && args.get("limit").is_none()),
            "offset and limit require projection=call_sites"
        );
    }
    Ok(())
}
struct OpenedInstance {
    path: PathBuf,
    id: String,
    // Own the headless service until the gateway closes; Drop cancels work and unregisters.
    server: Option<Server>,
}

fn open_apk_request(
    args: &Value,
    opened: &mut std::collections::HashMap<String, OpenedInstance>,
    directory: PathBuf,
) -> Result<Value> {
    let path = PathBuf::from(required(args, "path")?).canonicalize()?;
    ensure!(
        path.is_file()
            && path.extension().is_some_and(|e| {
                ["apk", "xapk", "apks"]
                    .iter()
                    .any(|format| e.eq_ignore_ascii_case(format))
            }),
        "Expected a local APK, XAPK, or APKS file"
    );
    let key = required(args, "request_key")?;
    let headless = args["headless"].as_bool().unwrap_or(false);
    if let Some(existing) = opened.get(key) {
        ensure!(
            existing.path == path && existing.server.is_some() == headless,
            "request_key belongs to a different package or launch mode"
        );
        return Ok(
            json!({"instance_id":existing.id,"state":"launching","apk_path":path,"headless":headless}),
        );
    }
    let id = random_id()?;
    if headless {
        let server = Server::start_in(path.clone(), directory, id.clone())?;
        opened.insert(
            key.into(),
            OpenedInstance {
                path: path.clone(),
                id: id.clone(),
                server: Some(server),
            },
        );
        return Ok(
            json!({"instance_id":id,"state":"launching","apk_path":path,"headless":true,"lifetime":"this MCP gateway connection","next_action":"Poll get_instance_info until Running, then pin its project_id"}),
        );
    }
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg(&path)
        .env("RDX_MCP_AUTO_START", "1")
        .env("RDX_MCP_INSTANCE", &id)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    opened.insert(
        key.into(),
        OpenedInstance {
            path: path.clone(),
            id: id.clone(),
            server: None,
        },
    );
    Ok(json!({"instance_id":id,"state":"launching","apk_path":path}))
}

fn gateway_call(
    name: &str,
    args: &Value,
    opened: &mut std::collections::HashMap<String, OpenedInstance>,
) -> Result<Value> {
    validate(name, args)?;
    if name == "forget_instance" {
        return forget_instance_in(&registry()?, required(args, "instance_id")?);
    }
    if name == "open_apk" {
        return open_apk_request(args, opened, registry()?);
    }
    let entries = registrations()?;
    if name == "list_instances" || name == "get_health" {
        let mut result = discover(&entries, args)?;
        if name == "get_health" {
            result["gateway"] = build_identity();
            result["transport"] = json!("connected");
            result["guidance"] = json!(
                "No registrations: enable MCP in the GUI or use open_apk. Unreachable registration: check the GUI and retry discovery, or explicitly forget_instance for stale discovery metadata. Transport closed before a tool response: reconnect/restart the MCP connector; a running GUI does not imply a running gateway. Each GUI request opens a fresh local connection."
            );
        }
        return Ok(result);
    }
    let id = required(args, "instance_id")?;
    let entry = entries
        .iter()
        .find(|r| r.instance_id == id)
        .context("INSTANCE_NOT_FOUND")?;
    remote(entry, name, args)
}
#[path = "mcp_gateway.rs"]
mod gateway;
/// Newline-delimited stdio MCP. May own explicitly opened headless services.
pub fn run_stdio() -> Result<()> {
    gateway::run()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_fqcn_suggests_loaded_exact_simple_names_without_selecting() {
        let project = Project {
            classes: vec![
                "com.baogong.app_login.LoginActivity".into(),
                "other.LoginActivity".into(),
                "other.LoginActivities".into(),
            ],
            resources: vec![],
        };
        let error = resolve_class_id(&project, "com.baogong.app_login.activity.LoginActivity")
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("SYMBOL_NOT_FOUND:"));
        assert!(error.contains(
            "\"candidates\":[\"com.baogong.app_login.LoginActivity\",\"other.LoginActivity\"]"
        ));
        assert!(error.contains("\"candidate_count\":2"));
        assert!(!error.contains("LoginActivities"));
        assert!(
            resolve_class_id(&project, "LoginActivity")
                .unwrap_err()
                .to_string()
                .contains("AMBIGUOUS_CLASS")
        );
        assert_eq!(
            resolve_class_id(&project, "Lcom/baogong/app_login/LoginActivity;").unwrap(),
            "com.baogong.app_login.LoginActivity"
        );
        let descriptor_error =
            resolve_class_id(&project, "Lcom/baogong/app_login/activity/LoginActivity;")
                .unwrap_err()
                .to_string();
        assert!(descriptor_error.contains("\"candidate_count\":2"));
        assert!(
            resolve_class_id(&project, "missing.Unknown")
                .unwrap_err()
                .to_string()
                .contains("\"candidates\":[]")
        );
    }

    #[test]
    fn class_suggestions_are_sorted_bounded_and_keep_exact_and_unique_lookup() {
        let mut project = Project {
            classes: (0..30)
                .rev()
                .map(|i| format!("package{i:02}.LoginActivity"))
                .collect(),
            resources: vec![],
        };
        let error = resolve_class_id(&project, "stale.LoginActivity")
            .unwrap_err()
            .to_string();
        let detail: Value = serde_json::from_str(error.split_once("; ").unwrap().1).unwrap();
        assert_eq!(detail["candidate_count"], 30);
        assert_eq!(detail["candidates_truncated"], true);
        assert_eq!(detail["candidates"].as_array().unwrap().len(), 20);
        assert_eq!(detail["candidates"][0], "package00.LoginActivity");
        assert_eq!(
            resolve_class_id(&project, "package29.LoginActivity").unwrap(),
            "package29.LoginActivity"
        );
        project.classes = vec!["package.LoginActivity".into()];
        assert_eq!(
            resolve_class_id(&project, "LoginActivity").unwrap(),
            "package.LoginActivity"
        );
        project.classes = vec![format!("{}.LoginActivity", "x".repeat(16384))];
        let error = resolve_class_id(&project, "stale.LoginActivity")
            .unwrap_err()
            .to_string();
        assert!(error.contains("\"candidates\":[]"));
        assert!(error.contains("\"candidates_truncated\":true"));
    }
    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("rdx-mcp-{}", random_id().unwrap()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }
    fn start_path(dir: &TestDir, path: PathBuf) -> (Server, Registration, Value) {
        let server = Server::start_in(path, dir.0.clone(), random_id().unwrap()).unwrap();
        let record: Registration =
            serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
        for _ in 0..6000 {
            let status = server.status();
            assert_ne!(status.state, "Failed", "{}", status.last_error);
            if status.state == "Running" {
                let args = json!({"instance_id":server.instance_id,"project_id":status.project_id});
                return (server, record, args);
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("Instance did not become ready")
    }
    fn start(dir: &TestDir, name: &str) -> (Server, Registration, Value) {
        start_path(dir, fixture(name))
    }

    #[test]
    fn code_search_hit_cursor_does_not_skip_the_rest_of_a_class() {
        let mut engine = NativeEngine::start().unwrap();
        let _project = engine.open(&fixture("navigation.apk")).unwrap();
        let source = engine
            .decompile_search_cancellable("sample.Caller", &AtomicBool::new(false))
            .unwrap();
        let expected: Vec<_> = source
            .source
            .match_indices(" ")
            .map(|(b, _)| source.source[..b].chars().count())
            .collect();
        assert!(expected.len() > 100);
        let project = Project {
            classes: vec!["sample.Caller".into()],
            resources: vec![],
        };
        let mut args = json!({"query":" "});
        let mut found = Vec::new();
        for _ in 0..100 {
            let page = search_code(&mut engine, &project, &args, &AtomicBool::new(false)).unwrap();
            found.extend(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|hit| hit["start"].as_u64().unwrap() as usize),
            );
            if page["exhausted"] == true {
                break;
            }
            args["offset"] = page["next_offset"].clone();
            args["source_offset"] = page["next_source_offset"].clone();
        }
        assert_eq!(found, expected);
        let absent = Project {
            classes: vec!["missing.Class".into()],
            resources: vec![],
        };
        let failure = search_code(
            &mut engine,
            &absent,
            &json!({"query":"needle"}),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(failure["errors"].as_array().unwrap().len(), 1);
        assert!(
            search_code(
                &mut engine,
                &project,
                &json!({"query":"return"}),
                &AtomicBool::new(true)
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "requires RDX_REVIEW_APK, RDX_REVIEW_METHOD, RDX_REVIEW_DECLARATION"]
    fn review_tools_match_a_local_project() {
        let dir = TestDir::new();
        let (_server, record, args) = start_path(
            &dir,
            PathBuf::from(std::env::var_os("RDX_REVIEW_APK").unwrap()),
        );
        let method = std::env::var("RDX_REVIEW_METHOD").unwrap();
        let declaration = std::env::var("RDX_REVIEW_DECLARATION").unwrap();
        let owner = crate::call_graph::owner(&method).unwrap();
        let mut graph_args = args.clone();
        graph_args["method_id"] = json!(method);
        graph_args["direction"] = json!("callers");
        graph_args["depth"] = json!(1);
        let graph = remote(&record, "get_call_graph", &graph_args).unwrap();
        assert!(
            graph["data"]["dispatch_targets"]
                .as_array()
                .unwrap()
                .contains(&json!(declaration))
        );
        let mut search_args = args.clone();
        search_args["query"] = json!("return");
        search_args["package_prefix"] = json!(owner);
        let search = remote(&record, "search_code", &search_args).unwrap();
        assert!(!search["data"]["items"].as_array().unwrap().is_empty());
        assert_eq!(search["data"]["errors"], json!([]));
        assert_eq!(search["data"]["exhausted"], true);
        let summary = remote(&record, "get_manifest_summary", &args).unwrap();
        assert_eq!(summary["data"]["format"], "markdown");
        let info = remote(&record, "get_instance_info", &json!({})).unwrap();
        assert!(!info["package_name"].as_str().unwrap().is_empty());
        assert!(instance_matches(
            &info,
            &json!({"package_name":info["package_name"],"apk_sha256":info["apk_sha256"]})
        ));
        assert_eq!(summary["identity"]["apk_sha256"], info["apk_sha256"]);
    }

    #[test]
    fn instance_filters_compose_without_cross_package_substrings() {
        let info =
            json!({"apk_path":"/tmp/one.apk","apk_sha256":"abc","package_name":"sample.app"});
        assert!(instance_matches(
            &info,
            &json!({"apk_path":"one.apk","package_name":"sample.app","apk_sha256":"abc"})
        ));
        assert!(!instance_matches(&info, &json!({"package_name":"sample"})));
        assert!(!instance_matches(&info, &json!({"apk_sha256":"different"})));
        assert!(!instance_matches(
            &json!({"state":"Loading"}),
            &json!({"package_name":"sample.app"})
        ));
    }

    #[test]
    fn code_search_resumes_and_manifest_summary_keeps_project_identity() {
        let dir = TestDir::new();
        let (_server, record, args) = start(&dir, "navigation.apk");
        let info = remote(&record, "get_instance_info", &json!({})).unwrap();
        let manifest = remote(&record, "get_android_manifest", &args).unwrap();
        let package =
            crate::manifest_summary::package_name(manifest["data"]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(info["package_name"], package);
        let summary = remote(&record, "get_manifest_summary", &args).unwrap();
        assert_eq!(summary["identity"]["project_id"], args["project_id"]);
        assert_eq!(summary["data"]["format"], "markdown");
        let text = summary["data"]["text"].as_str().unwrap();
        assert!(text.contains(info["apk_sha256"].as_str().unwrap()));
        assert!(text.contains("# Manifest summary"));
        let mut query = args.clone();
        query["query"] = json!("return");
        query["limit"] = json!(1);
        let mut found = Vec::new();
        for _ in 0..100 {
            let result = remote(&record, "search_code", &query).unwrap();
            assert_eq!(result["identity"]["project_id"], args["project_id"]);
            assert_eq!(result["data"]["errors"], json!([]));
            found.extend(result["data"]["items"].as_array().unwrap().iter().cloned());
            if result["data"]["exhausted"] == true {
                break;
            }
            query["offset"] = result["data"]["next_offset"].clone();
            query["source_offset"] = result["data"]["next_source_offset"].clone();
        }
        assert!(!found.is_empty());
        for hit in found {
            let mut source_args = args.clone();
            source_args["class_id"] = hit["class_id"].clone();
            let source = remote(&record, "get_class_source", &source_args).unwrap();
            assert_eq!(source["data"]["source_hash"], hit["source_hash"]);
            let actual: String = source["data"]["text"]
                .as_str()
                .unwrap()
                .chars()
                .skip(hit["start"].as_u64().unwrap() as usize)
                .take(6)
                .collect();
            assert_eq!(actual, "return");
        }
        query["project_id"] = json!("wrong");
        assert!(remote(&record, "search_code", &query).is_err());
        assert!(remote(&record, "get_manifest_summary", &query).is_err());
    }

    #[test]
    fn batch_call_graph_and_reference_resolution_dispatch_over_mcp() {
        let dir = TestDir::new();
        let (_server, record, args) = start(&dir, "navigation.apk");
        let mut batch = args.clone();
        batch["method_ids"] = json!(["sample.Target.doubleValue(I)I", "sample.Target.missing"]);
        batch["include_source_locations"] = json!(true);
        batch["include_forwarders"] = json!(true);
        validate("get_call_graph_batch", &batch).unwrap();
        let result = remote(&record, "get_call_graph_batch", &batch).unwrap();
        assert_eq!(result["data"]["complete"], true);
        assert_eq!(result["data"]["results"].as_array().unwrap().len(), 2);
        assert!(result["data"]["results"][1]["error"].is_string());
        let first = &result["data"]["results"][0]["data"];
        assert!(first["matching_total"].as_u64().unwrap() > 0);
        assert!(first["call_sites"][0]["source_mapping"]["status"].is_string());
        let info = remote(&record, "get_instance_info", &json!({})).unwrap();
        assert_eq!(info["invoke_index"]["state"], "ready");
        assert!(
            info["invoke_index"]["active_batches"]
                .as_object()
                .unwrap()
                .is_empty()
        );
        let mut source = args.clone();
        source["class_name"] = json!("sample.Caller");
        let document = remote(&record, "get_class_source", &source).unwrap();
        let text = document["data"]["text"].as_str().unwrap();
        let byte = text.find("doubleValue(").unwrap();
        let offset = text[..byte].chars().count();
        source["source_hash"] = document["data"]["source_hash"].clone();
        source["source_offset"] = json!(offset);
        validate("resolve_reference", &source).unwrap();
        let resolved = remote(&record, "resolve_reference", &source).unwrap();
        assert_eq!(
            resolved["data"]["references"][0]["target_id"],
            "sample.Target.doubleValue(I)I"
        );
        source["source_hash"] = json!("stale");
        assert!(
            remote(&record, "resolve_reference", &source)
                .unwrap_err()
                .to_string()
                .contains("SOURCE_CHANGED")
        );
        batch["method_ids"] = json!([]);
        assert!(validate("get_call_graph_batch", &batch).is_err());
    }

    #[test]
    fn identifier_search_contract_cache_and_project_identity() {
        let dir = TestDir::new();
        let (_server, record, mut args) = start(&dir, "navigation.apk");
        args["mode"] = json!("identifiers");
        args["query"] = json!("Target");
        args["symbol_kind"] = json!("class");
        args["match_mode"] = json!("exact");
        args["limit"] = json!(1);
        validate("search_code", &args).unwrap();
        let first = remote(&record, "search_code", &args).unwrap();
        assert_eq!(first["identity"]["project_id"], args["project_id"]);
        assert_eq!(first["data"]["index_complete"], true);
        assert_eq!(first["data"]["index_cached"], false);
        assert!(first["data"]["total"].as_u64().unwrap() > 0);
        let repeated = remote(&record, "search_code", &args).unwrap();
        assert_eq!(repeated["data"]["index_cached"], true);
        assert_eq!(first["data"]["items"], repeated["data"]["items"]);
        assert_eq!(first["data"]["index_hash"], repeated["data"]["index_hash"]);
        args["package_prefix"] = json!("sample");
        let scoped = remote(&record, "search_code", &args).unwrap();
        assert_eq!(scoped["data"]["execution_mode"], "streaming");
        assert_eq!(scoped["data"]["coverage_complete"], true);
        assert_eq!(scoped["data"]["index_complete"], false);
        assert_eq!(scoped["data"]["skipped_classes"], 0);
        assert!(scoped["data"]["total"].as_u64().unwrap() > 0);
        let scoped_again = remote(&record, "search_code", &args).unwrap();
        assert_eq!(scoped["data"]["items"], scoped_again["data"]["items"]);
        assert_eq!(
            scoped["data"]["source_hash"],
            scoped_again["data"]["source_hash"]
        );
        args.as_object_mut().unwrap().remove("package_prefix");
        args["pagination"] = json!("classes");
        assert!(validate("search_code", &args).is_err());
        args.as_object_mut().unwrap().remove("pagination");
        args["mode"] = json!("source");
        assert!(validate("search_code", &args).is_err());
        args["mode"] = json!("identifiers");
        args["project_id"] = json!("wrong");
        assert!(remote(&record, "search_code", &args).is_err());
    }

    #[test]
    fn integer_tool_validates_batches_and_dispatches_scoped_scans() {
        let dir = TestDir::new();
        let (_server, record, mut args) = start(&dir, "navigation.apk");
        args["values"] = json!([0, 1, 2, 3, 42, "0xffffffff"]);
        args["limit"] = json!(1);
        validate("find_integer_references", &args).unwrap();
        let first = remote(&record, "find_integer_references", &args).unwrap();
        assert_eq!(first["identity"]["project_id"], args["project_id"]);
        assert_eq!(first["data"]["scan_complete"], true);
        assert!(first["data"]["total"].as_u64().unwrap() > 0);
        assert_eq!(first["data"]["items"].as_array().unwrap().len(), 1);
        assert!(
            first["data"]["items"][0]["dex_entry"]
                .as_str()
                .unwrap()
                .ends_with(".dex")
        );
        args["offset"] = first["data"]["total"].clone();
        assert_eq!(
            remote(&record, "find_integer_references", &args).unwrap()["data"]["items"],
            json!([])
        );
        args["bit_width"] = json!(33);
        assert!(validate("find_integer_references", &args).is_err());
        args["bit_width"] = json!(32);
        args["values"] = json!([true]);
        assert!(validate("find_integer_references", &args).is_err());
        args["values"] = json!([1]);
        args["offset"] = json!(0);
        args["project_id"] = json!("wrong");
        assert!(remote(&record, "find_integer_references", &args).is_err());
    }

    #[test]
    fn allocation_tool_selectors_scope_and_pagination_are_dispatched() {
        let dir = TestDir::new();
        let (_server, record, mut args) = start(&dir, "navigation.apk");
        args["class_name"] = json!("Target");
        args["limit"] = json!(1);
        validate("find_instantiations", &args).unwrap();
        let simple = remote(&record, "find_instantiations", &args).unwrap();
        assert_eq!(simple["data"]["class_id"], "sample.Target");
        assert_eq!(simple["data"]["scan_complete"], true);
        args["class_name"] = json!("Lsample/Target;");
        let exact = remote(&record, "find_instantiations", &args).unwrap();
        assert_eq!(exact["data"]["items"], simple["data"]["items"]);
        args["class_name"] = json!("Ljava/lang/Object;");
        assert_eq!(
            remote(&record, "find_instantiations", &args).unwrap()["data"]["definition_available"],
            false
        );
        args["class_id"] = json!("sample.Target");
        assert!(remote(&record, "find_instantiations", &args).is_err());
        args.as_object_mut().unwrap().remove("class_id");
        args["class_name"] = json!("missing.Type");
        assert!(remote(&record, "find_instantiations", &args).is_err());
        args["class_name"] = json!("Target");
        args["project_id"] = json!("wrong-project");
        assert!(remote(&record, "find_instantiations", &args).is_err());
    }

    #[test]
    fn implementation_tool_is_scoped_paginated_and_dispatched() {
        let dir = TestDir::new();
        let (_server, record, mut args) = start(&dir, "navigation.apk");
        args["class_id"] = json!("sample.Target");
        args["limit"] = json!(1);
        assert!(validate("find_implementations", &args).is_ok());
        assert!(validate("find_implementations", &json!({"class_id":"sample.Target"})).is_err());
        let result = remote(&record, "find_implementations", &args).unwrap();
        assert_eq!(result["data"]["total"], 0);
        args["class_id"] = json!("missing.Type");
        assert!(remote(&record, "find_implementations", &args).is_err());
    }

    #[test]
    fn immutable_hierarchy_requests_complete_while_interactive_engine_is_locked() {
        let path = fixture("navigation.apk");
        let mut engine = NativeEngine::default();
        let project = Arc::new(engine.open(&path).unwrap());
        let analysis_snapshot = Arc::new(engine.clone());
        let loaded = Arc::new(LoadedProject {
            engine: Mutex::new(engine),
            analysis_snapshot: analysis_snapshot.clone(),
            project: project.clone(),
            hash: digest(&path).unwrap(),
            metadata: fs::metadata(&path).unwrap(),
            path,
            project_id: "snapshot-test".into(),
            package_name: String::new(),
            package_selection: String::new(),
            literals: literals::Controller::new(
                analysis_snapshot,
                project,
                Arc::new(AtomicBool::new(false)),
            ),
        });
        let guard = loaded.engine.lock().unwrap();
        let worker_project = loaded.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let cancel = AtomicBool::new(false);
            let args = json!({"class_name":"Ljava/lang/Object;"});
            let result = project_request(&worker_project, "find_implementations", &args, &cancel)
                .and_then(|result| {
                    ensure!(
                        result["total"].as_u64().unwrap() > 0,
                        "Expected descendants"
                    );
                    project_request(&worker_project, "find_direct_subclasses", &args, &cancel)
                })
                .and_then(|_| {
                    project_request(&worker_project, "find_instantiations", &args, &cancel)
                })
                .and_then(|_| {
                    project_request(
                        &worker_project,
                        "find_integer_references",
                        &json!({"values":[1]}),
                        &cancel,
                    )
                })
                .and_then(|prior| {
                    let indexed = project_request(
                        &worker_project,
                        "search_code",
                        &json!({"mode":"identifiers","query":"Target"}),
                        &cancel,
                    )?;
                    ensure!(
                        indexed["index_complete"] == true,
                        "Expected complete identifier index"
                    );
                    project_request(&worker_project, "get_call_graph_batch", &json!({"method_ids":["sample.Target.doubleValue(I)I"]}), &cancel)?;
                    let graph = project_request(&worker_project, "get_call_graph", &json!({"method_id":"sample.Target.doubleValue(I)I", "direction":"callers", "projection":"call_sites"}), &cancel)?;
                    ensure!(graph["invoke_index_cached"] == true, "Expected shared invoke index");
                    Ok(prior)
                });
            let _ = tx.send(result);
        });
        let result = rx.recv_timeout(Duration::from_secs(5));
        drop(guard);
        worker.join().unwrap();
        assert_eq!(result.unwrap().unwrap()["scan_complete"], true);
        let cancel = AtomicBool::new(true);
        assert!(
            project_request(
                &loaded,
                "find_implementations",
                &json!({"class_name":"java.lang.Object"}),
                &cancel
            )
            .is_err()
        );
    }

    #[test]
    fn external_hierarchy_selectors_normalize_and_errors_leave_server_responsive() {
        let dir = TestDir::new();
        let (_server, record, mut args) = start(&dir, "navigation.apk");
        args["class_name"] = json!("java.lang.Object");
        let fqcn = remote(&record, "find_implementations", &args).unwrap();
        assert!(fqcn["data"]["total"].as_u64().unwrap() > 0);
        args["class_name"] = json!("Ljava/lang/Object;");
        let descriptor = remote(&record, "find_implementations", &args).unwrap();
        assert_eq!(fqcn["data"]["items"], descriptor["data"]["items"]);
        assert_eq!(descriptor["data"]["class_id"], "java.lang.Object");
        args["class_name"] = json!("Object");
        let simple = remote(&record, "find_implementations", &args).unwrap();
        assert_eq!(simple["data"]["items"], fqcn["data"]["items"]);
        let hierarchy = remote(&record, "get_class_hierarchy", &args).unwrap();
        assert_eq!(hierarchy["data"]["descriptor"], "Ljava/lang/Object;");
        assert_eq!(hierarchy["data"]["definition_available"], false);
        args["class_name"] = json!("Lsample/Target;");
        let target = remote(&record, "get_class_hierarchy", &args).unwrap();
        assert_eq!(
            target["data"]["superclass"]["descriptor"],
            "Ljava/lang/Object;"
        );
        assert!(remote(&record, "get_class_source", &args).is_ok());
        args["class_name"] = json!("Ljava/lang/Object;");
        args["class_id"] = json!("java.lang.Object");
        assert!(
            remote(&record, "find_direct_subclasses", &args).unwrap()["data"]["total"]
                .as_u64()
                .unwrap()
                > 0
        );
        args["class_name"] = json!("sample.Target");
        assert!(
            remote(&record, "find_implementations", &args)
                .unwrap_err()
                .to_string()
                .contains("CONFLICTING_CLASS_SELECTORS")
        );
        args.as_object_mut().unwrap().remove("class_name");
        args["class_id"] = json!("Lsample/Target;");
        assert!(remote(&record, "find_implementations", &args).is_ok());
        for _ in 0..3 {
            args["class_id"] = json!("android.webkit.Missing");
            let error = remote(&record, "find_implementations", &args).unwrap_err();
            assert!(format!("{error:#}").contains("framework definitions may be absent"));
        }
        assert!(remote(&record, "get_instance_info", &json!({})).is_ok());
    }

    #[test]
    #[ignore = "requires RDX_IMPLEMENTATIONS_APK, RDX_IMPLEMENTATIONS_CLASS, RDX_IMPLEMENTATIONS_EXPECTED"]
    fn implementations_match_local_apk_inventory_over_mcp() {
        let dir = TestDir::new();
        let path = PathBuf::from(std::env::var_os("RDX_IMPLEMENTATIONS_APK").unwrap());
        let (_server, record, mut args) = start_path(&dir, path);
        args["class_id"] = json!(std::env::var("RDX_IMPLEMENTATIONS_CLASS").unwrap());
        args["limit"] = json!(2);
        let expected: Vec<String> =
            serde_json::from_str(&std::env::var("RDX_IMPLEMENTATIONS_EXPECTED").unwrap()).unwrap();
        let mut found = Vec::new();
        loop {
            args["offset"] = json!(found.len());
            let result = remote(&record, "find_implementations", &args).unwrap();
            for item in result["data"]["items"].as_array().unwrap() {
                found.push(item["class_id"].as_str().unwrap().to_owned());
            }
            if found.len() >= result["data"]["total"].as_u64().unwrap() as usize {
                break;
            }
        }
        assert_eq!(found, expected);
        args["offset"] = json!(0);
        let direct = remote(&record, "find_direct_subclasses", &args).unwrap();
        assert_eq!(direct["data"]["total"], 0);
    }

    #[test]
    fn nested_package_mcp_roundtrip_for_xapk_and_apks_without_toc() {
        let dir = TestDir::new();
        let nested_apk = fs::read(fixture("navigation.apk")).unwrap();
        for extension in ["xapk", "apks"] {
            let path = dir.0.join(format!("navigation.{extension}"));
            let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
            zip.start_file("base.apk", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&nested_apk).unwrap();
            zip.finish().unwrap();
            let (server, record, args) = start_path(&dir, path);
            let info = remote(&record, "get_instance_info", &json!({})).unwrap();
            assert_eq!(info["state"], "Running");
            let selection = info["package_selection"].as_str().unwrap();
            assert!(selection.contains("analysis union") && selection.contains("one base"));
            let classes = remote(&record, "get_all_classes", &args).unwrap();
            assert!(
                classes["data"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["class_id"] == "sample.Target")
            );
            let mut resource_args = args.clone();
            resource_args["resource_path"] = json!("AndroidManifest.xml");
            let manifest = remote(&record, "get_resource_file", &resource_args).unwrap();
            assert!(
                manifest["data"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("package=\"sample\"")
            );
            let mut source_args = args;
            source_args["class_id"] = json!("sample.Target");
            let source = remote(&record, "get_class_source", &source_args).unwrap();
            assert!(!source["data"]["text"].as_str().unwrap().is_empty());
            drop(server);
        }
    }
    #[test]
    fn call_graph_incomplete_selectors_return_candidates_in_both_projections() {
        let dir = TestDir::new();
        let (_server, record, mut args) = start(&dir, "navigation.apk");
        for projection in [None, Some("graph"), Some("call_sites")] {
            if let Some(projection) = projection {
                args["projection"] = json!(projection);
            }
            for method in [
                "sample.Target.doubleValue",
                "Lsample/Target;->doubleValue",
                "sample.Target.doubleValue(I)",
                "Lsample/Target;->doubleValue(I)",
            ] {
                args["method_id"] = json!(method);
                validate("get_call_graph", &args).unwrap();
                let error = remote(&record, "get_call_graph", &args)
                    .unwrap_err()
                    .to_string();
                assert!(error.contains("sample.Target.doubleValue(I)I"), "{error}");
                assert!(error.contains("no overload was selected"), "{error}");
                assert!(
                    error.contains("Lpkg/Class;->method(Ljava/lang/String;)V"),
                    "{error}"
                );
                assert!(
                    error.contains("a return descriptor after ')' (V for void)"),
                    "{error}"
                );
            }
            args["method_id"] = json!("sample.Target.doubleValue(I)I");
            let dotted = remote(&record, "get_call_graph", &args).unwrap();
            args["method_id"] = json!("Lsample/Target;->doubleValue(I)I");
            let smali = remote(&record, "get_call_graph", &args).unwrap();
            for key in ["nodes", "edges", "items"] {
                assert_eq!(dotted["data"][key], smali["data"][key], "{key}");
            }
            args["method_id"] = json!("Lpkg/Class;->m()");
            let error = remote(&record, "get_call_graph", &args)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("pkg.Class.method(Ljava/lang/String;)V"),
                "{error}"
            );
            assert!(
                error.contains("Lpkg/Class;->method(Ljava/lang/String;)V"),
                "{error}"
            );
            assert!(
                error.contains("a return descriptor after ')' (V for void)"),
                "{error}"
            );
        }
    }

    #[test]
    fn reviewer_tools_roundtrip_over_local_service() {
        let dir = TestDir::new();
        let (_server, record, scope) = start(&dir, "navigation.apk");
        for (tool, extra) in [
            (
                "find_annotation_usages",
                json!({"annotation_type":"sample.Annotation","element":"parameter"}),
            ),
            ("search_resources", json!({"query":"http"})),
            (
                "find_string_references",
                json!({"query":"hello","search_mode":"streaming"}),
            ),
            (
                "get_method_source",
                json!({"class_name":"Target","method_name":"doubleValue","descriptor":"(I)I"}),
            ),
            (
                "find_field_references",
                json!({"class_name":"Target","field_name":"value"}),
            ),
        ] {
            let mut args = scope.clone();
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            validate(tool, &args).unwrap();
            let response = remote(&record, tool, &args).unwrap();
            assert_eq!(response["identity"]["project_id"], scope["project_id"]);
        }
        let mut args = scope.clone();
        args.as_object_mut().unwrap().extend(json!({"method_id":"sample.Target.doubleValue(I)I","direction":"callers","projection":"call_sites","include_const_args":true}).as_object().unwrap().clone());
        let result = remote(&record, "get_call_graph", &args).unwrap();
        assert_eq!(
            result["data"]["call_sites"][0]["constant_arguments"]["status"],
            "analyzed"
        );
        let out =
            std::env::temp_dir().join(format!("rdx-reviewer-service-{}", random_id().unwrap()));
        let mut args = scope.clone();
        args.as_object_mut().unwrap().extend(
            json!({"class_name":"Target","out_path":out})
                .as_object()
                .unwrap()
                .clone(),
        );
        validate("export_source", &args).unwrap();
        let result = remote(&record, "export_source", &args).unwrap();
        let sidecar: Value =
            serde_json::from_slice(&fs::read(out.join("coverage.json")).unwrap()).unwrap();
        assert_eq!(
            sidecar["apk_identity"]["apk_sha256"],
            result["identity"]["apk_sha256"]
        );
        assert_eq!(
            digest(&out.join("source.txt")).unwrap(),
            result["data"]["sha256"]
        );
        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn projected_call_sites_roundtrip_with_identity_and_filters() {
        let dir = TestDir::new();
        let (_server, record, mut args) = start(&dir, "navigation.apk");
        args["method_id"] = json!("sample.Target.doubleValue(I)I");
        args["direction"] = json!("callers");
        args["direct_only"] = json!(true);
        args["projection"] = json!("call_sites");
        args["limit"] = json!(1);
        validate("get_call_graph", &args).unwrap();
        let first = remote(&record, "get_call_graph", &args).unwrap();
        assert_eq!(first["identity"]["project_id"], args["project_id"]);
        assert!(first["identity"]["apk_sha256"].as_str().is_some());
        let data = &first["data"];
        assert!(data.get("nodes").is_none());
        assert_eq!(data["depth"], 1);
        assert_eq!(data["truncated"], false);
        assert!(data["matching_total"].as_u64().unwrap() > 0);
        let site = &data["call_sites"][0];
        assert!(
            site["caller_method_id"]
                .as_str()
                .unwrap()
                .starts_with("sample.Caller.compute(")
        );
        assert_eq!(site["caller_signature"], site["caller_method_id"]);
        assert!(site["call_site_offset"].as_u64().is_some());
        let mut engine = NativeEngine::start().unwrap();
        engine.open(&fixture("navigation.apk")).unwrap();
        let class = engine.dex_class("sample.Caller").unwrap();
        let sites = crate::native_xrefs::call_sites(class, None, &AtomicBool::new(false)).unwrap();
        assert!(
            sites
                .iter()
                .any(|call| site["caller_method_id"] == call.caller
                    && site["callee_method_id"] == call.callee
                    && site["call_site_offset"] == call.pc)
        );
        args["offset"] = data["matching_total"].clone();
        let end = remote(&record, "get_call_graph", &args).unwrap();
        assert_eq!(end["data"]["source_hash"], data["source_hash"]);
        assert_eq!(end["data"]["call_sites"], json!([]));
        assert!(end["data"]["next_offset"].is_null());
        args["offset"] = json!(0);
        args["class_name"] = json!("sample.Unrelated");
        let empty = remote(&record, "get_call_graph", &args).unwrap();
        assert_eq!(empty["data"]["matching_total"], 0);
        for (key, value) in [
            ("direct_only", json!("true")),
            ("direct_only", Value::Null),
            ("depth", json!(2)),
            ("limit", json!(501)),
            ("annotation", json!([])),
        ] {
            let mut invalid = args.clone();
            invalid[key] = value;
            assert!(validate("get_call_graph", &invalid).is_err(), "{invalid}");
        }
        args["projection"] = json!("graph");
        assert!(validate("get_call_graph", &args).is_err());
    }

    // A compiled XML fixture verifies the endpoint decodes the archive itself.
    fn component_manifest_binary() -> Vec<u8> {
        fn words(values: &[u32]) -> Vec<u8> {
            values.iter().flat_map(|v| v.to_le_bytes()).collect()
        }
        fn chunk(kind: u16, header: u16, body: &[u8]) -> Vec<u8> {
            let mut bytes = Vec::new();
            bytes.extend(kind.to_le_bytes());
            bytes.extend(header.to_le_bytes());
            bytes.extend(((body.len() + 8) as u32).to_le_bytes());
            bytes.extend(body);
            bytes
        }
        fn start(tag: u32, attrs: &[[u32; 5]]) -> Vec<u8> {
            let mut bytes = words(&[1, u32::MAX, u32::MAX, tag]);
            for value in [20u16, 20, attrs.len() as u16, 0, 0, 0] {
                bytes.extend(value.to_le_bytes());
            }
            for attr in attrs {
                bytes.extend(words(attr));
            }
            chunk(0x102, 16, &bytes)
        }
        fn end(tag: u32) -> Vec<u8> {
            chunk(0x103, 16, &words(&[1, u32::MAX, u32::MAX, tag]))
        }
        let strings = [
            "android",
            "http://schemas.android.com/apk/res/android",
            "manifest",
            "package",
            "sample",
            "application",
            "activity",
            "name",
            ".Entry",
            "exported",
            "permission",
            "sample.ACCESS",
            "activity-alias",
            ".Link",
            "targetActivity",
            "intent-filter",
            "data",
            "scheme",
            "https",
            "host",
            "example.com",
            "pathPrefix",
            "/open",
        ];
        let mut offsets = Vec::new();
        let mut data = Vec::new();
        for value in strings {
            offsets.extend((data.len() as u32).to_le_bytes());
            data.extend([value.len() as u8, value.len() as u8]);
            data.extend(value.bytes());
            data.push(0);
        }
        let mut pool = words(&[
            strings.len() as u32,
            0,
            256,
            28 + 4 * strings.len() as u32,
            0,
        ]);
        pool.extend(offsets);
        pool.extend(data);
        let mut chunks = chunk(1, 28, &pool);
        chunks.extend(chunk(0x100, 16, &words(&[1, u32::MAX, 0, 1])));
        chunks.extend(start(2, &[[u32::MAX, 3, 4, 0x03000008, 4]]));
        chunks.extend(start(5, &[[1, 10, 11, 0x03000008, 11]]));
        chunks.extend(start(
            6,
            &[[1, 7, 8, 0x03000008, 8], [1, 9, u32::MAX, 0x12000008, 0]],
        ));
        chunks.extend(end(6));
        chunks.extend(start(
            12,
            &[
                [1, 7, 13, 0x03000008, 13],
                [1, 14, 8, 0x03000008, 8],
                [1, 9, u32::MAX, 0x12000008, 1],
            ],
        ));
        chunks.extend(start(15, &[]));
        chunks.extend(start(
            16,
            &[
                [1, 17, 18, 0x03000008, 18],
                [1, 19, 20, 0x03000008, 20],
                [1, 21, 22, 0x03000008, 22],
            ],
        ));
        for tag in [16, 15, 12, 5, 2] {
            chunks.extend(end(tag));
        }
        chunks.extend(chunk(0x101, 16, &words(&[1, u32::MAX, 0, 1])));
        chunk(3, 8, &chunks)
    }

    #[test]
    fn decoded_component_lookup_roundtrip_pages_aliases_and_routes() {
        use std::io::Write;
        let dir = TestDir::new();
        let path = dir.0.join("components.apk");
        let mut archive = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        archive
            .start_file(
                "AndroidManifest.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive.write_all(&component_manifest_binary()).unwrap();
        archive
            .start_file("classes.dex", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap();
        archive.finish().unwrap();
        let (_server, record, mut args) = start_path(&dir, path);
        args["class_name"] = json!(".Entry");
        args["match_mode"] = json!("exact");
        args["limit"] = json!(1);
        validate("get_manifest_components", &args).unwrap();
        let first = remote(&record, "get_manifest_components", &args).unwrap();
        assert_eq!(first["identity"]["project_id"], args["project_id"]);
        assert_eq!(first["data"]["total"], 2);
        assert_eq!(first["data"]["next_offset"], 1);
        assert_eq!(first["data"]["items"][0]["name"], "sample.Entry");
        assert_eq!(
            first["data"]["items"][0]["exported_state"]["effective"],
            false
        );
        assert_eq!(
            first["data"]["items"][0]["aliases"][0]["name"],
            "sample.Link"
        );
        args["offset"] = json!(1);
        let second = remote(&record, "get_manifest_components", &args).unwrap();
        assert_eq!(second["identity"], first["identity"]);
        assert_eq!(second["data"]["source_hash"], first["data"]["source_hash"]);
        assert!(second["data"]["next_offset"].is_null());
        assert_eq!(
            second["data"]["items"][0]["permission"]["effective"],
            "sample.ACCESS"
        );
        assert_eq!(
            second["data"]["items"][0]["permission"]["source"],
            "target_activity"
        );
        args["offset"] = json!(0);
        args["host"] = json!("example.com");
        args["scheme"] = json!("https");
        args["path"] = json!("/open");
        let route = remote(&record, "get_manifest_components", &args).unwrap();
        assert_eq!(route["data"]["total"], 1);
        assert_eq!(route["data"]["items"][0]["name"], "sample.Link");
        assert_eq!(
            route["data"]["items"][0]["matching_filter_indexes"],
            json!([0])
        );
        assert_eq!(
            route["data"]["items"][0]["exported_state"]["effective"],
            true
        );
        for (key, value) in [
            ("class_name", Value::Null),
            ("match_mode", json!("regex")),
            ("limit", json!(501)),
            ("component_type", json!("application")),
        ] {
            let mut invalid = args.clone();
            invalid[key] = value;
            assert!(
                validate("get_manifest_components", &invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn call_graph_tool_roundtrip() {
        let dir = TestDir::new();
        let (_server, record, mut args) = start(&dir, "navigation.apk");
        args["method_id"] = json!("sample.Target.doubleValue(I)I");
        args["direction"] = json!("callers");
        validate("get_call_graph", &args).unwrap();
        let result = remote(&record, "get_call_graph", &args).unwrap();
        let data = &result["data"];
        let nodes = data["nodes"].as_array().unwrap();
        assert!(nodes.iter().any(|n| {
            n["method_id"]
                .as_str()
                .unwrap()
                .starts_with("sample.Caller.compute(")
        }));
        assert!(
            data["edges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["to"] == 0)
        );
        assert_eq!(data["depth"], 20);
        assert_eq!(data["truncated"], false);
        args["direction"] = json!("both");
        assert!(remote(&record, "get_call_graph", &args).is_ok());
        args["direction"] = json!("wrong");
        assert!(validate("get_call_graph", &args).is_err());
        args["direction"] = json!("callees");
        args["depth"] = json!(101);
        assert!(validate("get_call_graph", &args).is_err());
    }
    #[test]
    fn instance_isolation_authentication_and_stop() {
        let dir = TestDir::new();
        let (a, record_a, args_a) = start(&dir, "hello.apk");
        let (b, record_b, args_b) = start(&dir, "navigation.apk");
        let response_a = remote(&record_a, "get_all_classes", &args_a).unwrap();
        let response_b = remote(&record_b, "get_all_classes", &args_b).unwrap();
        assert_ne!(
            response_a["identity"]["apk_sha256"],
            response_b["identity"]["apk_sha256"]
        );
        assert_eq!(response_a["identity"]["instance_id"], a.instance_id);
        assert_eq!(response_b["identity"]["instance_id"], b.instance_id);
        assert!(
            remote(&record_a, "get_all_classes", &args_b)
                .unwrap_err()
                .to_string()
                .contains("PROJECT_CHANGED")
        );
        let mut bad = record_a.clone();
        bad.token = "bad".into();
        assert!(
            remote(&bad, "get_instance_info", &json!({}))
                .unwrap_err()
                .to_string()
                .contains("Unauthorized")
        );
        let mut args = args_a.clone();
        args["class_id"] = response_a["data"]["items"][0]["class_id"].clone();
        let source = remote(&record_a, "get_class_source", &args).unwrap();
        assert!(!source["data"]["text"].as_str().unwrap().is_empty());
        let methods = remote(&record_a, "get_methods_of_class", &args).unwrap();
        assert!(methods["data"]["items"].as_array().is_some());
        let path = a.registration.clone();
        drop(a);
        assert!(!path.exists());
        thread::sleep(Duration::from_millis(60));
        assert!(remote(&record_a, "get_instance_info", &json!({})).is_err());
        assert!(remote(&record_b, "get_all_classes", &args_b).is_ok());
    }
    #[test]
    fn source_pagination_is_lossless_and_unicode_safe() {
        let original = "class Café {\n    String x = \"😀\";\n}\n";
        let mut offset = 0;
        let mut rebuilt = String::new();
        loop {
            let p =
                text_page(original.into(), &json!({"offset":offset,"limit":3}), "java").unwrap();
            rebuilt.push_str(p["text"].as_str().unwrap());
            match p["next_offset"].as_u64() {
                Some(n) => offset = n,
                None => break,
            }
        }
        assert_eq!(rebuilt, original);
        assert!(text_page(original.into(), &json!({"offset":999}), "java").is_err());
    }
    #[test]
    fn advertised_schemas_reject_unknown_and_invalid_arguments() {
        assert!(
            validate(
                "get_class_source",
                &json!({"instance_id":"a","project_id":"b","class_id":"C","limit":128001})
            )
            .is_err()
        );
        assert!(
            validate(
                "get_all_classes",
                &json!({"instance_id":"a","project_id":"b","command":"bad"})
            )
            .is_err()
        );
        assert!(validate("get_all_classes", &json!({"instance_id":"a"})).is_err());
        assert!(validate("rename_class", &json!({})).is_err());
        assert!(
            validate(
                "open_apk",
                &json!({"path":"/tmp/test.apk","request_key":"x"})
            )
            .is_ok()
        );
        let tools = tools_list();
        let open = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "open_apk")
            .unwrap();
        let description = open["description"].as_str().unwrap();
        assert!(description.contains("XAPK") && description.contains("APKS"));
        assert!(read_frame(&mut std::io::Cursor::new(b"{}".as_slice())).is_err());
    }
}

#[cfg(test)]
#[path = "mcp_improvement_tests.rs"]
mod improvement_tests;

#[cfg(test)]
mod corpus_review_tests {
    use super::*;
    #[test]
    #[ignore = "requires RDX_REVIEW_APK pointing to the pinned Airbnb APK"]
    fn airbnb_improved_queries_roundtrip() {
        let path = PathBuf::from(std::env::var_os("RDX_REVIEW_APK").unwrap());
        let directory =
            std::env::temp_dir().join(format!("rdx-mcp-airbnb-{}", random_id().unwrap()));
        fs::create_dir(&directory).unwrap();
        let server =
            Server::start_in(path.clone(), directory.clone(), random_id().unwrap()).unwrap();
        let record: Registration =
            serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        while server.status().state != "Running" {
            let status = server.status();
            assert_ne!(status.state, "Failed", "{}", status.last_error);
            assert!(std::time::Instant::now() < deadline, "project load timeout");
            thread::sleep(Duration::from_millis(50));
        }
        let info = remote(&record, "get_instance_info", &json!({})).unwrap();
        assert_eq!(info["apk_path"], json!(path.canonicalize().unwrap()));
        assert_eq!(info["package_name"], "com.airbnb.android");
        assert_eq!(
            info["apk_sha256"],
            "cc74899171c8e84a2c4b6d5e0f307ccbbfbb5d61911fb3113250ca17217d07ad"
        );
        let discovered =
            discover(std::slice::from_ref(&record), &json!({"apk_path":path})).unwrap();
        assert_eq!(discovered["total"], 1);
        assert!(discovered["diagnostics"].as_array().unwrap().is_empty());
        let mut args = json!({"instance_id":server.instance_id,"project_id":server.status().project_id,"component_type":"activity","limit":10});
        let manifest = remote(&record, "get_manifest_summary", &args).unwrap();
        assert!(!manifest["data"]["items"].as_array().unwrap().is_empty());
        assert!(
            manifest["data"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["component_type"] == "activity")
        );
        let mut permission = args.clone();
        permission.as_object_mut().unwrap().remove("component_type");
        permission["permission"] = json!("android.permission.INTERNET");
        let permissions = remote(&record, "get_manifest_summary", &permission).unwrap();
        assert!(
            permissions["data"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"] == "android.permission.INTERNET")
        );
        args.as_object_mut().unwrap().remove("component_type");
        args["query"] = json!("Intent arguments of ");
        args["match_mode"] = json!("exact");
        args["limit"] = json!(500);
        let first = std::time::Instant::now();
        let literal = remote(&record, "find_string_references", &args).unwrap();
        let first_ms = first.elapsed().as_millis();
        assert!(literal["data"]["index_complete"].as_bool().unwrap());
        assert!(literal["data"]["items"].as_array().unwrap().iter().any(|v|v["method_id"]=="com.airbnb.android.lib.airactivity.activities.a.onCreate(Landroid/os/Bundle;)V"));
        let second = std::time::Instant::now();
        let again = remote(&record, "find_string_references", &args).unwrap();
        assert_eq!(literal, again);
        println!(
            "{}",
            json!({"apk_sha256":info["apk_sha256"],"absolute_path_matches":discovered["total"],"activity_count":manifest["data"]["total"],"permission_count":permissions["data"]["total"],"literal_result":literal["data"],"first_query_ms":first_ms,"cached_query_ms":second.elapsed().as_millis()})
        );
        args["project_id"] = json!("wrong");
        assert!(
            remote(&record, "find_string_references", &args)
                .unwrap_err()
                .to_string()
                .contains("PROJECT_CHANGED")
        );
        drop(server);
        let _ = fs::remove_dir_all(directory);
    }
}

#[cfg(test)]
#[path = "mcp_indexing_tests.rs"]
mod indexing_tests;

#[cfg(test)]
#[path = "mcp_analysis_tests.rs"]
mod analysis_tests;

#[cfg(test)]
#[path = "mcp_method_format_tests.rs"]
mod method_format_tests;

#[cfg(test)]
#[path = "mcp_reviewer_tests.rs"]
mod reviewer_tests;

#[path = "mcp_hotwire.rs"]
mod hotwire;
