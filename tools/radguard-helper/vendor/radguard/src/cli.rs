use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::writer::SourceOutput;
use crate::{Decompiler, DexFile};

/// Structural statistics collected while parsing a DEX file.
#[derive(Clone, Debug)]
pub struct InspectSummary {
    pub version: u16,
    pub strings: usize,
    pub types: usize,
    pub protos: usize,
    pub fields: usize,
    pub methods: usize,
    pub classes: usize,
    pub encoded_fields: usize,
    pub encoded_methods: usize,
    pub parse_duration: std::time::Duration,
}

/// Thread-safe callback invoked for APK export lifecycle events.
///
/// Directory-export callbacks may run concurrently on pipeline workers and
/// must not assume event ordering by DEX number.
pub type ProgressCallback = Arc<dyn Fn(ExportProgress) + Send + Sync + 'static>;

/// Thread-safe predicate that returns `true` for class descriptors (e.g. `Lcom/pkg/Cls;`) to export.
pub type ClassFilter = Arc<dyn Fn(&str) -> bool + Send + Sync + 'static>;

/// Options shared by directory and `RGOUT001` exports.
///
/// The default output includes DEX metadata annotations and runs sequentially.

#[derive(Clone)]
pub struct ExportOptions {
    pub thread_count: Option<usize>,
    pub include_metadata: bool,
    pub debug: bool,
    pub progress_callback: Option<ProgressCallback>,
    pub class_filter: Option<ClassFilter>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            thread_count: None,
            include_metadata: true,
            debug: false,
            progress_callback: None,
            class_filter: None,
        }
    }
}

impl ExportOptions {
    /// Uses half of the logical CPUs reported as available to this process.
    /// At least one worker is always selected.
    pub fn parallel() -> Self {
        Self {
            thread_count: Some(default_thread_count()),
            ..Self::default()
        }
    }

    /// Uses a global budget of exactly `thread_count` pipeline workers.
    ///
    /// For APK directory export, workers share DEX inflate/parse and class
    /// decompile/write tasks. Each individual DEX uses at most eight concurrent
    /// class tasks, while different DEX files may be decompiled concurrently.
    /// Zero is rejected by export calls.
    pub const fn with_threads(mut self, thread_count: usize) -> Self {
        self.thread_count = Some(thread_count);
        self
    }

    /// Omits DEX annotations from rendered source.
    pub const fn without_metadata(mut self) -> Self {
        self.include_metadata = false;
        self
    }

    /// Emits CFG and structure-recovery diagnostics to stderr.
    pub const fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    /// Invokes `callback` for APK pipeline lifecycle and progress events.
    pub fn with_progress<F>(mut self, callback: F) -> Self
    where
        F: Fn(ExportProgress) + Send + Sync + 'static,
    {
        self.progress_callback = Some(Arc::new(callback));
        self
    }

    /// Filters exported classes using a thread-safe descriptor predicate.
    pub fn with_class_filter<F>(mut self, filter: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        self.class_filter = Some(Arc::new(filter));
        self
    }

    fn validated_threads(&self) -> crate::Result<Option<usize>> {
        if self.thread_count == Some(0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "thread count must be greater than zero",
            )
            .into());
        }
        Ok(self.thread_count)
    }
}

/// Progress events emitted while exporting an APK.
///
/// `DexInflated` follows successful decompression, CRC verification, parsing,
/// and class-task creation. `DexWorking` is emitted once when a DEX's first
/// class task starts. Events may be delivered concurrently and out of DEX order.
#[derive(Clone, Copy, Debug)]
pub enum ExportProgress {
    Started {
        total_dex_files: usize,
        worker_count: usize,
    },
    DexInflated {
        inflated_dex_files: usize,
        total_dex_files: usize,
        dex_number: u32,
        inflate_workers: usize,
    },
    Activity {
        busy_workers: usize,
        idle_workers: usize,
        decompile_workers: usize,
        decompile_dex_files: usize,
        inflate_workers: usize,
        queued_dex_files: usize,
        ready_dex_files: usize,
        total_dex_files: usize,
    },
    DexWorking {
        started_dex_files: usize,
        total_dex_files: usize,
        dex_number: u32,
        busy_workers: usize,
        idle_workers: usize,
        decompile_workers: usize,
        decompile_dex_files: usize,
        inflate_workers: usize,
    },
    DexCompleted {
        completed_dex_files: usize,
        total_dex_files: usize,
        dex_number: u32,
        dex_duration: std::time::Duration,
        busy_workers: usize,
        idle_workers: usize,
        decompile_workers: usize,
        decompile_dex_files: usize,
        inflate_workers: usize,
    },
}

impl ExportProgress {
    pub fn percentage(self) -> f64 {
        match self {
            Self::Started { .. } | Self::Activity { .. } => 0.0,
            Self::DexInflated {
                inflated_dex_files,
                total_dex_files,
                ..
            } => inflated_dex_files as f64 * 100.0 / total_dex_files as f64,
            Self::DexWorking {
                started_dex_files,
                total_dex_files,
                ..
            } => started_dex_files as f64 * 100.0 / total_dex_files as f64,
            Self::DexCompleted {
                completed_dex_files,
                total_dex_files,
                ..
            } => completed_dex_files as f64 * 100.0 / total_dex_files as f64,
        }
    }
}

/// Aggregate result from an APK or DEX export.
#[derive(Clone, Copy, Debug)]
pub struct ExportSummary {
    pub dex_files: usize,
    pub classes: usize,
    pub duration: std::time::Duration,
}

/// Parses a DEX and returns its structural summary without decompiling it.
pub fn inspect_file(path: impl AsRef<Path>) -> crate::Result<InspectSummary> {
    let bytes = std::fs::read(path)?;
    let started = std::time::Instant::now();
    let dex = DexFile::parse(&bytes)?;
    let parse_duration = started.elapsed();
    Ok(InspectSummary {
        version: dex.header.version,
        strings: dex.strings().len(),
        types: dex.type_ids().len(),
        protos: dex.protos().len(),
        fields: dex.field_ids().len(),
        methods: dex.method_ids().len(),
        classes: dex.classes().len(),
        encoded_fields: dex
            .classes()
            .iter()
            .map(|class| class.fields().count())
            .sum(),
        encoded_methods: dex
            .classes()
            .iter()
            .map(|class| class.methods().count())
            .sum(),
        parse_duration,
    })
}

/// Decompiles one class from a DEX with annotations and member-reference metadata.
pub fn decompile_class_file(
    path: impl AsRef<Path>,
    class_descriptor: &str,
) -> crate::Result<Option<SourceOutput>> {
    decompile_class_file_configured(path.as_ref(), class_descriptor, true, false)
}

fn decompile_class_file_configured(
    path: &Path,
    class_descriptor: &str,
    include_metadata: bool,
    debug: bool,
) -> crate::Result<Option<SourceOutput>> {
    let bytes = std::fs::read(path)?;
    let decompiler = Decompiler::parse(&bytes)?.with_debug(debug);
    if include_metadata {
        decompiler.decompile_class(class_descriptor)
    } else {
        decompiler
            .without_metadata()
            .decompile_class(class_descriptor)
    }
}

fn print_inspect(path: &Path) -> crate::Result<()> {
    let summary = inspect_file(path)?;
    println!(
        "version={} strings={} types={} protos={} fields={} methods={} classes={} encoded_fields={} encoded_methods={} parse={:.6}s",
        summary.version,
        summary.strings,
        summary.types,
        summary.protos,
        summary.fields,
        summary.methods,
        summary.classes,
        summary.encoded_fields,
        summary.encoded_methods,
        summary.parse_duration.as_secs_f64(),
    );
    Ok(())
}

fn usage(program: &str) {
    eprintln!("usage: {program} inspect <file.dex>");
    eprintln!(
        "       {program} decompile [--no-metadata] [--debug] <file.dex> <class-descriptor>"
    );
    eprintln!(
        "       {program} export-all [--no-metadata] [--debug] <file.dex|file.apk> [output-directory]"
    );
    eprintln!(
        "       {program} export-all-parallel [--no-metadata] [--debug] <file.dex|file.apk> [thread-count] [output-directory]"
    );
    eprintln!("       (thread-count defaults to half the available logical CPU count)");
}

fn take_metadata_option(args: &mut Vec<OsString>) -> Option<bool> {
    let mut found = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == OsStr::new("--no-metadata") {
            if found.is_some() {
                return None;
            }
            found = Some(index);
        }
        index += 1;
    }
    if let Some(index) = found {
        args.remove(index);
        Some(false)
    } else {
        Some(true)
    }
}

fn take_debug_option(args: &mut Vec<OsString>) -> Option<bool> {
    let mut found = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == OsStr::new("--debug") {
            if found.is_some() {
                return None;
            }
            found = Some(index);
        }
        index += 1;
    }
    if let Some(index) = found {
        args.remove(index);
        Some(true)
    } else {
        Some(false)
    }
}

fn default_thread_count_for(available: usize) -> usize {
    (available / 2).max(1)
}

fn default_thread_count() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .map(default_thread_count_for)
        .unwrap_or(1)
}

fn parse_thread_count(value: &OsStr) -> Option<usize> {
    value
        .to_str()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|thread_count| *thread_count != 0)
}

fn java_relative_path(descriptor: &str) -> Option<PathBuf> {
    let class_name = descriptor.strip_prefix('L')?.strip_suffix(';')?;
    if class_name.is_empty()
        || class_name.split('/').any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.contains(['\\', ':', '\0'])
        })
    {
        return None;
    }
    let mut path = PathBuf::new();
    for component in class_name.split('/') {
        path.push(component);
    }
    path.set_extension("java");
    Some(path)
}

fn write_java_class(output_directory: &Path, descriptor: &str, source: &str) -> crate::Result<()> {
    let relative = java_relative_path(descriptor).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid class descriptor for export: {descriptor}"),
        )
    })?;
    let path = output_directory.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, source)?;
    Ok(())
}

fn export_dex_to_directory(
    dex_bytes: &[u8],
    output_directory: &Path,
    thread_count: Option<usize>,
    include_metadata: bool,
    debug: bool,
    class_filter: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
) -> crate::Result<usize> {
    let decompiler = Decompiler::parse(dex_bytes)?.with_debug(debug);
    let decompiler = if include_metadata {
        decompiler
    } else {
        decompiler.without_metadata()
    };
    let classes = if let Some(filter) = class_filter {
        decompiler.decompile_selected_parallel(thread_count, filter)?
    } else {
        match thread_count {
            Some(thread_count) => decompiler.decompile_all_parallel(thread_count)?,
            None => decompiler.decompile_all()?,
        }
    };
    let count = classes.len();
    for (descriptor, source) in classes {
        write_java_class(output_directory, &descriptor, &source.source)?;
    }
    Ok(count)
}

const APK_MAX_DECOMPILE_WORKERS: usize = 8;
const APK_CLASS_CHUNK_SIZE: usize = 16;

type CompletionState = Arc<(Mutex<usize>, Condvar)>;

/// Live counters for the CPU stages of one APK export. File writes are not
/// included: the CLI reports only the user-provided worker budget.
struct PipelineWorkers {
    budget: usize,
    inflating: AtomicUsize,
    inflate_completed: AtomicUsize,
    decompiling: AtomicUsize,
    decompiling_dexes: AtomicUsize,
    started_dexes: AtomicUsize,
    queued_dex: AtomicUsize,
    ready_dex: AtomicUsize,
    last_report: Mutex<Option<std::time::Instant>>,
}

impl PipelineWorkers {
    fn snapshot(&self) -> (usize, usize, usize, usize, usize, usize, usize) {
        let inflate_workers = self.inflating.load(Ordering::Relaxed);
        let decompile_workers = self.decompiling.load(Ordering::Relaxed);
        let busy_workers = inflate_workers + decompile_workers;
        (
            busy_workers,
            self.budget.saturating_sub(busy_workers),
            decompile_workers,
            self.decompiling_dexes.load(Ordering::Relaxed),
            inflate_workers,
            self.queued_dex.load(Ordering::Relaxed),
            self.ready_dex.load(Ordering::Relaxed),
        )
    }
}

fn complete_apk_dex(
    completion: &CompletionState,
    callback: Option<&(dyn Fn(ExportProgress) + Send + Sync)>,
    total_dex_files: usize,
    number: u32,
    duration: std::time::Duration,
    workers: Option<&PipelineWorkers>,
) -> usize {
    let (lock, changed) = &**completion;
    let completed = {
        let mut count = lock.lock().expect("APK completion mutex poisoned");
        *count += 1;
        let completed = *count;
        changed.notify_all();
        completed
    };
    report_apk_progress(
        callback,
        completed,
        total_dex_files,
        number,
        duration,
        workers,
    );
    completed
}

#[derive(Debug)]
struct PipelineState {
    error: Mutex<Option<crate::Error>>,
}

impl PipelineState {
    fn record(&self, error: crate::Error) {
        let mut first = self.error.lock().expect("pipeline error mutex poisoned");
        if first.is_none() {
            *first = Some(error);
        }
    }

    fn finish(self) -> crate::Result<()> {
        self.error
            .into_inner()
            .expect("pipeline error mutex poisoned")
            .map_or(Ok(()), Err)
    }
}

struct DexOutputState {
    pending_chunks: usize,
    reported_complete: bool,
}

struct ReadyDex {
    number: u32,
    started: std::time::Instant,
    decompiler: Arc<Decompiler<'static>>,
    active_decompilers: AtomicUsize,
    working_reported: AtomicBool,
    output: Mutex<DexOutputState>,
}

struct ClassJob {
    dex: Arc<ReadyDex>,
    indices: Vec<usize>,
    start_class: usize,
}

enum PipelineTask {
    Inflate(DexZipEntry),
    Decompile(ClassJob, bool),
}

struct PipelineTaskState {
    inflate: VecDeque<DexZipEntry>,
    decompile: VecDeque<ClassJob>,
    closed: bool,
    active_inflaters: usize,
    inflate_limit: usize,
}

type SharedPipelineTasks = Arc<(Mutex<PipelineTaskState>, Condvar)>;

fn pipeline_push(tasks: &SharedPipelineTasks, task: PipelineTask) {
    let (lock, changed) = &**tasks;
    let mut state = lock.lock().expect("pipeline task mutex poisoned");
    match task {
        PipelineTask::Inflate(task) => state.inflate.push_back(task),
        PipelineTask::Decompile(task, _) => state.decompile.push_back(task),
    }
    changed.notify_all();
}

fn pipeline_pop(tasks: &SharedPipelineTasks) -> Option<PipelineTask> {
    let (lock, changed) = &**tasks;
    let mut state = lock.lock().expect("pipeline task mutex poisoned");
    loop {
        // While compressed entries remain, reserve T-8 workers for preparing
        // DEX and leave at most eight workers for class work.
        if state.active_inflaters < state.inflate_limit
            && let Some(task) = state.inflate.pop_front()
        {
            state.active_inflaters += 1;
            return Some(PipelineTask::Inflate(task));
        }
        // Start one chunk from every ready DEX before assigning additional
        // lanes to an already-running DEX. This prevents a contiguous run of
        // chunks from the first few DEX files monopolizing the worker pool.
        let position = state
            .decompile
            .iter()
            .position(|task| task.dex.active_decompilers.load(Ordering::Relaxed) == 0)
            .or_else(|| {
                state.decompile.iter().position(|task| {
                    task.dex.active_decompilers.load(Ordering::Relaxed) < APK_MAX_DECOMPILE_WORKERS
                })
            });
        if let Some(position) = position {
            let task = state
                .decompile
                .remove(position)
                .expect("located decompile task disappeared");
            let previous = task.dex.active_decompilers.fetch_add(1, Ordering::Relaxed);
            assert!(
                previous < APK_MAX_DECOMPILE_WORKERS,
                "per-DEX decompile worker limit exceeded"
            );
            return Some(PipelineTask::Decompile(task, previous == 0));
        }
        if state.closed {
            return None;
        }
        state = changed.wait(state).expect("pipeline task mutex poisoned");
    }
}

fn pipeline_finish_inflate(tasks: &SharedPipelineTasks) {
    let (lock, changed) = &**tasks;
    let mut state = lock.lock().expect("pipeline task mutex poisoned");
    state.active_inflaters -= 1;
    changed.notify_all();
}

fn pipeline_finish_decompile(tasks: &SharedPipelineTasks, dex: &ReadyDex) -> bool {
    let (lock, changed) = &**tasks;
    let _state = lock.lock().expect("pipeline task mutex poisoned");
    let previous = dex.active_decompilers.fetch_sub(1, Ordering::Relaxed);
    assert!(previous != 0, "per-DEX decompile worker count underflow");
    changed.notify_all();
    previous == 1
}

fn pipeline_close(tasks: &SharedPipelineTasks) {
    let (lock, changed) = &**tasks;
    lock.lock().expect("pipeline task mutex poisoned").closed = true;
    changed.notify_all();
}

fn dex_entry_number(name: &str) -> Option<u32> {
    if name.contains('/') {
        return None;
    }
    let suffix = name.strip_prefix("classes")?.strip_suffix(".dex")?;
    if suffix.is_empty() {
        return Some(1);
    }
    let number = suffix.parse().ok()?;
    (number >= 2).then_some(number)
}

#[derive(Clone, Copy)]
struct DexZipEntry {
    number: u32,
    data_start: u64,
    compressed_size: u64,
    uncompressed_size: u64,
    crc32: u32,
    compression: u16,
}

fn zip_u16(data: &[u8], offset: usize) -> crate::Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| std::io::Error::other("truncated ZIP metadata"))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn zip_u32(data: &[u8], offset: usize) -> crate::Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| std::io::Error::other("truncated ZIP metadata"))?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("four-byte slice"),
    ))
}

fn zip_u64(data: &[u8], offset: usize) -> crate::Result<u64> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or_else(|| std::io::Error::other("truncated ZIP metadata"))?;
    Ok(u64::from_le_bytes(
        bytes.try_into().expect("eight-byte slice"),
    ))
}

fn zip64_entry_values(
    extra: &[u8],
    uncompressed_32: u32,
    compressed_32: u32,
    local_offset_32: u32,
) -> crate::Result<(u64, u64, u64)> {
    let mut offset = 0;
    while offset + 4 <= extra.len() {
        let tag = zip_u16(extra, offset)?;
        let size = usize::from(zip_u16(extra, offset + 2)?);
        let data = extra
            .get(offset + 4..offset + 4 + size)
            .ok_or_else(|| std::io::Error::other("truncated ZIP extra field"))?;
        if tag == 1 {
            let mut cursor = 0;
            let mut next = || -> crate::Result<u64> {
                let value = zip_u64(data, cursor)?;
                cursor += 8;
                Ok(value)
            };
            let uncompressed = if uncompressed_32 == u32::MAX {
                next()?
            } else {
                u64::from(uncompressed_32)
            };
            let compressed = if compressed_32 == u32::MAX {
                next()?
            } else {
                u64::from(compressed_32)
            };
            let local_offset = if local_offset_32 == u32::MAX {
                next()?
            } else {
                u64::from(local_offset_32)
            };
            return Ok((uncompressed, compressed, local_offset));
        }
        offset += 4 + size;
    }
    if uncompressed_32 == u32::MAX || compressed_32 == u32::MAX || local_offset_32 == u32::MAX {
        return Err(std::io::Error::other("missing ZIP64 entry metadata").into());
    }
    Ok((
        u64::from(uncompressed_32),
        u64::from(compressed_32),
        u64::from(local_offset_32),
    ))
}

struct ZipDirectory {
    range: std::ops::Range<usize>,
    archive_offset: usize,
}

fn zip_central_directory(apk: &[u8]) -> crate::Result<ZipDirectory> {
    const MAX_EOCD_SIZE: usize = 22 + u16::MAX as usize;
    let tail_start = apk.len().saturating_sub(MAX_EOCD_SIZE);
    let eocd = apk[tail_start..]
        .windows(4)
        .enumerate()
        .rev()
        .find_map(|(relative, signature)| {
            if signature != b"PK\x05\x06" {
                return None;
            }
            let offset = tail_start + relative;
            let comment_len = usize::from(zip_u16(apk, offset + 20).ok()?);
            (offset.checked_add(22 + comment_len) == Some(apk.len())).then_some(offset)
        })
        .ok_or_else(|| std::io::Error::other("APK has no valid ZIP end record"))?;
    let entries = zip_u16(apk, eocd + 10)?;
    let size_32 = zip_u32(apk, eocd + 12)?;
    let offset_32 = zip_u32(apk, eocd + 16)?;
    let (directory_size, directory_offset, directory_end) =
        if entries == u16::MAX || size_32 == u32::MAX || offset_32 == u32::MAX {
            let locator = eocd
                .checked_sub(20)
                .ok_or_else(|| std::io::Error::other("invalid ZIP64 locator"))?;
            if apk.get(locator..locator + 4) != Some(b"PK\x06\x07") {
                return Err(std::io::Error::other("missing ZIP64 locator").into());
            }
            let _zip64_relative = zip_u64(apk, locator + 8)?;
            let zip64 = apk[..locator]
                .windows(4)
                .rposition(|signature| signature == b"PK\x06\x06")
                .ok_or_else(|| std::io::Error::other("missing ZIP64 end record"))?;
            let size = usize::try_from(zip_u64(apk, zip64 + 40)?)
                .map_err(|_| std::io::Error::other("ZIP64 directory is too large"))?;
            let offset = usize::try_from(zip_u64(apk, zip64 + 48)?)
                .map_err(|_| std::io::Error::other("ZIP64 directory offset is too large"))?;
            (size, offset, zip64)
        } else {
            (size_32 as usize, offset_32 as usize, eocd)
        };
    let physical_start = directory_end
        .checked_sub(directory_size)
        .ok_or_else(|| std::io::Error::other("invalid ZIP central directory size"))?;
    let archive_offset = physical_start
        .checked_sub(directory_offset)
        .ok_or_else(|| std::io::Error::other("invalid ZIP archive offset"))?;
    Ok(ZipDirectory {
        range: physical_start..directory_end,
        archive_offset,
    })
}

/// Finds `classes*.dex` by scanning only the real central-directory bytes for
/// the literal `classes` name. The APK itself remains one shared memory buffer.
fn apk_dex_entries(apk: &[u8]) -> crate::Result<Vec<DexZipEntry>> {
    let directory = zip_central_directory(apk)?;
    let mut dex_entries = Vec::new();
    for relative_name_start in apk[directory.range.clone()]
        .windows(b"classes".len())
        .enumerate()
        .filter_map(|(offset, bytes)| (bytes == b"classes").then_some(offset))
    {
        let name_start = directory.range.start + relative_name_start;
        let Some(header_start) = name_start.checked_sub(46) else {
            continue;
        };
        if header_start < directory.range.start
            || apk.get(header_start..header_start + 4) != Some(b"PK\x01\x02")
        {
            continue;
        }
        let flags = zip_u16(apk, header_start + 8)?;
        let compression = zip_u16(apk, header_start + 10)?;
        let crc32 = zip_u32(apk, header_start + 16)?;
        let compressed_32 = zip_u32(apk, header_start + 20)?;
        let uncompressed_32 = zip_u32(apk, header_start + 24)?;
        let name_len = usize::from(zip_u16(apk, header_start + 28)?);
        let extra_len = usize::from(zip_u16(apk, header_start + 30)?);
        let comment_len = usize::from(zip_u16(apk, header_start + 32)?);
        let local_offset_32 = zip_u32(apk, header_start + 42)?;
        let name_end = name_start + name_len;
        let extra_end = name_end + extra_len;
        let entry_end = extra_end + comment_len;
        let Some(name) = apk.get(name_start..name_end) else {
            continue;
        };
        if entry_end > directory.range.end {
            continue;
        }
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        let Some(number) = dex_entry_number(name) else {
            continue;
        };
        if flags & 1 != 0 {
            return Err(std::io::Error::other("encrypted DEX entry is unsupported").into());
        }
        let extra = &apk[name_end..extra_end];
        let (uncompressed_size, compressed_size, local_offset) =
            zip64_entry_values(extra, uncompressed_32, compressed_32, local_offset_32)?;
        let local_offset = directory
            .archive_offset
            .checked_add(
                usize::try_from(local_offset)
                    .map_err(|_| std::io::Error::other("DEX local offset is too large"))?,
            )
            .ok_or_else(|| std::io::Error::other("DEX local offset overflow"))?;
        let local_header = apk
            .get(local_offset..local_offset + 30)
            .ok_or_else(|| std::io::Error::other("truncated ZIP local header"))?;
        if &local_header[..4] != b"PK\x03\x04" {
            return Err(std::io::Error::other("invalid ZIP local header").into());
        }
        let data_start = local_offset
            .checked_add(
                30 + usize::from(zip_u16(local_header, 26)?)
                    + usize::from(zip_u16(local_header, 28)?),
            )
            .ok_or_else(|| std::io::Error::other("DEX data offset overflow"))?;
        let data_end = data_start
            .checked_add(
                usize::try_from(compressed_size)
                    .map_err(|_| std::io::Error::other("compressed DEX is too large"))?,
            )
            .ok_or_else(|| std::io::Error::other("DEX data range overflow"))?;
        if data_end > apk.len() {
            return Err(std::io::Error::other("truncated DEX ZIP payload").into());
        }
        dex_entries.push(DexZipEntry {
            number,
            data_start: data_start as u64,
            compressed_size,
            uncompressed_size,
            crc32,
            compression,
        });
    }
    dex_entries.sort_unstable_by_key(|entry| entry.number);
    if dex_entries.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "APK contains no classes*.dex entries",
        )
        .into());
    }
    if dex_entries
        .windows(2)
        .any(|entries| entries[0].number == entries[1].number)
    {
        return Err(std::io::Error::other("APK contains duplicate classes*.dex entries").into());
    }
    Ok(dex_entries)
}

fn dex_entry_name(number: u32) -> String {
    if number == 1 {
        "classes.dex".to_owned()
    } else {
        format!("classes{number}.dex")
    }
}

fn report_pipeline_activity(
    callback: Option<&(dyn Fn(ExportProgress) + Send + Sync)>,
    workers: &PipelineWorkers,
    total_dex_files: usize,
) {
    let Some(callback) = callback else {
        return;
    };
    let now = std::time::Instant::now();
    let mut last = workers
        .last_report
        .lock()
        .expect("pipeline reporter mutex poisoned");
    if last.is_some_and(|last| now.duration_since(last) < std::time::Duration::from_millis(250)) {
        return;
    }
    *last = Some(now);
    drop(last);
    let (
        busy_workers,
        idle_workers,
        decompile_workers,
        decompile_dex_files,
        inflate_workers,
        queued_dex_files,
        ready_dex_files,
    ) = workers.snapshot();
    callback(ExportProgress::Activity {
        busy_workers,
        idle_workers,
        decompile_workers,
        decompile_dex_files,
        inflate_workers,
        queued_dex_files,
        ready_dex_files,
        total_dex_files,
    });
}

fn report_dex_inflated(
    callback: Option<&(dyn Fn(ExportProgress) + Send + Sync)>,
    workers: &PipelineWorkers,
    total_dex_files: usize,
    number: u32,
) {
    let inflated_dex_files = workers.inflate_completed.fetch_add(1, Ordering::Relaxed) + 1;
    if let Some(callback) = callback {
        callback(ExportProgress::DexInflated {
            inflated_dex_files,
            total_dex_files,
            dex_number: number,
            inflate_workers: workers.inflating.load(Ordering::Relaxed),
        });
    }
}

fn report_dex_working(
    callback: Option<&(dyn Fn(ExportProgress) + Send + Sync)>,
    workers: &PipelineWorkers,
    total_dex_files: usize,
    number: u32,
) {
    let Some(callback) = callback else {
        return;
    };
    let (busy_workers, idle_workers, decompile_workers, decompile_dex_files, inflate_workers, _, _) =
        workers.snapshot();
    let started_dex_files = workers.started_dexes.fetch_add(1, Ordering::Relaxed) + 1;
    callback(ExportProgress::DexWorking {
        started_dex_files,
        total_dex_files,
        dex_number: number,
        busy_workers,
        idle_workers,
        decompile_workers,
        decompile_dex_files,
        inflate_workers,
    });
}

fn report_apk_progress(
    callback: Option<&(dyn Fn(ExportProgress) + Send + Sync)>,
    completed: usize,
    total: usize,
    number: u32,
    duration: std::time::Duration,
    workers: Option<&PipelineWorkers>,
) {
    if let Some(callback) = callback {
        let (
            busy_workers,
            idle_workers,
            decompile_workers,
            decompile_dex_files,
            inflate_workers,
            _,
            _,
        ) = workers
            .map(PipelineWorkers::snapshot)
            .unwrap_or((0, 0, 0, 0, 0, 0, 0));
        callback(ExportProgress::DexCompleted {
            completed_dex_files: completed,
            total_dex_files: total,
            dex_number: number,
            dex_duration: duration,
            busy_workers,
            idle_workers,
            decompile_workers,
            decompile_dex_files,
            inflate_workers,
        });
    }
}

fn read_apk_dex(
    apk: &[u8],
    entry: DexZipEntry,
    decompressor: &mut libdeflater::Decompressor,
) -> crate::Result<Vec<u8>> {
    let compressed_size = usize::try_from(entry.compressed_size).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "compressed DEX is too large",
        )
    })?;
    let uncompressed_size = usize::try_from(entry.uncompressed_size)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "DEX is too large"))?;
    let data_start = usize::try_from(entry.data_start)
        .map_err(|_| std::io::Error::other("DEX data offset is too large"))?;
    let compressed = apk
        .get(data_start..data_start + compressed_size)
        .ok_or_else(|| std::io::Error::other("truncated DEX ZIP payload"))?;
    let dex_bytes = if entry.compression == 0 {
        if compressed_size != uncompressed_size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stored DEX has inconsistent sizes",
            )
            .into());
        }
        compressed.to_vec()
    } else if entry.compression == 8 {
        let mut output = vec![0; uncompressed_size];
        let written = decompressor
            .deflate_decompress(compressed, &mut output)
            .map_err(std::io::Error::other)?;
        if written != uncompressed_size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "libdeflate returned an unexpected DEX size",
            )
            .into());
        }
        output
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "unsupported DEX ZIP compression method {}",
                entry.compression
            ),
        )
        .into());
    };
    if libdeflater::crc32(&dex_bytes) != entry.crc32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DEX ZIP CRC-32 mismatch",
        )
        .into());
    }
    Ok(dex_bytes)
}

fn process_apk_parallel<F>(
    input: &Path,
    thread_count: usize,
    include_metadata: bool,
    debug: bool,
    progress_callback: Option<&(dyn Fn(ExportProgress) + Send + Sync)>,
    class_filter: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
    emit_chunk: F,
) -> crate::Result<(usize, usize)>
where
    F: Fn(u32, usize, Vec<(String, String)>) -> crate::Result<()> + Sync,
{
    let apk: Arc<[u8]> = std::fs::read(input)?.into();
    let dex_entries = apk_dex_entries(&apk)?;
    let total_dex_files = dex_entries.len();
    if let Some(callback) = progress_callback {
        callback(ExportProgress::Started {
            total_dex_files,
            worker_count: thread_count,
        });
    }

    let inflate_limit = thread_count
        .saturating_sub(thread_count.min(APK_MAX_DECOMPILE_WORKERS))
        .max(1);
    let tasks: SharedPipelineTasks = Arc::new((
        Mutex::new(PipelineTaskState {
            inflate: dex_entries.into_iter().collect(),
            decompile: VecDeque::new(),
            closed: false,
            active_inflaters: 0,
            inflate_limit,
        }),
        Condvar::new(),
    ));
    let pipeline = Arc::new(PipelineState {
        error: Mutex::new(None),
    });
    let completion: CompletionState = Arc::new((Mutex::new(0), Condvar::new()));
    let workers = Arc::new(PipelineWorkers {
        budget: thread_count,
        inflating: AtomicUsize::new(0),
        inflate_completed: AtomicUsize::new(0),
        decompiling: AtomicUsize::new(0),
        decompiling_dexes: AtomicUsize::new(0),
        started_dexes: AtomicUsize::new(0),
        queued_dex: AtomicUsize::new(total_dex_files),
        ready_dex: AtomicUsize::new(0),
        last_report: Mutex::new(None),
    });
    let class_count = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(thread_count);
        for _ in 0..thread_count {
            let tasks = Arc::clone(&tasks);
            let pipeline = Arc::clone(&pipeline);
            let completion = Arc::clone(&completion);
            let workers = Arc::clone(&workers);
            let class_count = Arc::clone(&class_count);
            let apk = Arc::clone(&apk);
            let emit_chunk = &emit_chunk;
            handles.push(scope.spawn(move || {
                let mut decompressor = libdeflater::Decompressor::new();
                while let Some(task) = pipeline_pop(&tasks) {
                    match task {
                        PipelineTask::Inflate(entry) => {
                            workers.queued_dex.fetch_sub(1, Ordering::Relaxed);
                            workers.inflating.fetch_add(1, Ordering::Relaxed);
                            report_pipeline_activity(progress_callback, &workers, total_dex_files);
                            let started = std::time::Instant::now();
                            let result =
                                read_apk_dex(&apk, entry, &mut decompressor).and_then(|bytes| {
                                    let decompiler =
                                        Decompiler::parse_owned(Arc::from(bytes))?
                                            .with_debug(debug);
                                    let decompiler = if include_metadata {
                                        decompiler
                                    } else {
                                        decompiler.without_metadata()
                                    };
                                    let decompiler = Arc::new(decompiler);
                                    let all_classes = decompiler.dex().classes();
                                    let selected_indices: Vec<usize> = if let Some(filter) = class_filter {
                                        let mut indices = Vec::new();
                                        for (idx, class) in all_classes.iter().enumerate() {
                                            let descriptor = decompiler.dex().class_descriptor(class)?;
                                            if filter(descriptor) {
                                                indices.push(idx);
                                            }
                                        }
                                        indices
                                    } else {
                                        (0..all_classes.len()).collect()
                                    };
                                    let chunks = selected_indices.len().div_ceil(APK_CLASS_CHUNK_SIZE);
                                    if chunks == 0 {
                                        return Ok(false);
                                    }
                                    workers.ready_dex.fetch_add(1, Ordering::Relaxed);
                                    let dex = Arc::new(ReadyDex {
                                        number: entry.number,
                                        started,
                                        decompiler,
                                        active_decompilers: AtomicUsize::new(0),
                                        working_reported: AtomicBool::new(false),
                                        output: Mutex::new(DexOutputState {
                                            pending_chunks: chunks,
                                            reported_complete: false,
                                        }),
                                    });
                                    for chunk in selected_indices.chunks(APK_CLASS_CHUNK_SIZE) {
                                        pipeline_push(
                                            &tasks,
                                            PipelineTask::Decompile(
                                                ClassJob {
                                                    dex: Arc::clone(&dex),
                                                    indices: chunk.to_vec(),
                                                    start_class: chunk[0],
                                                },
                                                false,
                                            ),
                                        );
                                    }
                                    Ok(true)
                                });
                            workers.inflating.fetch_sub(1, Ordering::Relaxed);
                            pipeline_finish_inflate(&tasks);
                            match result {
                                Ok(prepared) => {
                                    report_dex_inflated(
                                        progress_callback,
                                        &workers,
                                        total_dex_files,
                                        entry.number,
                                    );
                                    if prepared {
                                        continue;
                                    }
                                    if complete_apk_dex(
                                        &completion,
                                        progress_callback,
                                        total_dex_files,
                                        entry.number,
                                        started.elapsed(),
                                        Some(&workers),
                                    ) == total_dex_files
                                    {
                                        pipeline_close(&tasks);
                                    }
                                }
                                Err(error) => {
                                    pipeline.record(error);
                                    if complete_apk_dex(
                                        &completion,
                                        progress_callback,
                                        total_dex_files,
                                        entry.number,
                                        started.elapsed(),
                                        Some(&workers),
                                    ) == total_dex_files
                                    {
                                        pipeline_close(&tasks);
                                    }
                                }
                            }
                        }
                        PipelineTask::Decompile(job, first_for_dex) => {
                            if first_for_dex {
                                workers.decompiling_dexes.fetch_add(1, Ordering::Relaxed);
                            }
                            workers.decompiling.fetch_add(1, Ordering::Relaxed);
                            if !job.dex.working_reported.swap(true, Ordering::Relaxed) {
                                report_dex_working(
                                    progress_callback,
                                    &workers,
                                    total_dex_files,
                                    job.dex.number,
                                );
                            }
                            report_pipeline_activity(progress_callback, &workers, total_dex_files);
                            let dex = job.dex;
                            let indices = job.indices;
                            let start_class = job.start_class;
                            let mut records = Vec::with_capacity(indices.len());
                            let result = dex.decompiler.decompile_class_indices_for_export(
                                &indices,
                                |descriptor, output| {
                                    records.push((descriptor.to_owned(), output.source));
                                    Ok(())
                                },
                            );
                            match result {
                                Ok(count) => {
                                    class_count.fetch_add(count, Ordering::Relaxed);
                                }
                                Err(error) => pipeline.record(error),
                            }
                            if let Err(error) = emit_chunk(dex.number, start_class, records) {
                                pipeline.record(error);
                            }
                            let completed_dex = {
                                let mut output =
                                    dex.output.lock().expect("DEX output mutex poisoned");
                                output.pending_chunks -= 1;
                                if output.pending_chunks == 0 && !output.reported_complete {
                                    output.reported_complete = true;
                                    true
                                } else {
                                    false
                                }
                            };
                            workers.decompiling.fetch_sub(1, Ordering::Relaxed);
                            if pipeline_finish_decompile(&tasks, &dex) {
                                workers.decompiling_dexes.fetch_sub(1, Ordering::Relaxed);
                            }
                            if completed_dex {
                                workers.ready_dex.fetch_sub(1, Ordering::Relaxed);
                                if complete_apk_dex(
                                    &completion,
                                    progress_callback,
                                    total_dex_files,
                                    dex.number,
                                    dex.started.elapsed(),
                                    Some(&workers),
                                ) == total_dex_files
                                {
                                    pipeline_close(&tasks);
                                }
                            }
                        }
                    }
                    report_pipeline_activity(progress_callback, &workers, total_dex_files);
                }
                Ok::<_, crate::Error>(())
            }));
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| std::io::Error::other("APK pipeline worker panicked"))??;
        }
        Ok::<_, crate::Error>(())
    })?;

    let classes = class_count.load(Ordering::Relaxed);
    Arc::try_unwrap(pipeline)
        .expect("pipeline state still referenced")
        .finish()?;
    Ok((total_dex_files, classes))
}

fn export_apk_parallel_to_directory(
    input: &Path,
    output_directory: &Path,
    thread_count: usize,
    include_metadata: bool,
    debug: bool,
    progress_callback: Option<&(dyn Fn(ExportProgress) + Send + Sync)>,
    class_filter: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
) -> crate::Result<(usize, usize)> {
    process_apk_parallel(
        input,
        thread_count,
        include_metadata,
        debug,
        progress_callback,
        class_filter,
        |_, _, records| {
            for (descriptor, source) in records {
                write_java_class(output_directory, &descriptor, &source)?;
            }
            Ok(())
        },
    )
}

fn write_source_outputs<W: Write>(
    output: &mut W,
    classes: Vec<(String, SourceOutput)>,
) -> crate::Result<()> {
    for (descriptor, source) in classes {
        write_class_record(output, &descriptor, &source.source)?;
    }
    Ok(())
}

fn write_class_record<W: Write>(
    output: &mut W,
    descriptor: &str,
    source: &str,
) -> crate::Result<()> {
    output.write_all(&(descriptor.len() as u32).to_le_bytes())?;
    output.write_all(&(source.len() as u64).to_le_bytes())?;
    output.write_all(descriptor.as_bytes())?;
    output.write_all(source.as_bytes())?;
    Ok(())
}

struct BinaryDexOutput {
    class_count: u32,
    payload: Vec<u8>,
}

fn decompile_dex_binary(
    dex_bytes: &[u8],
    thread_count: usize,
    include_metadata: bool,
    debug: bool,
    class_filter: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
) -> crate::Result<BinaryDexOutput> {
    let decompiler = Decompiler::parse(dex_bytes)?.with_debug(debug);
    let decompiler = if include_metadata {
        decompiler
    } else {
        decompiler.without_metadata()
    };
    let mut payload = Vec::new();
    let classes = if let Some(filter) = class_filter {
        decompiler.decompile_selected_parallel(Some(thread_count), filter)?
    } else if thread_count > 1 {
        decompiler.decompile_all_parallel(thread_count)?
    } else {
        decompiler.decompile_all()?
    };
    let class_count = classes.len() as u32;
    write_source_outputs(&mut payload, classes)?;
    Ok(BinaryDexOutput {
        class_count,
        payload,
    })
}

fn cli_progress(progress: ExportProgress) {
    match progress {
        ExportProgress::Activity { .. } => {}
        ExportProgress::DexInflated { .. } => {}
        ExportProgress::Started {
            total_dex_files,
            worker_count,
        } => eprintln!("APK export: {total_dex_files} DEX files, {worker_count} worker(s)"),
        ExportProgress::DexWorking {
            started_dex_files,
            total_dex_files,
            dex_number,
            busy_workers,
            idle_workers,
            decompile_workers,
            decompile_dex_files,
            inflate_workers,
        } => eprintln!(
            "[apk {started_dex_files}/{total_dex_files} {:5.1}% working] {} | busy={busy_workers} idle={idle_workers} decompile_total={decompile_workers} decompile_dex={decompile_dex_files} per_dex_cap={APK_MAX_DECOMPILE_WORKERS} inflate={inflate_workers}",
            progress.percentage(),
            dex_entry_name(dex_number),
        ),
        ExportProgress::DexCompleted { .. } => {}
    }
}

fn export_binary(input: &Path, options: ExportOptions) -> crate::Result<()> {
    let stdout = std::io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    export_to_writer(input, &mut output, options)?;
    output.flush()?;
    Ok(())
}

/// Exports every class from a DEX or APK into package directories as one `.java` file per class.
///
/// Annotations are included by default through [`ExportOptions::default`]. For
/// a parallel APK export, the file is loaded once and the requested global
/// worker budget shares DEX preparation and class decompile/write tasks.
pub fn export_to_directory(
    input: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
    options: ExportOptions,
) -> crate::Result<ExportSummary> {
    let input = input.as_ref();
    let output_directory = output_directory.as_ref();
    let thread_count = options.validated_threads()?;
    std::fs::create_dir_all(output_directory)?;
    let started = std::time::Instant::now();
    let (dex_files, classes) = if input
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(OsStr::new("apk")))
    {
        export_apk_parallel_to_directory(
            input,
            output_directory,
            thread_count.unwrap_or(1),
            options.include_metadata,
            options.debug,
            options.progress_callback.as_deref(),
            options.class_filter.as_deref(),
        )?
    } else {
        let bytes = std::fs::read(input)?;
        (
            1,
            export_dex_to_directory(
                &bytes,
                output_directory,
                thread_count,
                options.include_metadata,
                options.debug,
                options.class_filter.as_deref(),
            )?,
        )
    };
    Ok(ExportSummary {
        dex_files,
        classes,
        duration: started.elapsed(),
    })
}

/// Writes all decompiled classes to an `RGOUT001` stream.
///
/// This is primarily useful for bulk integrations that want to avoid creating
/// hundreds of thousands of files. APK input uses the same inflate, parse, and
/// class-task pipeline as [`export_to_directory`]; indexed class chunks are
/// buffered only at the final sink so records can be emitted in deterministic
/// DEX/class order. Annotations are included by default.
pub fn export_to_writer(
    input: impl AsRef<Path>,
    output: &mut impl Write,
    options: ExportOptions,
) -> crate::Result<ExportSummary> {
    let input = input.as_ref();
    let thread_count = options.validated_threads()?;
    let started = std::time::Instant::now();
    let apk = input
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(OsStr::new("apk")));
    let (dex_files, classes, records, direct_payload) = if apk {
        let records = Mutex::new(BTreeMap::<
            (u32, usize),
            Vec<(String, String)>,
        >::new());
        let (dex_files, classes) = process_apk_parallel(
            input,
            thread_count.unwrap_or(1),
            options.include_metadata,
            options.debug,
            options.progress_callback.as_deref(),
            options.class_filter.as_deref(),
            |dex_number, start_class, chunk| {
                records
                    .lock()
                    .expect("RGOUT record mutex poisoned")
                    .insert((dex_number, start_class), chunk);
                Ok(())
            },
        )?;
        let ordered = records
            .into_inner()
            .expect("RGOUT record mutex poisoned");
        (dex_files, classes, Some(ordered), None)
    } else {
        let bytes = std::fs::read(input)?;
        let dex = decompile_dex_binary(
            &bytes,
            thread_count.unwrap_or(1),
            options.include_metadata,
            options.debug,
            options.class_filter.as_deref(),
        )?;
        (1, dex.class_count as usize, None, Some(dex.payload))
    };
    let duration = started.elapsed();
    output.write_all(b"RGOUT001")?;
    output.write_all(&duration.as_secs_f64().to_le_bytes())?;
    output.write_all(&(classes as u32).to_le_bytes())?;
    if let Some(records) = records {
        for (_, chunk) in records {
            for (descriptor, source) in chunk {
                write_class_record(output, &descriptor, &source)?;
            }
        }
    } else if let Some(payload) = direct_payload {
        output.write_all(&payload)?;
    }
    Ok(ExportSummary {
        dex_files,
        classes,
        duration,
    })
}

pub fn run() -> crate::Result<()> {
    let mut args = std::env::args_os();
    let program = args
        .next()
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "radguard".to_owned());
    let command = args.next();
    let mut command_args: Vec<_> = args.collect();
    let supports_metadata_option = command.as_deref().is_some_and(|command| {
        matches!(
            command.to_str(),
            Some("decompile" | "export-all" | "export-all-parallel")
        )
    });
    let include_metadata = if supports_metadata_option {
        take_metadata_option(&mut command_args).unwrap_or_else(|| {
            usage(&program);
            std::process::exit(2);
        })
    } else {
        true
    };
    let debug = if supports_metadata_option {
        take_debug_option(&mut command_args).unwrap_or_else(|| {
            usage(&program);
            std::process::exit(2);
        })
    } else {
        false
    };
    let mut args = command_args.into_iter();
    let path = args.next();
    match command.as_deref() {
        Some(command) if command == std::ffi::OsStr::new("inspect") => {
            let Some(path) = path else {
                usage(&program);
                std::process::exit(2);
            };
            if args.next().is_some() {
                usage(&program);
                std::process::exit(2);
            }
            print_inspect(Path::new(&path))
        }
        Some(command) if command == std::ffi::OsStr::new("decompile") => {
            let Some(path) = path else {
                usage(&program);
                std::process::exit(2);
            };
            let Some(class) = args.next().and_then(|value| value.into_string().ok()) else {
                usage(&program);
                std::process::exit(2);
            };
            if args.next().is_some() {
                usage(&program);
                std::process::exit(2);
            }
            let source =
                decompile_class_file_configured(
                    Path::new(&path),
                    &class,
                    include_metadata,
                    debug,
                )?
                .map(|output| output.source)
                .unwrap_or_else(|| {
                    format!("Error: Class {class} not found in the reconstructed DEX.")
                });
            print!("{source}");
            Ok(())
        }
        Some(command) if command == std::ffi::OsStr::new("export-all") => {
            let Some(path) = path else {
                usage(&program);
                std::process::exit(2);
            };
            let output_directory = args.next();
            if args.next().is_some() {
                usage(&program);
                std::process::exit(2);
            }
            let options = if include_metadata {
                ExportOptions::default()
            } else {
                ExportOptions::default().without_metadata()
            }
            .with_debug(debug)
            .with_progress(cli_progress);
            if let Some(output_directory) = output_directory {
                let summary =
                    export_to_directory(Path::new(&path), Path::new(&output_directory), options)?;
                eprintln!(
                    "exported {} classes from {} DEX file(s) to {} in {:.6}s",
                    summary.classes,
                    summary.dex_files,
                    Path::new(&output_directory).display(),
                    summary.duration.as_secs_f64(),
                );
                return Ok(());
            }
            export_binary(Path::new(&path), options)
        }
        Some(command) if command == std::ffi::OsStr::new("export-all-parallel") => {
            let Some(path) = path else {
                usage(&program);
                std::process::exit(2);
            };
            let first_optional = args.next();
            let second_optional = args.next();
            if args.next().is_some() {
                usage(&program);
                std::process::exit(2);
            }
            let (thread_count, output_directory) = match (first_optional, second_optional) {
                (None, None) => (default_thread_count(), None),
                (Some(value), None) => match parse_thread_count(&value) {
                    Some(thread_count) => (thread_count, None),
                    None => (default_thread_count(), Some(value)),
                },
                (Some(value), Some(output_directory)) => {
                    let Some(thread_count) = parse_thread_count(&value) else {
                        usage(&program);
                        std::process::exit(2);
                    };
                    (thread_count, Some(output_directory))
                }
                (None, Some(_)) => unreachable!("second argument requires the first argument"),
            };
            let options = if include_metadata {
                ExportOptions::default()
            } else {
                ExportOptions::default().without_metadata()
            }
            .with_threads(thread_count)
            .with_debug(debug)
            .with_progress(cli_progress);
            if let Some(output_directory) = output_directory {
                let summary =
                    export_to_directory(Path::new(&path), Path::new(&output_directory), options)?;
                eprintln!(
                    "exported {} classes from {} DEX file(s) to {} in {:.6}s",
                    summary.classes,
                    summary.dex_files,
                    Path::new(&output_directory).display(),
                    summary.duration.as_secs_f64(),
                );
                return Ok(());
            }
            export_binary(Path::new(&path), options)
        }
        _ => {
            usage(&program);
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        default_thread_count_for, dex_entry_number, java_relative_path, parse_thread_count,
        take_metadata_option,
    };
    use std::ffi::{OsStr, OsString};
    use std::path::Path;

    #[test]
    fn java_export_path_follows_package_structure() {
        assert_eq!(
            java_relative_path("Lcom/example/Outer$Inner;"),
            Some(Path::new("com/example/Outer$Inner.java").to_owned())
        );
        assert_eq!(java_relative_path("L../Escape;"), None);
        assert_eq!(java_relative_path("L..\\Escape;"), None);
        assert_eq!(java_relative_path("LC:/Escape;"), None);
        assert_eq!(java_relative_path("not-a-descriptor"), None);
    }

    #[test]
    fn apk_dex_entries_use_multidex_order() {
        assert_eq!(dex_entry_number("classes.dex"), Some(1));
        assert_eq!(dex_entry_number("classes2.dex"), Some(2));
        assert_eq!(dex_entry_number("classes10.dex"), Some(10));
        assert_eq!(dex_entry_number("classes0.dex"), None);
        assert_eq!(dex_entry_number("assets/classes2.dex"), None);
        assert_eq!(dex_entry_number("classes.txt"), None);
    }

    #[test]
    fn optional_parallel_argument_distinguishes_threads_from_directory() {
        assert_eq!(parse_thread_count(OsStr::new("8")), Some(8));
        assert_eq!(parse_thread_count(OsStr::new("0")), None);
        assert_eq!(parse_thread_count(OsStr::new("output")), None);
    }

    #[test]
    fn metadata_option_is_position_independent_and_rejects_duplicates() {
        let mut default_args = vec![OsString::from("classes.dex")];
        assert_eq!(take_metadata_option(&mut default_args), Some(true));
        assert_eq!(default_args, [OsString::from("classes.dex")]);

        let mut flagged_args = vec![
            OsString::from("classes.dex"),
            OsString::from("--no-metadata"),
            OsString::from("output"),
        ];
        assert_eq!(take_metadata_option(&mut flagged_args), Some(false));
        assert_eq!(
            flagged_args,
            [OsString::from("classes.dex"), OsString::from("output")]
        );

        let mut duplicate_args = vec![
            OsString::from("--no-metadata"),
            OsString::from("classes.dex"),
            OsString::from("--no-metadata"),
        ];
        assert_eq!(take_metadata_option(&mut duplicate_args), None);
    }

    #[test]
    fn default_parallelism_uses_half_the_available_logical_cpus() {
        assert_eq!(default_thread_count_for(1), 1);
        assert_eq!(default_thread_count_for(2), 1);
        assert_eq!(default_thread_count_for(3), 1);
        assert_eq!(default_thread_count_for(4), 2);
        assert_eq!(default_thread_count_for(20), 10);
    }
}
