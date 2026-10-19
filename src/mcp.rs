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
#[path = "mcp_literals.rs"]
mod literals;
#[path = "mcp_socket.rs"]
mod socket;

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
                    let literals = literals::Controller::new(
                        Arc::new(engine.clone()),
                        project.clone(),
                        loading_stop,
                    );
                    Ok(Arc::new(LoadedProject {
                        engine: Mutex::new(engine),
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
                                "literal_index":loaded.as_ref().map(|project| project.literals.status()).unwrap_or_else(|| s.literal_index.clone()),"active_requests":s.active_requests,
                                "error":if s.state == "Failed" {s.last_error.as_str()} else {""},
                                "last_request_error":if s.last_error.is_empty() || s.state == "Failed" {Value::Null} else {json!({"message":s.last_error,"tool":s.last_error_tool,"requested_class_id":s.last_error_class_id})},"build":build}),
                            );
                        }
                        let project = loaded.as_ref().context("PROJECT_NOT_READY")?;
                        let args = &request["arguments"];
                        context = Some((
                            name.to_owned(),
                            args["class_id"].as_str().unwrap_or("").to_owned(),
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
            | "get_resource_file"
            | "get_manifest_summary"
            | "find_string_references"
    ) {
        check_project_file(project)?;
    }
    if name == "find_string_references" {
        let result = project.literals.search(args, cancel);
        check_project_file(project)?;
        return result;
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
    if name == "get_manifest_summary" {
        let xml = engine
            .read_resource_with_metadata("AndroidManifest.xml")?
            .source;
        let mut result = if manifest_filtered(args) {
            let records = crate::manifest_summary::query(&xml, args).map_err(anyhow::Error::msg)?;
            let hash = crate::engine::source_identity(&serde_json::to_string(&records)?);
            let mut result = page(records, args)?;
            result["format"] = json!("records");
            result["source_hash"] = json!(hash);
            result["offset_unit"] = json!("matching records");
            result
        } else {
            let summary = crate::manifest_summary::render(
                &xml,
                &project.path,
                &project.hash,
                engine.resource_table(),
            )
            .map_err(anyhow::Error::msg)?;
            text_page(summary, args, "markdown")?
        };
        result["analysis"] = json!(
            "Manifest declarations only; exported does not establish runtime callback reachability or authorization."
        );
        Ok(result)
    } else {
        dispatch(&mut engine, &project.project, name, args, cancel)
    }
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
        json!({"text":text.chars().skip(start).take(limit).collect::<String>(),"format":format,"source_hash":hash,"offset":start,"total_chars":total,"next_offset":(end<total).then_some(end)}),
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

fn discover(entries: &[Registration], args: &Value) -> Result<Value> {
    let start = args["registration_offset"].as_u64().unwrap_or(0) as usize;
    ensure!(start <= entries.len(), "Invalid registration_offset");
    let end = start.saturating_add(16).min(entries.len());
    let mut items = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in &entries[start..end] {
        match remote(entry, "get_instance_info", &json!({})) {
            Ok(info) => { if instance_matches(&info,args) { items.push(info); } },
            Err(error) => diagnostics.push(json!({"instance_id":entry.instance_id,
                "status":"instance_unreachable", "error":format!("{error:#}"),
                "action":"Check the GUI MCP status and retry. If the GUI has closed, reopen it; an unreachable registration alone does not prove a stale process."})),
        }
    }
    let mut result = page(items, args)?;
    result["diagnostics"] = json!(diagnostics);
    result["discovery_complete"] = json!(end == entries.len());
    result["next_registration_offset"] = json!((end < entries.len()).then_some(end));
    result["registered_instances"] = json!(entries.len());
    Ok(result)
}

fn search_code(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
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
        json!({"items":items,"scope":"generated Java and mixed DEX; case-sensitive literal plus exact numeric aliases for resolved resource links; resource files excluded","offset":offset,"scanned_classes":scanned,"total_classes":classes.len(),"errors":errors,"next_offset":next,"next_source_offset":next.map(|_|next_source),"exhausted":next.is_none()}),
    )
}

fn resolve_class_id<'a>(project: &'a Project, requested: &str) -> Result<&'a str> {
    if let Some(exact) = project.classes.iter().find(|c| c.as_str() == requested) {
        return Ok(exact);
    }
    ensure!(
        !requested.contains('.'),
        "SYMBOL_NOT_FOUND: requested_class_id={requested}"
    );
    let mut matches = project
        .classes
        .iter()
        .filter(|c| c.rsplit('.').next() == Some(requested));
    let found = matches
        .next()
        .with_context(|| format!("SYMBOL_NOT_FOUND: requested_class_id={requested}"))?;
    ensure!(
        matches.next().is_none(),
        "AMBIGUOUS_CLASS: requested_class_id={requested}; use a fully qualified class_id"
    );
    Ok(found)
}
fn dispatch(
    engine: &mut NativeEngine,
    project: &Project,
    name: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut normalized = args.clone();
    if let Some(requested) = args["class_id"].as_str() {
        normalized["class_id"] = json!(resolve_class_id(project, requested)?);
    }
    let mut result = dispatch_resolved(engine, project, name, &normalized, cancel)?;
    if args["class_id"].is_string() {
        result["class_id"] = normalized["class_id"].clone();
        result["requested_class_id"] = args["class_id"].clone();
    }
    Ok(result)
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
        "get_call_graph" => {
            let direction = args["direction"].as_str().unwrap_or("callees");
            let depth = args["depth"].as_u64().unwrap_or(20) as usize;
            let graph = engine.call_graph_by_id(required(args,"method_id")?, depth, direction, cancel)?;
            let masks = graph.component_reachability();
            let kinds = |mask: u8| ["Activity", "Service", "Receiver", "Provider"].into_iter().enumerate()
                .filter_map(|(i,k)| ((mask & (1 << i)) != 0).then_some(k)).collect::<Vec<_>>();
            let nodes: Vec<_> = graph.nodes.iter().enumerate().map(|(i,n)| json!({
                "id":i,"method_id":n.method,"depth":n.depth,"component":n.component.map(|c| c.label()),
                "available":n.available,"reachable_components":kinds(masks[i])})).collect();
            let edges: Vec<_> = graph.edges.iter().map(|e|json!({"from":e.from,"to":e.to,"call_sites":e.sites,
                "highlighted":masks[e.to]!=0,"reachable_components":kinds(masks[e.to])})).collect();
            Ok(json!({"dispatch_targets":graph.dispatch_targets,"dispatch_targets_truncated":graph.dispatch_targets_truncated,
                "dispatch_note":"Related ancestor declarations are possible dispatch entry points, not proven runtime callers. Query their callers separately. Component ancestry and accepted service starts do not prove callback reachability. Constructor/provider callers do not establish active route registration; route-table data flow and runtime intent delivery are not inferred.",
                "root":0,"direction":direction,"depth":depth,"nodes":nodes,"edges":edges,
                "truncated":graph.truncated,"node_limit":5000,"edge_limit":20000,
                "depth_boundary_reached":graph.nodes.iter().any(|n|n.depth==depth),
                "analysis":"static DEX declared calls; inherited aliases resolved through known superclasses; no runtime dispatch, reflection, or Intent target inference"}))
        },
        "get_method_source" => {
            let id = required(args, "method_id")?;
            let (code, fallback) = engine.decompile_method_source(id, cancel)?;
            let mut result = method_source_page(code.source, args, if fallback.is_some() { "rdx-dex" } else { "java" })?;
            result["method_id"] = json!(id);
            result["offset_unit"] = json!("Unicode scalar characters relative to this method source");
            result["fallback_reason"] = json!(fallback);
            result["scope"] = json!("Selected method; enclosing imports and generated class helpers are available from get_class_source");
            Ok(result)
        },
        "search_code" => search_code(engine, project, args, cancel),
        "get_all_classes" | "search_classes_by_keyword" => page(project.classes.iter().filter(|c|c.contains(query)).map(|c|json!({"class_id":c,"name":c})).collect(),args),
        "get_class_source"=>text_page(engine.decompile_search_cancellable(required(args,"class_id")?,cancel)?.source,args,"java-or-mixed-dex"),
        "get_class_disassembly"=> {let name=required(args,"class_id")?; let class=engine.dex_class(name).context("SYMBOL_NOT_FOUND")?; text_page(crate::native_engine::disassembly::render(name,class).source,args,"rdx-dex")},
        "get_methods_of_class"|"get_fields_of_class"=>{
            let owner=required(args,"class_id")?;
            let class=engine.dex_class(owner).context("SYMBOL_NOT_FOUND")?;
            metadata(class,owner,args,name=="get_fields_of_class")
        },
        "find_implementations"=>page(engine.implementation_names(required(args,"class_id")?,cancel)?.into_iter().map(|c|json!({"class_id":c})).collect(),args),
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
    let mut out = Vec::new();
    for entry in fs::read_dir(registry()?)?.take(1024) {
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
            {
                out.push(record);
            }
        }
    }
    out.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
    Ok(out)
}
fn remote(entry: &Registration, name: &str, args: &Value) -> Result<Value> {
    remote_controlled(entry, name, args, None)
}
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
        bail!("{error}")
    }
    Ok(reply["result"].clone())
}
pub fn client_config() -> Result<String> {
    Ok(serde_json::to_string_pretty(
        &json!({"mcpServers":{"rdx":{"command":std::env::current_exe()?,"args":["mcp"]}}}),
    )?)
}
fn tools_list() -> Value {
    let definitions = [
        (
            "get_health",
            "Check this gateway and registered GUI connections, including executable identity and reconnect guidance. A closed client transport requires client-side reconnection before this tool can run.",
            vec![],
            false,
        ),
        (
            "find_string_references",
            "Search a cached DEX literal-owner index without Java decompilation. Includes const-string/jumbo instructions, encoded static values and annotation values. Excludes unreferenced strings and dynamically constructed values. Returns exact class/member IDs and code-unit offsets; literal previews are capped at 4096 characters. Pagination is by matching occurrences. The first query shares a cancellable background build; get_instance_info.literal_index reports progress. Failed or cancelled builds retry on the next query.",
            vec!["query"],
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
            "Open a local APK, XAPK, or APKS in a new RDX GUI instance with MCP enabled.",
            vec!["path", "request_key"],
            false,
        ),
        (
            "get_call_graph",
            "Build a static method call graph with component-path highlights. Direction: callers, callees (default), or both. Depth defaults to 20; capped at 5000 nodes/20000 edges. Returns full graph, not paginated.",
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
            "Search generated Java and mixed DEX by case-sensitive literal substring. No resources. Bounded class pages (limit defaults to 25, max 500), at most 100 matches per response. Resume using next_offset AND next_source_offset with the same query/package_prefix; no-match is conclusive only after all pages with no errors.",
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
            "get_method_source",
            "Read one exact DEX method_id from get_methods_of_class. Returns the complete method by default; optional limit/offset paginates method-local Unicode characters with a stable hash. Includes Java or explicit DEX fallback and pinned project/APK identity. Enclosing imports and helpers remain in get_class_source.",
            vec!["method_id"],
            true,
        ),
        (
            "get_class_source",
            "Read Java or mixed Java/DEX source. All text is available through offset pagination.",
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
            "find_implementations",
            "List concrete implementations of an interface or class, including indirect implementations through known hierarchy edges.",
            vec!["class_id"],
            true,
        ),
        (
            "find_direct_subclasses",
            "List immediate superclass children; interface implementors are available through find_implementations.",
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
        if (scoped && name!="get_call_graph")||["list_instances","get_health"].contains(&name){
            properties.insert("offset".into(),json!({"type":"integer","minimum":0}));
            properties.insert("limit".into(),json!({"type":"integer","minimum":1,"maximum":if ["get_method_source","get_class_source","get_class_disassembly","get_android_manifest","get_resource_file","get_manifest_summary"].contains(&name){128000}else{500}}));
        }
        if ["list_instances","get_health"].contains(&name) {
            properties.insert("registration_offset".into(),json!({"type":"integer","minimum":0}));
            for key in ["apk_path", "apk_sha256", "package_name"] { properties.insert(key.into(),json!({"type":"string","minLength":1})); }
        }
        if name=="get_manifest_summary" {
            for key in ["component", "host", "scheme", "path", "permission", "query"] { properties.insert(key.into(),json!({"type":"string","minLength":1})); }
            properties.insert("component_type".into(),json!({"type":"string","enum":["activity","activity-alias","service","receiver","provider"]}));
            properties.insert("format".into(),json!({"type":"string","enum":["markdown","records"]}));
        }
        if name=="find_string_references" {
            properties.insert("match_mode".into(),json!({"type":"string","enum":["exact","contains"],"default":"contains"}));
            properties.insert("package_prefix".into(),json!({"type":"string"}));
        }
        if name=="search_code" {
            properties.insert("package_prefix".into(),json!({"type":"string"}));
            properties.insert("source_offset".into(),json!({"type":"integer","minimum":0}));
        }
        if name=="get_call_graph" {
            properties.insert("depth".into(),json!({"type":"integer","minimum":1,"maximum":100,"default":20}));
            properties.insert("direction".into(),json!({"type":"string","enum":["callers","callees","both"],"default":"callees"}));
        }
        if ["get_all_classes","get_all_resource_file_names","get_strings","get_dex_strings"].contains(&name){properties.insert("query".into(),json!({"type":"string"}));}
        json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":name!="open_apk","destructiveHint":false,"openWorldHint":false}})
    }).collect::<Vec<_>>()})
}
fn validate(name: &str, args: &Value) -> Result<()> {
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
        required(args, key.as_str().unwrap())?;
    }
    for (key, value) in object {
        let property = &schema["properties"][key];
        ensure!(!property.is_null(), "Unknown argument: {key}");
        if property["type"] == "string" {
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
        } else {
            let number = value.as_u64().context("Expected a nonnegative integer")?;
            ensure!(
                number >= property["minimum"].as_u64().unwrap_or(0)
                    && number <= property["maximum"].as_u64().unwrap_or(usize::MAX as u64),
                "Invalid {key}"
            );
        }
    }
    if name == "get_manifest_summary" && manifest_filtered(args) {
        ensure!(
            args["limit"].as_u64().unwrap_or(100) <= 500,
            "Filtered manifest record limit must be <=500"
        );
    }
    Ok(())
}
fn gateway_call(
    name: &str,
    args: &Value,
    opened: &mut std::collections::HashMap<String, (PathBuf, String)>,
) -> Result<Value> {
    validate(name, args)?;
    if name == "open_apk" {
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
        if let Some((existing, id)) = opened.get(key) {
            ensure!(
                existing == &path,
                "request_key belongs to a different package"
            );
            return Ok(json!({"instance_id":id,"state":"launching","apk_path":path}));
        }
        let id = random_id()?;
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
        opened.insert(key.into(), (path.clone(), id.clone()));
        return Ok(json!({"instance_id":id,"state":"launching","apk_path":path}));
    }
    let entries = registrations()?;
    if name == "list_instances" || name == "get_health" {
        let mut result = discover(&entries, args)?;
        if name == "get_health" {
            result["gateway"] = build_identity();
            result["transport"] = json!("connected");
            result["guidance"] = json!(
                "No registrations: enable MCP in the GUI or use open_apk. Unreachable registration: check the GUI and retry discovery. Transport closed before a tool response: reconnect/restart the MCP connector; a running GUI does not imply a running gateway. Each GUI request opens a fresh local connection."
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
/// Newline-delimited stdio MCP. This process never owns a GUI project.
pub fn run_stdio() -> Result<()> {
    gateway::run()
}

#[cfg(test)]
mod tests {
    use super::*;
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
