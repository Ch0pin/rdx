//! Optional isolated class decompilation. Bulk render paths never call this module.
use crate::{engine::DecompiledCode, package_container::PackageContainer};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

pub const BACKEND: &str = "androguard-f7fa4a4853d957ebc3c54355d128a3835ef5ae22-rdx2";
pub const UNVERIFIED_WARNING: &str = "Unverified Androguard reconstruction";
pub const ORIGINAL_DEX_BEGIN: &str = "// RDX_ORIGINAL_DEX_BEGIN ";
pub const ORIGINAL_DEX_END: &str = "// RDX_ORIGINAL_DEX_END";
const RESULT_LIMIT: u64 = 8 * 1024 * 1024;
const CACHE_LIMIT: u64 = 64 * 1024 * 1024;
static WORKER: Mutex<()> = Mutex::new(());

struct WorkerChild(Child);
impl std::ops::Deref for WorkerChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for WorkerChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for WorkerChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "macos")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        )
    };
    (result == size).then_some(info.pti_resident_size)
}
#[cfg(target_os = "linux")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|kb| kb * 1024)
    })
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resident_bytes(_: u32) -> Option<u64> {
    None
}

#[derive(Default)]
struct State {
    directory: Option<tempfile::TempDir>,
    identity: Option<(PathBuf, u64, Option<SystemTime>, String)>,
    entries: VecDeque<(String, u64)>,
}

/// Clones share project lifetime, opt-in and disk cache; no alternate APK model is retained.
#[derive(Clone, Default)]
pub(crate) struct Session {
    enabled: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
    helper: Option<PathBuf>,
    timeout: Option<Duration>,
}

pub(crate) struct ClassInput<'a> {
    pub input: &'a Path,
    pub container: Option<&'a PackageContainer>,
    pub owner: &'a str,
    pub metadata: &'a crate::native_dex::DexClass,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Method {
    id: String,
    java: Option<String>,
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    diagnostics: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Response {
    protocol: u32,
    backend: String,
    class: String,
    methods: Vec<Method>,
}

pub fn helper_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RDX_ANDROGUARD_HELPER") {
        return Some(path.into());
    }
    let name = if cfg!(windows) {
        "rdx-androguard-helper.exe"
    } else {
        "rdx-androguard-helper"
    };
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|path| path.join(name))
}
pub fn availability() -> String {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return "Androguard fallback unavailable on this platform: worker memory enforcement is unsupported".into();
    }
    match helper_path().filter(|p| p.is_file()) {
        Some(_) => "Androguard decompiler is available".into(),
        None => "Androguard helper unavailable; install the optional release helper beside RDX or set RDX_ANDROGUARD_HELPER".into(),
    }
}

impl Session {
    pub fn bind_input(&self, input: &Path) -> Result<()> {
        let metadata = fs::metadata(input)?;
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("alternate session lock poisoned"))?
            .identity = Some((
            input.into(),
            metadata.len(),
            metadata.modified().ok(),
            String::new(),
        ));
        Ok(())
    }
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn obtain(
        &self,
        input: &Path,
        container: Option<&PackageContainer>,
        origin: &str,
        class: &str,
        requested: &BTreeSet<String>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(Response, bool, (u64, u64))> {
        ensure!(
            cfg!(any(target_os = "macos", target_os = "linux")),
            "alternate worker memory enforcement unsupported on this platform"
        );
        let helper = self
            .helper
            .clone()
            .or_else(helper_path)
            .filter(|p| p.is_file())
            .context("optional Androguard helper unavailable")?;
        let mut state = loop {
            ensure!(!cancelled(), "CANCELLED");
            match self.state.try_lock() {
                Ok(state) => break state,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(_) => anyhow::bail!("alternate session lock poisoned"),
            }
        };
        let metadata = fs::metadata(input)?;
        if let Some((path, len, modified, _)) = &state.identity {
            ensure!(
                path == input && *len == metadata.len() && *modified == metadata.modified().ok(),
                "PROJECT_CHANGED: reopen project before alternate reconstruction"
            );
        }
        if state
            .identity
            .as_ref()
            .is_none_or(|identity| identity.3.is_empty())
        {
            let mut file = File::open(input)?;
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                ensure!(!cancelled(), "CANCELLED");
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            let after = fs::metadata(input)?;
            ensure!(
                after.len() == metadata.len() && after.modified().ok() == metadata.modified().ok(),
                "PROJECT_CHANGED during hashing"
            );
            state.identity = Some((
                input.into(),
                metadata.len(),
                metadata.modified().ok(),
                format!("{:x}", hash.finalize()),
            ));
        }
        let key = format!(
            "{:x}",
            Sha256::digest(format!(
                "{}\0{class}\0{BACKEND}\0default-restructure",
                state.identity.as_ref().unwrap().3
            ))
        );
        if state.directory.is_none() {
            let directory = tempfile::Builder::new()
                .prefix("rdx-alternate-")
                .tempdir()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
            }
            state.directory = Some(directory);
        }
        let directory = state.directory.as_ref().unwrap().path().to_path_buf();
        let cached = directory.join(format!("{key}.json"));
        let mut previous = Vec::new();
        if cached.is_file() {
            let response = read_response(&cached, class)?;
            if let Some(index) = state.entries.iter().position(|(id, _)| id == &key) {
                let entry = state.entries.remove(index).unwrap();
                state.entries.push_back(entry);
            }
            if requested
                .iter()
                .all(|id| response.methods.iter().any(|method| &method.id == id))
            {
                return Ok((response, true, (0, 0)));
            }
            previous = response.methods;
        }
        let _worker = loop {
            ensure!(!cancelled(), "CANCELLED");
            match WORKER.try_lock() {
                Ok(worker) => break worker,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(_) => anyhow::bail!("alternate worker lock poisoned"),
            }
        };
        let work = tempfile::Builder::new()
            .prefix("work-")
            .tempdir_in(&directory)?;
        let dex = if let Some(container) = container {
            let entry = container
                .entries()
                .iter()
                .find(|entry| {
                    let name = if entry.source_path.is_empty() {
                        entry.member_path.clone()
                    } else {
                        format!("{} / {}", entry.source_path, entry.member_path)
                    };
                    name == origin
                })
                .context("owning DEX member unavailable")?;
            let dex = work.path().join("input.dex");
            container.copy_member(entry.index, &mut File::create(&dex)?, 256 * 1024 * 1024)?;
            dex
        } else {
            ensure!(
                fs::metadata(input)?.len() <= 256 * 1024 * 1024,
                "DEX exceeds 256 MiB"
            );
            let dex = work.path().join("input.dex");
            let mut source = File::open(input)?.take(256 * 1024 * 1024 + 1);
            let copied = std::io::copy(&mut source, &mut File::create(&dex)?)?;
            ensure!(copied <= 256 * 1024 * 1024, "DEX exceeds 256 MiB");
            dex
        };
        let output = work.path().join("response.json");
        let errors = work.path().join("stderr.txt");
        let mut command = Command::new(&helper);
        command
            .arg(&dex)
            .arg(class)
            .stdin(Stdio::null())
            .stdout(File::create(&output)?)
            .stderr(File::create(&errors)?)
            .env("RAYON_NUM_THREADS", "1");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Only async-signal-safe libc calls between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    let file = libc::rlimit {
                        rlim_cur: RESULT_LIMIT,
                        rlim_max: RESULT_LIMIT,
                    };
                    if libc::setrlimit(libc::RLIMIT_FSIZE, &file) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    #[cfg(target_os = "linux")]
                    let data = libc::rlimit {
                        rlim_cur: 1024 * 1024 * 1024,
                        rlim_max: 1024 * 1024 * 1024,
                    };
                    #[cfg(target_os = "linux")]
                    if libc::setrlimit(libc::RLIMIT_DATA, &data) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    #[cfg(target_os = "linux")]
                    if libc::setrlimit(libc::RLIMIT_AS, &data) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let cpu = libc::rlimit {
                        rlim_cur: 30,
                        rlim_max: 30,
                    };
                    if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = WorkerChild(
            command
                .spawn()
                .with_context(|| format!("cannot run {}", helper.display()))?,
        );
        let started = Instant::now();
        let mut peak_rss = 0;
        let mut peak_combined = 0;
        let status = loop {
            let rss = resident_bytes(child.id());
            if let Some(rss) = rss {
                peak_rss = peak_rss.max(rss);
                peak_combined =
                    peak_combined.max(rss + resident_bytes(std::process::id()).unwrap_or(0));
            }
            if cancelled()
                || started.elapsed() > self.timeout.unwrap_or(Duration::from_secs(30))
                || rss.is_some_and(|bytes| bytes > 512 * 1024 * 1024)
                || fs::metadata(&output)?.len() > RESULT_LIMIT
                || fs::metadata(&errors)?.len() > RESULT_LIMIT
            {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!(if cancelled() {
                    "CANCELLED"
                } else {
                    "alternate helper exceeded time/output/resident-memory limit"
                });
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if rss.is_none() {
                        let _ = child.kill();
                        let _ = child.wait();
                        anyhow::bail!("cannot read worker resident memory; worker terminated");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            }
        };
        ensure!(
            status.success(),
            "alternate helper exited {status}; {}",
            fs::read_to_string(&errors)
                .unwrap_or_default()
                .chars()
                .take(1000)
                .collect::<String>()
        );
        ensure!(!cancelled(), "CANCELLED");
        let mut response = read_response(&output, class)?;
        verify_input(input, state.identity.as_ref().unwrap(), cancelled)?;
        for method in &mut response.methods {
            if let Some(java) = &method.java
                && let Some(reason) = output_rejection(java)
                && !method.diagnostics.contains(&reason)
            {
                method.diagnostics.push(reason);
            }
        }
        // Cache every nonempty requested result, including unverified text and diagnostics.
        // Missing methods and actual backend/protocol errors remain retryable.
        let mut accepted = previous;
        for method in &response.methods {
            if requested.contains(&method.id)
                && method.error.is_none()
                && method
                    .java
                    .as_ref()
                    .is_some_and(|java| !java.trim().is_empty())
            {
                accepted.retain(|old| old.id != method.id);
                accepted.push(method.clone());
            }
        }
        for method in &accepted {
            response.methods.retain(|current| current.id != method.id);
            response.methods.push(method.clone());
        }
        if !accepted.is_empty() {
            let saved = Response {
                protocol: 1,
                backend: BACKEND.into(),
                class: class.into(),
                methods: accepted,
            };
            let bytes = serde_json::to_vec(&saved)?;
            ensure!(
                bytes.len() as u64 <= RESULT_LIMIT,
                "accepted alternate result exceeds 8 MiB"
            );
            if let Some(index) = state.entries.iter().position(|(id, _)| id == &key) {
                state.entries.remove(index);
            }
            while state.entries.iter().map(|(_, bytes)| *bytes).sum::<u64>() + bytes.len() as u64
                > CACHE_LIMIT
            {
                if let Some((old, _)) = state.entries.pop_front() {
                    fs::remove_file(directory.join(format!("{old}.json")))?;
                } else {
                    break;
                }
            }
            let ready = work.path().join("accepted.json");
            fs::write(&ready, &bytes)?;
            fs::rename(&ready, &cached)?;
            state.entries.push_back((key, bytes.len() as u64));
        }
        Ok((response, false, (peak_rss, peak_combined)))
    }

    pub fn apply(
        &self,
        request: ClassInput<'_>,
        code: &mut DecompiledCode,
        coverage: &mut Value,
        cancelled: &impl Fn() -> bool,
    ) -> Result<()> {
        let ClassInput {
            input,
            container,
            owner: class,
            metadata,
        } = request;
        let origin = metadata.symbols.origin.get().map_or("", String::as_str);
        if !self.enabled() || coverage["counts"]["dex-fallback"].as_u64().unwrap_or(0) == 0 {
            return Ok(());
        }
        ensure!(!cancelled(), "CANCELLED");
        if let Some(methods) = coverage["methods"].as_array_mut() {
            for report in methods {
                if report["status"] != "dex-fallback" {
                    continue;
                }
                let eligible = metadata
                    .methods
                    .iter()
                    .find(|method| {
                        report["method_id"].as_str()
                            == Some(&crate::source_coverage::method_id(class, method))
                    })
                    .is_some_and(|method| context_compatible(metadata, method));
                if !eligible {
                    report["alternate_diagnostics"] = json!([
                        "class context may require nested/generic/name presentation rewriting"
                    ]);
                }
            }
        }
        let requested: BTreeSet<String> = coverage["methods"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|method| method["status"] == "dex-fallback")
            .filter_map(|method| method["method_id"].as_str().map(str::to_owned))
            .collect();
        if requested.is_empty() {
            return Ok(());
        }
        let result = self.obtain(input, container, origin, class, &requested, cancelled);
        ensure!(!cancelled(), "CANCELLED");
        match result {
            Ok((response, hit, (worker_rss, combined_rss))) => {
                let replaced = substitute(code, coverage, &response);
                let host_rss = resident_bytes(std::process::id()).unwrap_or(0);
                coverage["alternate"] = json!({"backend":BACKEND,"status":if replaced > 0 {"substituted"} else {"no_usable_methods"},"replaced_methods":replaced,"cache_hit":hit,"worker_peak_sampled_rss_bytes":worker_rss,"combined_peak_sampled_rss_bytes":combined_rss.max(host_rss),"host_post_substitution_rss_bytes":host_rss,"rss_sampling_ms":20});
            }
            Err(error) => {
                coverage["alternate"] =
                    json!({"backend":BACKEND,"status":"failed","error":format!("{error:#}")});
            }
        }
        // Cache/measurement metadata stays out of source so the same successful
        // document has a stable hash and native offsets on cached reopening.
        let mut text = format!(
            "// Optional Androguard: backend={BACKEND}; status={}; substituted_methods={}\n",
            coverage["alternate"]["status"],
            coverage["alternate"]["replaced_methods"]
                .as_u64()
                .unwrap_or(0)
        );
        if coverage["alternate"]["replaced_methods"]
            .as_u64()
            .unwrap_or(0)
            > 0
        {
            text.push_str("// Native failure details below describe the original native output; alternate methods are labelled inline.\n");
        }
        if let Some(error) = coverage["alternate"]["error"].as_str() {
            text.push_str(&format!(
                "// Androguard diagnostic: {}\n",
                serde_json::to_string(&error.chars().take(1200).collect::<String>())?
            ));
        }
        prepend(code, &text);
        crate::source_coverage::refresh_offsets(coverage, code);
        Ok(())
    }
}

fn read_response(path: &Path, class: &str) -> Result<Response> {
    ensure!(
        fs::metadata(path)?.len() <= RESULT_LIMIT,
        "alternate result exceeds 8 MiB"
    );
    let response: Response = serde_json::from_reader(File::open(path)?)?;
    ensure!(
        response.protocol == 1 && response.backend == BACKEND && response.class == class,
        "alternate protocol/backend/class mismatch"
    );
    let mut ids = BTreeSet::new();
    for method in &response.methods {
        ensure!(
            method.id.starts_with(&format!("{class}.")) && ids.insert(&method.id),
            "alternate duplicate/foreign method ID"
        );
        ensure!(
            method.java.is_some() != method.error.is_some(),
            "alternate ambiguous method result"
        );
    }
    Ok(response)
}

fn verify_input(
    input: &Path,
    identity: &(PathBuf, u64, Option<SystemTime>, String),
    cancelled: &impl Fn() -> bool,
) -> Result<()> {
    let metadata = fs::metadata(input)?;
    ensure!(
        metadata.len() == identity.1 && metadata.modified().ok() == identity.2,
        "PROJECT_CHANGED during alternate reconstruction"
    );
    let mut file = File::open(input)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        ensure!(!cancelled(), "CANCELLED");
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let after = fs::metadata(input)?;
    ensure!(
        after.len() == identity.1
            && after.modified().ok() == identity.2
            && format!("{:x}", hash.finalize()) == identity.3,
        "PROJECT_CHANGED during alternate reconstruction"
    );
    Ok(())
}

fn context_compatible(
    class: &crate::native_dex::DexClass,
    method: &crate::native_dex::DexMethod,
) -> bool {
    let identifier = |name: &str| {
        !name.is_empty()
            && name.chars().enumerate().all(|(i, ch)| {
                ch.is_ascii_alphabetic() || ch == '_' || ch == '$' || (i > 0 && ch.is_ascii_digit())
            })
            && ![
                "class",
                "interface",
                "enum",
                "default",
                "public",
                "private",
                "static",
                "return",
                "new",
                "this",
                "super",
                "void",
                "int",
                "long",
                "boolean",
                "switch",
                "case",
                "if",
                "else",
                "try",
                "catch",
                "throw",
                "throws",
                "null",
                "true",
                "false",
                "var",
                "record",
                "yield",
            ]
            .contains(&name)
    };
    if class.descriptor.contains('$')
        || method.name.as_ref() == "<clinit>"
        || class.fields.iter().any(|field| !identifier(&field.name))
        || class
            .methods
            .iter()
            .any(|method| !method.name.starts_with('<') && !identifier(&method.name))
    {
        return false;
    }
    if let Some(code) = &method.code {
        let Ok(decoded) = crate::native_ir::DecodedMethod::decode(code) else {
            return false;
        };
        for instruction in decoded.instructions {
            let Some(reference) = instruction.reference else {
                continue;
            };
            let name = match reference.kind {
                crate::native_ir::PoolKind::Method => class
                    .symbols
                    .methods
                    .get(reference.index as usize)
                    .and_then(|(_, _, name)| class.symbols.strings.get(*name as usize)),
                crate::native_ir::PoolKind::Field => class
                    .symbols
                    .fields
                    .get(reference.index as usize)
                    .and_then(|(_, _, name)| class.symbols.strings.get(*name as usize)),
                _ => None,
            };
            if name.is_some_and(|name| name != "<init>" && !identifier(name)) {
                return false;
            }
        }
    }
    let generic_class = class
        .symbols
        .annotations
        .get(&class.annotations_offset)
        .and_then(|directory| directory.class.as_ref())
        .is_some_and(|annotations| {
            annotations.iter().any(|annotation| {
                class
                    .symbols
                    .types
                    .get(annotation.type_idx as usize)
                    .is_some_and(|ty| ty.as_ref() == "Ldalvik/annotation/Signature;")
            })
        });
    !generic_class
}

fn prepend(code: &mut DecompiledCode, text: &str) {
    let count = text.chars().count();
    code.source.insert_str(0, text);
    for link in &mut code.links {
        link.start += count;
        link.end += count;
    }
    for definition in &mut code.definitions {
        definition.start += count;
        definition.end += count;
    }
}

/// Reject a known upstream failure shape. This is a lexical sanity check only,
/// not a Java compiler or a proof of semantic equivalence.
fn output_rejection(java: &str) -> Option<String> {
    if java.trim().is_empty() {
        return Some("backend emitted empty Java".into());
    }
    if [
        "<clinit>",
        "/*",
        "//",
        "goto ",
        "UnsupportedOperationException",
        "TODO",
        "unimplemented",
        "unknown",
    ]
    .iter()
    .any(|marker| java.contains(marker))
    {
        return Some("backend emitted placeholder, comment or unsupported IR".into());
    }
    static GENERATED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static DECLARED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static LITERALS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static CONTINUED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let literals = LITERALS
        .get_or_init(|| regex::Regex::new(r#""(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'"#).unwrap());
    let text = literals.replace_all(java, " ");
    let generated = GENERATED.get_or_init(|| regex::Regex::new(r"\b(?:(?:v|p|arr|i|l|f|d|b|s|c|z|obj|sb|intent|uri|bundle|ctx|view|cls|ex|list|map|set|local)[0-9]+(?:_[0-9]+)*|result|length(?:_[0-9]+)?)\b").unwrap());
    let declared = DECLARED.get_or_init(|| regex::Regex::new(r"(?:^|[({;,:])\s*(?:final\s+)?([A-Za-z_$][\w.$]*(?:<[^;{}()]*>)?(?:\[\])*)\s+([A-Za-z_$][\w$]*)\b").unwrap());
    if let Some((_, parameters)) = text.split_once('(').and_then(|(head, rest)| {
        rest.split_once(')')
            .map(|(parameters, _)| (head, parameters))
    }) {
        let parameters = format!("({parameters}");
        let parameter_names: BTreeSet<_> = declared
            .captures_iter(&parameters)
            .map(|capture| capture[2].to_owned())
            .collect();
        if let Some(body) = text.split_once('{').map(|(_, body)| body) {
            let body = format!("{{{body}");
            if let Some(name) = declared
                .captures_iter(&body)
                .filter(|capture| {
                    ![
                        "return", "throw", "new", "case", "else", "break", "continue",
                    ]
                    .contains(&&capture[1])
                })
                .map(|capture| capture[2].to_owned())
                .find(|name| parameter_names.contains(name))
            {
                return Some(format!("backend redeclares method parameter: {name}"));
            }
        }
    }
    let mut names: BTreeSet<_> = declared
        .captures_iter(&text)
        .filter(|capture| {
            ![
                "return",
                "throw",
                "new",
                "case",
                "else",
                "break",
                "continue",
                "public",
                "private",
                "protected",
                "static",
            ]
            .contains(&&capture[1])
        })
        .map(|capture| capture[2].to_owned())
        .collect();
    let continued =
        CONTINUED.get_or_init(|| regex::Regex::new(r",\s*([A-Za-z_$][\w$]*)\s*[=,;]").unwrap());
    names.extend(
        continued
            .captures_iter(&text)
            .map(|capture| capture[1].to_owned()),
    );
    for found in generated.find_iter(&text) {
        let before = text[..found.start()].trim_end();
        let after = text[found.end()..].trim_start();
        if before.ends_with('.') || after.starts_with('(') || names.contains(found.as_str()) {
            continue;
        }
        return Some(format!(
            "backend output uses an undeclared generated register/local: {}",
            found.as_str()
        ));
    }
    None
}

/// Native method subrange within a tagged payload. Only blank/comment detail
/// lines may precede the exact original .method block.
pub fn original_dex_method_offset(payload: &str) -> Option<usize> {
    let mut offset = 0;
    for line in payload.split_inclusive('\n') {
        if line.starts_with(".method ") {
            return payload.ends_with("\n.end method\n").then_some(offset);
        }
        if !line.trim().is_empty() && !line.trim_start().starts_with("//") {
            return None;
        }
        offset += line.len();
    }
    None
}

fn substitute(code: &mut DecompiledCode, coverage: &mut Value, response: &Response) -> usize {
    let mut replacements = Vec::new();
    let requested: BTreeSet<_> = coverage["methods"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|method| method["status"] == "dex-fallback")
        .filter_map(|method| {
            method["declaration_offset"]["start"]
                .as_u64()
                .map(|offset| offset as usize)
        })
        .collect();
    let mut bounds = BTreeMap::new();
    for (offset, (byte, _)) in code.source.char_indices().enumerate() {
        if requested.contains(&offset) {
            bounds.insert(offset, byte);
        }
        if bounds.len() == requested.len() {
            break;
        }
    }
    let Some(methods) = coverage["methods"].as_array_mut() else {
        return 0;
    };
    for method in methods {
        if method["status"] != "dex-fallback" {
            continue;
        }
        let Some(alternate) = response
            .methods
            .iter()
            .find(|candidate| method["method_id"].as_str() == Some(&candidate.id))
        else {
            continue;
        };
        if let Some(reason) = &alternate.error {
            method["alternate_skip_reason"] = json!(reason);
            continue;
        }
        let Some(java) = alternate
            .java
            .as_ref()
            .filter(|java| !java.trim().is_empty())
        else {
            continue;
        };
        let mut diagnostics: Vec<String> = method["alternate_diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect();
        diagnostics.extend(alternate.diagnostics.iter().cloned());
        if let Some(reason) = output_rejection(java) {
            diagnostics.push(reason);
        }
        diagnostics.sort();
        diagnostics.dedup();
        method["alternate_diagnostics"] = json!(diagnostics);
        let Some(start) = method["declaration_offset"]["start"]
            .as_u64()
            .and_then(|n| bounds.get(&(n as usize)))
            .copied()
        else {
            continue;
        };
        let begin = code.source[..start].rfind('\n').map_or(0, |n| n + 1);
        if code.source[begin..start].trim() != ".method" {
            continue;
        }
        let Some(end) = code.source[start..]
            .find("\n.end method\n")
            .map(|n| start + n + "\n.end method\n".len())
        else {
            continue;
        };
        // Preserve exactly this original method, bounded by the existing native
        // document's method range. No whole-class DEX duplication or guessed links.
        let original = &code.source[begin..end];
        let diagnostic_lines: String = diagnostics
            .iter()
            .map(|reason| {
                format!(
                    "// Androguard diagnostic: {}\n",
                    serde_json::to_string(reason).unwrap()
                )
            })
            .collect();
        let payload = format!(
            "// method_id={}; native_failure={}; original DEX: dex://{}\n{}{original}",
            serde_json::to_string(&alternate.id).unwrap(),
            method["reason"],
            response.class,
            diagnostic_lines,
        );
        let text = format!(
            "// {UNVERIFIED_WARNING}\n{ORIGINAL_DEX_BEGIN}{}\n{payload}{ORIGINAL_DEX_END}\n{java}\n",
            payload.len()
        );
        method["original_dex"] = json!({"method_id":alternate.id,"preserved":true,"inline_bytes":original.len(),"folded_by_default":true,"source_links":false});
        replacements.push((begin, end, text));
        method["origin"] = json!(BACKEND);
        method["representation"] = json!("alternate_java_unverified");
        method["native_status"] = json!("dex-fallback");
        method["native_failure_reason"] = method["reason"].clone();
        method["status"] = json!("alternate-decompiled");
        method["declaration_offset"] = Value::Null;
    }
    replacements.sort_by_key(|(start, _, _)| *start);
    for (begin, end, text) in replacements.iter().rev() {
        let start = code.source[..*begin].chars().count();
        let finish = code.source[..*end].chars().count();
        let delta = text.chars().count() as isize - (finish - start) as isize;
        code.links
            .retain(|link| link.end <= start || link.start >= finish);
        code.definitions
            .retain(|definition| definition.end <= start || definition.start >= finish);
        for link in &mut code.links {
            if link.start >= finish {
                link.start = link.start.saturating_add_signed(delta);
                link.end = link.end.saturating_add_signed(delta);
            }
        }
        for definition in &mut code.definitions {
            if definition.start >= finish {
                definition.start = definition.start.saturating_add_signed(delta);
                definition.end = definition.end.saturating_add_signed(delta);
            }
        }
        code.source.replace_range(*begin..*end, text);
    }
    // Native reconstruction quality and displayed representations are separate.
    coverage["native_counts"] = coverage["counts"].clone();
    let native_fallbacks = coverage["counts"]["dex-fallback"].as_u64().unwrap_or(0);
    coverage["counts"]["dex-fallback"] =
        json!(native_fallbacks.saturating_sub(replacements.len() as u64));
    coverage["counts"]["alternate-decompiled"] = json!(replacements.len());
    coverage["native_java_complete"] = coverage["java_complete"].clone();
    // Nonempty alternate output is displayable, not proof of Java validity.
    coverage["displayed_java_complete"] = if replacements.is_empty() {
        coverage["java_complete"].clone()
    } else {
        Value::Null
    };
    coverage["alternate_output_verified"] = json!(false);
    coverage["counts_basis"] =
        json!("displayed_representations; java_complete retains native reconstruction status");
    replacements.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CodeDefinition, CodeLink};

    fn response(class: &str, methods: Vec<Method>) -> Response {
        Response {
            protocol: 1,
            backend: BACKEND.into(),
            class: class.into(),
            methods,
        }
    }
    fn candidate(id: &str, java: &str) -> Method {
        Method {
            id: id.into(),
            java: Some(java.into()),
            error: None,
            diagnostics: vec![],
        }
    }
    #[test]
    fn exact_overload_substitution_preserves_native_text_and_navigation() {
        let good = "    public java.lang.String same(java.lang.String text) { return text; }\n";
        let raw = ".method sample.Target.same(I)I // access=0x9\n    unknown-opcode\n.end method\n";
        let source = format!("// α\n{good}{raw}    public int later() {{ return 7; }}\n");
        let raw_start = source.find(".method").unwrap();
        let raw_start_chars = source[..raw_start].chars().count();
        let good_start = source.find("same(").unwrap();
        let good_chars = source[..good_start].chars().count();
        let later_chars = source[..source.find("later()").unwrap()].chars().count();
        let mut code = DecompiledCode {
            source,
            source_hash: String::new(),
            links: vec![
                CodeLink {
                    start: good_chars,
                    end: good_chars + 4,
                    label: "sample.Target.same(Ljava/lang/String;)Ljava/lang/String;".into(),
                },
                CodeLink {
                    start: later_chars,
                    end: later_chars + 5,
                    label: "sample.Target.later()I".into(),
                },
            ],
            definitions: vec![CodeDefinition {
                start: raw_start_chars + 8,
                end: raw_start_chars + 8 + "sample.Target.same(I)I".len(),
                kind: "method".into(),
                name: "same".into(),
            }],
        };
        let mut coverage = json!({"counts":{"dex-fallback":1,"decompiled":2},"java_complete":false,"methods":[
            {"method_id":"sample.Target.same(I)I","status":"dex-fallback","reason":"original reason","declaration_offset":{"start":raw_start_chars+8}},
            {"method_id":"sample.Target.same(Ljava/lang/String;)Ljava/lang/String;","status":"decompiled"}
        ]});
        let other = "    public java.lang.String same(java.lang.String s0) { return \"wrong\"; }\n";
        let result = response(
            "sample.Target",
            vec![
                candidate(
                    "sample.Target.same(Ljava/lang/String;)Ljava/lang/String;",
                    other,
                ),
                candidate(
                    "sample.Target.same(I)I",
                    "    public int same(int i0) { return i0 + 1; }\n",
                ),
            ],
        );
        assert_eq!(substitute(&mut code, &mut coverage, &result), 1);
        assert!(code.source.contains(good));
        assert_eq!(code.source.matches(UNVERIFIED_WARNING).count(), 1);
        assert!(!code.source.contains(other));
        assert!(
            code.source.contains(raw),
            "exact original DEX remains available"
        );
        assert!(code.source.contains("original reason"));
        assert_eq!(coverage["methods"][0]["reason"], "original reason");
        assert_eq!(coverage["methods"][0]["declaration_offset"], Value::Null);
        assert!(code.definitions.is_empty());
        assert_eq!(code.links.len(), 2);
        let chars: Vec<_> = code.source.chars().collect();
        assert_eq!(
            chars[code.links[1].start..code.links[1].end]
                .iter()
                .collect::<String>(),
            "later"
        );
    }

    #[test]
    fn diagnostic_java_is_displayed_with_exact_original_dex_and_warning() {
        let raw = ".method sample.A.<clinit>()V // access=0x8\n    return-void\n.end method\n";
        let java = "/* incomplete */ static void <clinit>() { kotlin.Result.bad-impl(v9); }";
        let mut code = DecompiledCode {
            source: raw.into(),
            source_hash: String::new(),
            links: vec![],
            definitions: vec![],
        };
        let mut coverage = json!({"counts":{"dex-fallback":1},"java_complete":false,"methods":[{"method_id":"sample.A.<clinit>()V","status":"dex-fallback","reason":"native failure","declaration_offset":{"start":8},"alternate_diagnostics":["class context requires rewriting"]}]});
        let result = response("sample.A", vec![candidate("sample.A.<clinit>()V", java)]);
        assert_eq!(substitute(&mut code, &mut coverage, &result), 1);
        assert!(code.source.contains(java));
        assert!(code.source.contains(raw));
        let warning = code.source.find(UNVERIFIED_WARNING).unwrap();
        let fold = code.source.find(ORIGINAL_DEX_BEGIN).unwrap();
        let reconstructed = code.source.find(java).unwrap();
        assert!(warning < fold && fold < reconstructed);
        assert!(
            !coverage["methods"][0]["alternate_diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(coverage["native_counts"]["dex-fallback"], 1);
        assert_eq!(
            coverage["methods"][0]["native_failure_reason"],
            "native failure"
        );
        assert_eq!(
            coverage["methods"][0]["original_dex"]["inline_bytes"],
            raw.len()
        );
        assert!(code.links.is_empty() && code.definitions.is_empty());
    }

    #[test]
    fn constructors_use_exact_ids_and_context_collisions_stay_dex() {
        let source =
            ".method sample.Target.<init>(I)V // access=0x1\n    return-void\n.end method\n";
        let mut code = DecompiledCode {
            source: source.into(),
            source_hash: String::new(),
            links: vec![],
            definitions: vec![],
        };
        let mut coverage = json!({"counts":{"dex-fallback":1},"methods":[{"method_id":"sample.Target.<init>(I)V","status":"dex-fallback","declaration_offset":{"start":8},"reason":"constructor native reason"}]});
        let result = response(
            "sample.Target",
            vec![
                candidate(
                    "sample.Target.<init>()V",
                    "    public Target() { super(); }\n",
                ),
                candidate(
                    "sample.Target.<init>(I)V",
                    "    public Target(int i0) { super(); }\n",
                ),
            ],
        );
        assert_eq!(substitute(&mut code, &mut coverage, &result), 1);
        assert!(code.source.contains("Target(int i0)"));
        let mut class =
            crate::native_dex::parse(include_bytes!("../tests/fixtures/navigation.dex"))
                .unwrap()
                .classes
                .into_iter()
                .find(|class| class.descriptor.as_ref() == "Lsample/Target;")
                .unwrap();
        assert!(context_compatible(&class, &class.methods[0]));
        class.fields[0].name = "class".into();
        assert!(!context_compatible(&class, &class.methods[0]));
    }

    #[test]
    fn referenced_member_names_requiring_rewriting_keep_native_dex() {
        let mut class =
            crate::native_dex::parse(include_bytes!("../tests/fixtures/androguard_context.dex"))
                .unwrap()
                .classes
                .into_iter()
                .find(|class| class.descriptor.as_ref() == "Lsample/Collision;")
                .unwrap();
        let method = class
            .methods
            .iter()
            .position(|method| method.name.as_ref() == "external")
            .unwrap();
        assert!(context_compatible(&class, &class.methods[method]));
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        let name = symbols
            .methods
            .iter()
            .find(|(_, _, name)| symbols.strings[*name as usize] == "getTime")
            .unwrap()
            .2;
        symbols.strings[name as usize] = "exceptionOrNull-impl".into();
        assert!(!context_compatible(&class, &class.methods[method]));
    }

    #[cfg(unix)]
    fn helper(directory: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("helper");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn private_cache_reuses_success_retries_errors_and_cleans_up() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.dex");
        fs::write(&input, include_bytes!("../tests/fixtures/hello.dex")).unwrap();
        let result = serde_json::to_string(&response(
            "sample.Hello",
            vec![candidate(
                "sample.Hello.answer()I",
                "int answer() { return 42; }",
            )],
        ))
        .unwrap();
        let helper_exe = helper(directory.path(), &format!("printf '%s' '{}'", result));
        let session = Session {
            helper: Some(helper_exe.clone()),
            ..Session::default()
        };
        let (first, hit, _) = session
            .obtain(
                &input,
                None,
                "",
                "sample.Hello",
                &BTreeSet::from(["sample.Hello.answer()I".into()]),
                &|| false,
            )
            .unwrap();
        assert!(!hit && first.methods.len() == 1);
        let cache = session
            .state
            .lock()
            .unwrap()
            .directory
            .as_ref()
            .unwrap()
            .path()
            .to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&cache).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        fs::remove_file(helper_exe).unwrap();
        // The executable remains required for this experimental backend, so a
        // missing helper is honest even if a previous session result exists.
        assert!(
            session
                .obtain(
                    &input,
                    None,
                    "",
                    "sample.Hello",
                    &BTreeSet::from(["sample.Hello.answer()I".into()]),
                    &|| false
                )
                .is_err()
        );
        let _ = helper(directory.path(), "exit 3");
        assert!(
            session
                .obtain(
                    &input,
                    None,
                    "",
                    "sample.Hello",
                    &BTreeSet::from(["sample.Hello.answer()I".into()]),
                    &|| false
                )
                .unwrap()
                .1
        );
        fs::write(&input, b"changed").unwrap();
        assert!(
            session
                .obtain(
                    &input,
                    None,
                    "",
                    "sample.Hello",
                    &BTreeSet::from(["sample.Hello.answer()I".into()]),
                    &|| false
                )
                .unwrap_err()
                .to_string()
                .contains("PROJECT_CHANGED")
        );
        drop(session);
        assert!(!cache.exists());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn cancellation_timeout_crash_and_malformed_output_leave_no_success_cache() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.dex");
        fs::write(&input, b"dex").unwrap();
        for body in ["exit 7", "printf malformed", "exec sleep 2"] {
            let helper = helper(directory.path(), body);
            let session = Session {
                helper: Some(helper),
                timeout: Some(Duration::from_millis(60)),
                ..Session::default()
            };
            assert!(
                session
                    .obtain(
                        &input,
                        None,
                        "",
                        "C",
                        &BTreeSet::from(["C.a()V".into()]),
                        &|| false
                    )
                    .is_err()
            );
            let state = session.state.lock().unwrap();
            assert!(state.entries.is_empty());
            assert_eq!(
                fs::read_dir(state.directory.as_ref().unwrap())
                    .unwrap()
                    .count(),
                0
            );
        }
        let session = Session::default();
        assert!(
            session
                .obtain(
                    &input,
                    None,
                    "",
                    "C",
                    &BTreeSet::from(["C.a()V".into()]),
                    &|| true
                )
                .is_err()
        );
        let helper = helper(directory.path(), "exec sleep 2");
        let session = Session {
            helper: Some(helper),
            ..Session::default()
        };
        let started = Instant::now();
        let error = session
            .obtain(
                &input,
                None,
                "",
                "C",
                &BTreeSet::from(["C.a()V".into()]),
                &|| started.elapsed() > Duration::from_millis(40),
            )
            .unwrap_err();
        assert!(error.to_string().contains("CANCELLED"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn known_quality_issues_produce_diagnostics() {
        assert!(output_rejection("public int identity(int i0) { return i0; }").is_none());
        assert!(
            output_rejection("public C(int i2, Object p3) { int p3 = i2 & 1; int i2 = 0; }")
                .is_some_and(|reason| reason.contains("redeclares"))
        );
        assert!(
            output_rejection(
                "public int sum(int i0, int i1) { int i2=1, i3=2; return i0+i1+i2+i3; }"
            )
            .is_none()
        );
        assert!(output_rejection("public int missing() { return i0; }").is_some());
        assert!(
            output_rejection(
                "public long external(java.util.Date p0) { return p0.getTime() + l1; }"
            )
            .is_some()
        );
        assert!(output_rejection("public int same(int i0) { return this.base + i0_0; }").is_some());
        assert!(output_rejection("public int same(int i0) { return this.base + i0; }").is_none());
        assert!(output_rejection("public java.lang.String same(java.lang.String s0) { return new java.lang.StringBuilder().append(s0).toString(); }").is_none());
    }

    #[test]
    #[ignore = "requires the separately built release helper"]
    fn real_backend_caches_unverified_requested_methods_with_diagnostics() {
        let session = Session::default();
        let input = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/androguard_context.dex"
        ));
        let good = "sample.Collision.same(Ljava/lang/String;)Ljava/lang/String;".to_owned();
        let bad = "sample.Collision.same(I)I".to_owned();
        let requested = BTreeSet::from([good.clone()]);
        let began = Instant::now();
        let (_, hit, memory) = session
            .obtain(&input, None, "", "sample.Collision", &requested, &|| false)
            .unwrap();
        let first = began.elapsed();
        assert!(!hit);
        let began = Instant::now();
        let (cached, hit, _) = session
            .obtain(&input, None, "", "sample.Collision", &requested, &|| false)
            .unwrap();
        assert!(hit);
        assert_eq!(cached.methods.len(), 1);
        assert_eq!(cached.methods[0].id, good);
        eprintln!(
            "real accepted cache: first={first:?}; cached={:?}; worker_peak_sampled_rss={}; combined_peak_sampled_rss={}",
            began.elapsed(),
            memory.0,
            memory.1
        );
        let requested = BTreeSet::from([good, bad.clone()]);
        for attempt in 0..2 {
            let (result, hit, _) = session
                .obtain(&input, None, "", "sample.Collision", &requested, &|| false)
                .unwrap();
            assert_eq!(hit, attempt != 0, "nonempty unverified output is reusable");
            assert!(result.methods.iter().any(|method| {
                method.id == bad
                    && method
                        .java
                        .as_ref()
                        .is_some_and(|java| java.contains("i0_0"))
                    && method
                        .diagnostics
                        .iter()
                        .any(|reason| reason.contains("i0_0"))
            }));
            let state = session.state.lock().unwrap();
            let entry = fs::read_dir(state.directory.as_ref().unwrap())
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let saved = read_response(&entry, "sample.Collision").unwrap();
            assert_eq!(saved.methods.len(), 2);
        }
    }

    #[test]
    fn ordinary_search_and_native_export_never_request_the_helper() {
        let directory = tempfile::tempdir().unwrap();
        let mut bytes = include_bytes!("../tests/fixtures/hello.dex").to_vec();
        let parsed = crate::native_dex::parse(&bytes).unwrap();
        let method = &parsed.classes[0].methods[0];
        let offset = method.code.as_ref().unwrap().offset as usize + 16;
        bytes[offset..offset + 2].copy_from_slice(&0x00ffu16.to_le_bytes());
        let path = directory.path().join("unsupported.dex");
        fs::write(&path, bytes).unwrap();
        let mut engine = crate::engine::NativeEngine::default();
        engine
            .open_with_limits(&path, crate::load_limits::LoadLimits::Standard)
            .unwrap();
        engine.set_alternate_source_enabled(true);
        let cancel = AtomicBool::new(false);
        for source in [
            engine
                .decompile_search_cancellable("sample.Hello", &cancel)
                .unwrap()
                .source,
            engine.decompile_source("sample.Hello").unwrap().source,
        ] {
            assert!(!source.contains("Optional Androguard"));
            assert!(source.contains(".method sample.Hello.answer()I"));
        }
        let (_, coverage) = engine
            .decompile_native_class_source_report("sample.Hello", &cancel)
            .unwrap();
        assert!(coverage["alternate"].is_null());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn disk_and_result_limits_are_enforced() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.dex");
        fs::write(&input, b"dex").unwrap();
        let result =
            serde_json::to_string(&response("C", vec![candidate("C.a()V", "void a() { }")]))
                .unwrap();
        let executable = helper(directory.path(), &format!("printf '%s' '{result}'"));
        let session = Session {
            helper: Some(executable),
            ..Session::default()
        };
        let private = tempfile::tempdir().unwrap();
        let old = private.path().join("old.json");
        File::create(&old).unwrap().set_len(CACHE_LIMIT).unwrap();
        {
            let mut state = session.state.lock().unwrap();
            state.directory = Some(private);
            state.entries.push_back(("old".into(), CACHE_LIMIT));
        }
        session
            .obtain(
                &input,
                None,
                "",
                "C",
                &BTreeSet::from(["C.a()V".into()]),
                &|| false,
            )
            .unwrap();
        assert!(!old.exists());
        let state = session.state.lock().unwrap();
        assert!(state.entries.iter().map(|(_, bytes)| *bytes).sum::<u64>() <= CACHE_LIMIT);
        drop(state);
        let executable = helper(directory.path(), "exec head -c 8388609 /dev/zero");
        let session = Session {
            helper: Some(executable),
            ..Session::default()
        };
        assert!(
            session
                .obtain(
                    &input,
                    None,
                    "",
                    "C",
                    &BTreeSet::from(["C.a()V".into()]),
                    &|| false
                )
                .is_err()
        );
        assert!(session.state.lock().unwrap().entries.is_empty());
    }
}
