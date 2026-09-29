//! Project-scoped, conservative DEX candidate index for Find usages.
use crate::{
    native_dex::{DexAnnotation, DexClass, DexValue},
    native_engine::disassembly::{type_name, visit_instruction_symbols},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
const MAX_POSTING_BUFFER_BYTES: usize = 32 * 1024 * 1024;
const POSTING_DISK_BYTES: u64 = 12;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Posting {
    fingerprint: u64,
    owner: u32,
}

fn fingerprint(symbol: &str) -> u64 {
    let digest = Sha256::digest(symbol.as_bytes());
    u64::from_le_bytes(digest[..8].try_into().unwrap())
}

struct SpillDir(PathBuf);

impl SpillDir {
    fn new() -> io::Result<Self> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|error| io::Error::other(error.to_string()))?;
        let path = std::env::temp_dir().join(format!(
            "rdx-usage-{}-{:032x}",
            std::process::id(),
            u128::from_le_bytes(nonce)
        ));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for SpillDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct SpillRun {
    path: PathBuf,
    count: usize,
    modified: Option<std::time::SystemTime>,
}

impl SpillRun {
    fn matching(
        &self,
        fingerprint: u64,
        ids: &mut Vec<u32>,
        cancel: &AtomicBool,
        owner_count: usize,
    ) -> io::Result<bool> {
        let mut file = File::open(&self.path)?;
        let metadata = file.metadata()?;
        if metadata.len() != self.count as u64 * POSTING_DISK_BYTES
            || metadata.modified().ok() != self.modified
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "spill run changed",
            ));
        }
        let mut low = 0usize;
        let mut high = self.count;
        let mut record = [0u8; POSTING_DISK_BYTES as usize];
        while low < high {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let mid = low + (high - low) / 2;
            file.seek(SeekFrom::Start(mid as u64 * POSTING_DISK_BYTES))?;
            file.read_exact(&mut record)?;
            if u64::from_le_bytes(record[..8].try_into().unwrap()) < fingerprint {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        file.seek(SeekFrom::Start(low as u64 * POSTING_DISK_BYTES))?;
        while low < self.count {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            file.read_exact(&mut record)?;
            if u64::from_le_bytes(record[..8].try_into().unwrap()) != fingerprint {
                break;
            }
            let owner = u32::from_le_bytes(record[8..12].try_into().unwrap());
            if owner as usize >= owner_count {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid spill owner",
                ));
            }
            ids.push(owner);
            low += 1;
        }
        Ok(true)
    }
}

fn spill_postings(
    postings: &mut Vec<Posting>,
    directory: &SpillDir,
    runs: &mut Vec<SpillRun>,
) -> io::Result<()> {
    postings.sort_unstable();
    postings.dedup();
    let path = directory.0.join(format!("run-{:06}.bin", runs.len()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut writer = BufWriter::new(options.open(&path)?);
    for posting in postings.iter() {
        let fingerprint = posting.fingerprint.to_le_bytes();
        let owner = posting.owner.to_le_bytes();
        writer.write_all(&fingerprint)?;
        writer.write_all(&owner)?;
    }
    writer.flush()?;
    drop(writer);
    runs.push(SpillRun {
        modified: fs::metadata(&path)?.modified().ok(),
        path,
        count: postings.len(),
    });
    postings.clear();
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct UsageIndexStats {
    pub owners: usize,
    pub indexed_owners: usize,
    /// Class-symbol associations; repeated symbols in different classes count separately.
    pub symbols: usize,
    pub postings: usize,
    pub uncertain_owners: usize,
    pub estimated_bytes: usize,
    pub fallback: bool,
    pub partial: bool,
    pub spill_runs: usize,
    pub spill_bytes: u64,
}

struct Index {
    names: Vec<String>,
    postings: Vec<Posting>,
    runs: Vec<SpillRun>,
    _spill_dir: Option<SpillDir>,
    uncertain: Vec<u32>,
    stats: UsageIndexStats,
}

#[derive(Default)]
struct State {
    scanned: usize,
    total: usize,
    ready: Option<Arc<Index>>,
}

struct ProjectLease {
    state: Arc<(Mutex<State>, Condvar)>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct CompletedCache {
    entries: HashMap<String, Vec<String>>,
    bytes: usize,
}

impl Drop for ProjectLease {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.state.1.notify_all();
    }
}

#[derive(Clone)]
pub struct UsageIndexHandle {
    state: Arc<(Mutex<State>, Condvar)>,
    cancel: Arc<AtomicBool>,
    _owner: Option<Arc<ProjectLease>>,
    completed: Arc<Mutex<CompletedCache>>,
}

impl UsageIndexHandle {
    pub(crate) fn start(
        classes: Arc<BTreeMap<String, DexClass>>,
        nested: Arc<BTreeMap<String, Vec<String>>>,
    ) -> Self {
        let state = Arc::new((
            Mutex::new(State {
                total: classes.len(),
                ..State::default()
            }),
            Condvar::new(),
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = Self {
            state: Arc::clone(&state),
            cancel: Arc::clone(&cancel),
            _owner: Some(Arc::new(ProjectLease { state, cancel })),
            completed: Arc::new(Mutex::new(CompletedCache::default())),
        };
        let worker = Self {
            state: Arc::clone(&handle.state),
            cancel: Arc::clone(&handle.cancel),
            _owner: None,
            completed: Arc::clone(&handle.completed),
        };
        thread::spawn(move || {
            let built = std::panic::catch_unwind(|| worker.build(&classes, &nested))
                .ok()
                .flatten();
            let (lock, wake) = &*worker.state;
            let mut state = lock.lock().unwrap_or_else(|error| error.into_inner());
            state.ready =
                Some(Arc::new(built.unwrap_or_else(|| {
                    Index::fallback(classes.keys().cloned().collect())
                })));
            wake.notify_all();
        });
        handle
    }

    pub fn cancel_project(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.state.1.notify_all();
    }

    pub fn stats(&self) -> Option<UsageIndexStats> {
        self.state
            .0
            .lock()
            .ok()?
            .ready
            .as_ref()
            .map(|index| index.stats.clone())
    }

    pub fn record_completed(&self, target: &str, owners: Vec<String>) {
        let cost = target.len() + 80 + owners.iter().map(|owner| owner.len() + 24).sum::<usize>();
        if cost > 1024 * 1024 {
            return;
        }
        if let Ok(mut completed) = self.completed.lock() {
            if let Some(previous) = completed.entries.remove(target) {
                completed.bytes = completed.bytes.saturating_sub(
                    target.len()
                        + 80
                        + previous.iter().map(|owner| owner.len() + 24).sum::<usize>(),
                );
            }
            if completed.entries.len() >= 64 || completed.bytes + cost > 1024 * 1024 {
                completed.entries.clear();
                completed.bytes = 0;
            }
            completed.entries.insert(target.to_owned(), owners);
            completed.bytes += cost;
        }
    }

    /// None means only this query was cancelled. The shared build continues.
    pub fn candidates(
        &self,
        target: &str,
        query_cancel: &AtomicBool,
        mut progress: impl FnMut(usize, usize),
    ) -> Option<Vec<String>> {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().ok()?;
        while state.ready.is_none() {
            if query_cancel.load(Ordering::Relaxed) {
                return None;
            }
            let (scanned, total) = (state.scanned, state.total);
            drop(state);
            progress(scanned, total);
            state = lock.lock().ok()?;
            if state.ready.is_some() {
                break;
            }
            state = wake.wait_timeout(state, Duration::from_millis(100)).ok()?.0;
        }
        let index = Arc::clone(state.ready.as_ref()?);
        drop(state);
        if query_cancel.load(Ordering::Relaxed) {
            return None;
        }
        if let Some(owners) = self.completed.lock().ok()?.entries.get(target).cloned() {
            return Some(owners);
        }
        if target.starts_with(crate::resource_table::PREFIX) {
            return Some(index.names.clone());
        }
        let mut ids = match index.matches(target, query_cancel) {
            Ok(Some(ids)) => ids,
            Ok(None) => return None,
            Err(_) => return Some(index.names.clone()),
        };
        ids.extend_from_slice(&index.uncertain);
        ids.sort_unstable();
        ids.dedup();
        Some(
            ids.into_iter()
                .filter_map(|id| index.names.get(id as usize).cloned())
                .collect(),
        )
    }

    fn build(
        &self,
        classes: &BTreeMap<String, DexClass>,
        nested: &BTreeMap<String, Vec<String>>,
    ) -> Option<Index> {
        self.build_with_limit(classes, nested, MAX_INDEX_BYTES)
    }

    fn build_with_limit(
        &self,
        classes: &BTreeMap<String, DexClass>,
        nested: &BTreeMap<String, Vec<String>>,
        max_bytes: usize,
    ) -> Option<Index> {
        let mut names: Vec<String> = classes.keys().cloned().collect();
        let ids: HashMap<&str, u32> = names
            .iter()
            .enumerate()
            .map(|(i, name)| (name.as_str(), i as u32))
            .collect();
        let mut parents: HashMap<&str, Vec<u32>> = HashMap::new();
        for (parent, children) in nested {
            if let Some(&id) = ids.get(parent.as_str()) {
                for child in children {
                    parents.entry(child).or_default().push(id);
                }
            }
        }
        let mut postings: Vec<Posting> = Vec::new();
        let mut runs = Vec::new();
        let mut spill_dir: Option<SpillDir> = None;
        let mut uncertain = Vec::new();
        let names_bytes = names.iter().map(String::capacity).sum::<usize>()
            + names.capacity() * std::mem::size_of::<String>();
        let max_records = max_bytes
            .saturating_sub(names_bytes + names.len() * 4)
            .min(MAX_POSTING_BUFFER_BYTES)
            .checked_div(std::mem::size_of::<Posting>())
            .unwrap_or(0)
            .max(1);
        let mut posting_count = 0usize;
        let mut symbol_occurrences = 0usize;
        let mut indexed_owners = 0usize;
        let mut last_progress = std::time::Instant::now();
        for (id, (name, class)) in classes.iter().enumerate() {
            if self.cancel.load(Ordering::Relaxed) {
                return None;
            }
            let owner_ids = std::iter::once(id as u32)
                .chain(parents.get(name.as_str()).into_iter().flatten().copied())
                .collect::<Vec<_>>();
            let mut symbols = HashSet::new();
            let complete = collect_class(class, &mut symbols, &self.cancel)
                && symbols.len() <= 16_384
                && symbols
                    .iter()
                    .map(|symbol| symbol.len() + 48)
                    .sum::<usize>()
                    <= 8 * 1024 * 1024;
            if !complete {
                symbols.clear();
                uncertain.extend_from_slice(&owner_ids);
            }
            symbol_occurrences += symbols.len();
            for symbol in symbols {
                let fingerprint = fingerprint(&symbol);
                for &owner in &owner_ids {
                    if postings.len() >= max_records {
                        if spill_dir.is_none() {
                            spill_dir = Some(SpillDir::new().ok()?);
                        }
                        spill_postings(&mut postings, spill_dir.as_ref()?, &mut runs).ok()?;
                    }
                    if postings.len() == postings.capacity() {
                        postings.reserve_exact(
                            postings.capacity().max(1).min(max_records - postings.len()),
                        );
                    }
                    postings.push(Posting { fingerprint, owner });
                    posting_count += 1;
                }
            }
            indexed_owners = id + 1;
            if id + 1 == classes.len()
                || (id + 1).is_multiple_of(256)
                    && last_progress.elapsed() >= Duration::from_millis(100)
            {
                let (lock, wake) = &*self.state;
                if let Ok(mut state) = lock.lock() {
                    state.scanned = id + 1;
                    wake.notify_all();
                }
                last_progress = std::time::Instant::now();
            }
        }
        postings.sort_unstable();
        postings.dedup();
        postings.shrink_to_fit();
        uncertain.sort_unstable();
        uncertain.dedup();
        uncertain.shrink_to_fit();
        names.shrink_to_fit();
        let bytes = names.iter().map(String::capacity).sum::<usize>()
            + names.capacity() * std::mem::size_of::<String>()
            + postings.capacity() * std::mem::size_of::<Posting>()
            + uncertain.capacity() * 4
            + runs.capacity() * std::mem::size_of::<SpillRun>()
            + runs.iter().map(|run| run.path.capacity()).sum::<usize>()
            + spill_dir.as_ref().map_or(0, |dir| dir.0.capacity());
        let spill_bytes = runs
            .iter()
            .map(|run: &SpillRun| run.count as u64 * POSTING_DISK_BYTES)
            .sum();
        let stats = UsageIndexStats {
            owners: names.len(),
            indexed_owners,
            symbols: symbol_occurrences,
            postings: posting_count,
            uncertain_owners: uncertain.len(),
            estimated_bytes: bytes,
            fallback: false,
            partial: false,
            spill_runs: runs.len(),
            spill_bytes,
        };
        Some(Index {
            names,
            postings,
            runs,
            _spill_dir: spill_dir,
            uncertain,
            stats,
        })
    }
}

impl Index {
    fn matches(&self, target: &str, cancel: &AtomicBool) -> io::Result<Option<Vec<u32>>> {
        let fingerprint = fingerprint(target);
        let start = self
            .postings
            .partition_point(|posting| posting.fingerprint < fingerprint);
        let mut ids = Vec::new();
        for posting in self.postings[start..]
            .iter()
            .take_while(|posting| posting.fingerprint == fingerprint)
        {
            ids.push(posting.owner);
        }
        for run in &self.runs {
            if !run.matching(fingerprint, &mut ids, cancel, self.names.len())? {
                return Ok(None);
            }
        }
        Ok(Some(ids))
    }

    fn fallback(names: Vec<String>) -> Self {
        let owners = names.len();
        let estimated_bytes = names.iter().map(String::capacity).sum::<usize>()
            + names.capacity() * std::mem::size_of::<String>()
            + owners * std::mem::size_of::<u32>();
        Self {
            names,
            postings: Vec::new(),
            runs: Vec::new(),
            _spill_dir: None,
            uncertain: (0..owners as u32).collect(),
            stats: UsageIndexStats {
                owners,
                indexed_owners: 0,
                uncertain_owners: owners,
                estimated_bytes,
                fallback: true,
                partial: false,
                ..UsageIndexStats::default()
            },
        }
    }
}

fn add_type(value: &str, out: &mut HashSet<String>) {
    let element = value.trim_start_matches('[');
    if element.starts_with('L') && element.ends_with(';') {
        out.insert(type_name(element));
        if let Ok(display) = crate::native_java::java_type(element) {
            out.insert(display);
        }
    }
}

struct CollectionGuard<'a> {
    cancel: &'a AtomicBool,
    steps: usize,
}

impl CollectionGuard<'_> {
    fn tick(&mut self, out: &HashSet<String>) -> bool {
        self.steps += 1;
        if self.steps > 1_000_000 {
            return false;
        }
        !self.steps.is_multiple_of(256)
            || (!self.cancel.load(Ordering::Relaxed)
                && out.len() <= 16_384
                && out.iter().map(|symbol| symbol.len() + 48).sum::<usize>() <= 8 * 1024 * 1024)
    }
}

fn add_value(
    value: &DexValue,
    class: &DexClass,
    out: &mut HashSet<String>,
    guard: &mut CollectionGuard<'_>,
) -> bool {
    if !guard.tick(out) {
        return false;
    }
    match value {
        DexValue::Type(index) => {
            let Some(ty) = class.symbols.types.get(*index as usize) else {
                return false;
            };
            add_type(ty, out);
            out.insert("java.lang.Class".to_owned());
        }
        DexValue::Field(index) | DexValue::Enum(index) => {
            let Some(symbol) =
                crate::native_engine::disassembly::symbol(&class.symbols, 'f', *index as usize)
            else {
                return false;
            };
            add_reference(&symbol, out);
        }
        DexValue::Method(index) => {
            let Some(symbol) =
                crate::native_engine::disassembly::symbol(&class.symbols, 'm', *index as usize)
            else {
                return false;
            };
            add_reference(&symbol, out);
        }
        DexValue::Array(values) => {
            for value in values {
                if !add_value(value, class, out, guard) {
                    return false;
                }
            }
        }
        DexValue::Annotation { type_idx, elements } => {
            let Some(ty) = class.symbols.types.get(*type_idx as usize) else {
                return false;
            };
            add_type(ty, out);
            for (_, value) in elements {
                if !add_value(value, class, out, guard) {
                    return false;
                }
            }
        }
        _ => {}
    }
    true
}

fn add_annotations(
    annotations: &[Arc<DexAnnotation>],
    class: &DexClass,
    out: &mut HashSet<String>,
    guard: &mut CollectionGuard<'_>,
) -> bool {
    for annotation in annotations {
        if !guard.tick(out) {
            return false;
        }
        let Some(ty) = class.symbols.types.get(annotation.type_idx as usize) else {
            return false;
        };
        add_type(ty, out);
        if ty.as_ref() == "Ldalvik/annotation/Signature;" {
            for (_, value) in &annotation.elements {
                if !add_signature(value, class, out, guard) {
                    return false;
                }
            }
        }
        for (_, value) in &annotation.elements {
            if !add_value(value, class, out, guard) {
                return false;
            }
        }
    }
    true
}

fn add_signature(
    value: &DexValue,
    class: &DexClass,
    out: &mut HashSet<String>,
    guard: &mut CollectionGuard<'_>,
) -> bool {
    fn append(
        value: &DexValue,
        class: &DexClass,
        text: &mut String,
        guard: &mut CollectionGuard<'_>,
        out: &HashSet<String>,
        depth: usize,
    ) -> bool {
        if depth > 64 || guard.cancel.load(Ordering::Relaxed) || !guard.tick(out) {
            return false;
        }
        match value {
            DexValue::String(index) => {
                let Some(part) = class.symbols.strings.get(*index as usize) else {
                    return false;
                };
                if text.len().saturating_add(part.len()) > 1024 * 1024 {
                    return false;
                }
                text.push_str(part);
            }
            DexValue::Array(parts) => {
                for part in parts {
                    if !append(part, class, text, guard, out, depth + 1) {
                        return false;
                    }
                }
            }
            _ => return false,
        }
        true
    }
    let mut text = String::new();
    if !append(value, class, &mut text, guard, out, 0) {
        return false;
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut next_check = 0;
    while i < bytes.len() {
        if i >= next_check {
            if guard.cancel.load(Ordering::Relaxed) || !guard.tick(out) {
                return false;
            }
            next_check = i.saturating_add(256);
        }
        if bytes[i] != b'L' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end] != b';' && bytes[end] != b'<' {
            if end >= next_check {
                if guard.cancel.load(Ordering::Relaxed) || !guard.tick(out) {
                    return false;
                }
                next_check = end.saturating_add(256);
            }
            end += 1;
        }
        if end == bytes.len() || end == start {
            return false;
        }
        let Some(name) = text.get(start..end) else {
            return false;
        };
        out.insert(name.replace('/', "."));
        i = end + 1;
    }
    true
}

fn collect_class(class: &DexClass, out: &mut HashSet<String>, cancel: &AtomicBool) -> bool {
    let mut guard = CollectionGuard { cancel, steps: 0 };
    if cancel.load(Ordering::Relaxed) {
        return false;
    }
    if let Some(parent) = &class.superclass {
        add_type(parent, out)
    }
    for interface in &class.interfaces {
        if !guard.tick(out) {
            return false;
        }
        add_type(interface, out)
    }
    for field in &class.fields {
        if !guard.tick(out) {
            return false;
        }
        add_type(&field.field_type, out);
        add_type(&field.declaring_type, out);
    }
    for method in &class.methods {
        if !guard.tick(out) {
            return false;
        }
        add_type(&method.declaring_type, out);
        add_type(&method.return_type, out);
        for ty in &method.parameters {
            if !guard.tick(out) {
                return false;
            }
            add_type(ty, out)
        }
        for ty in &method.thrown_types {
            if !guard.tick(out) {
                return false;
            }
            add_type(ty, out)
        }
        if let Some(ty) = crate::native_java::inherited_override_exception(class, method) {
            add_type(ty, out);
        }
        if let Some(code) = &method.code {
            let mut pc = 0;
            while pc < code.instructions.len() {
                if !guard.tick(out) {
                    return false;
                }
                if code.instructions[pc] as u8 == 0x1c {
                    out.insert("java.lang.Class".to_owned());
                }
                let Some(width) = crate::native_engine::disassembly::width(&code.instructions, pc)
                else {
                    return false;
                };
                pc += width;
            }
            for region in &code.try_regions {
                if !guard.tick(out) {
                    return false;
                }
                for (ty, _) in region.catches.iter() {
                    if !guard.tick(out) {
                        return false;
                    }
                    if let Some(ty) = ty {
                        add_type(ty, out)
                    } else {
                        out.insert("java.lang.Throwable".to_owned());
                    }
                }
            }
        }
    }
    if let Some(hierarchy) = class.symbols.hierarchy.get() {
        for constructor in hierarchy.recovered_constructors(&class.descriptor) {
            let parent = type_name(&constructor.parent);
            let signature = format!("{parent}.<init>({})V", constructor.parameters.join(""));
            add_reference(&signature, out);
            for ty in &constructor.thrown_types {
                add_type(ty, out);
            }
        }
    }
    for value in &class.static_values {
        if !add_value(value, class, out, &mut guard) {
            return false;
        }
    }
    if class.annotations_offset != 0 {
        let Some(directory) = class.symbols.annotations.get(&class.annotations_offset) else {
            return false;
        };
        for set in directory
            .class
            .iter()
            .chain(directory.fields.iter().flatten())
            .chain(directory.methods.iter().flatten())
            .chain(directory.parameters.iter().flatten().flatten())
        {
            if !guard.tick(out) || !add_annotations(set, class, out, &mut guard) {
                return false;
            }
        }
    }
    let mut added_bytes = 0usize;
    visit_instruction_symbols(class, &|| cancel.load(Ordering::Relaxed), |reference| {
        added_bytes += reference.len() + 80;
        if added_bytes > 8 * 1024 * 1024 || out.len() > 16_384 {
            return false;
        }
        add_reference(&reference, out);
        true
    })
}

fn add_reference(reference: &str, out: &mut HashSet<String>) {
    out.insert(reference.to_owned());
    add_type(reference, out);
    if reference.starts_with("[L") && reference.contains(";.") {
        out.insert(reference.replacen(";.", ".", 1));
    }
    let signature = reference.find('(').or_else(|| reference.find(':'));
    if let Some(boundary) = signature {
        if let Some(dot) = reference[..boundary].rfind('.') {
            out.insert(reference[..dot].to_owned());
            let descriptor = format!("L{};", reference[..dot].replace('.', "/"));
            add_type(&descriptor, out);
        }
        let mut rest = &reference[boundary..];
        while let Some(start) = rest.find('L') {
            rest = &rest[start..];
            let Some(end) = rest.find(';') else { break };
            add_type(&rest[..=end], out);
            rest = &rest[end + 1..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{DecompilerEngine, NativeEngine};

    #[test]
    fn array_and_member_references_retain_source_aliases_and_component_types() {
        let mut symbols = HashSet::new();
        add_reference("[Lsample/Component;", &mut symbols);
        add_reference(
            "[Lsample.Component;.clone()Ljava/lang/Object;",
            &mut symbols,
        );
        assert!(symbols.contains("sample.Component"));
        assert!(symbols.contains("[Lsample.Component.clone()Ljava/lang/Object;"));
        assert!(symbols.contains("java.lang.Object"));
    }

    #[test]
    fn invalid_java_class_name_indexes_rendered_alias() {
        let mut symbols = HashSet::new();
        add_type("Lj$/util/Collection$-CC;", &mut symbols);
        assert!(symbols.contains("j$.util.Collection$-CC"));
        assert!(symbols.contains("j$.util._rdx_436f6c6c656374696f6e242d4343"));
    }

    #[test]
    fn const_class_instruction_indexes_implicit_class_type() {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let code = class
            .methods
            .iter_mut()
            .find_map(|method| method.code.as_mut())
            .unwrap();
        code.instructions = vec![0x001c, 0, 0x000e];
        let mut symbols = HashSet::new();
        assert!(collect_class(&class, &mut symbols, &AtomicBool::new(false)));
        assert!(symbols.contains("java.lang.Class"));
    }

    #[test]
    fn catchall_handler_indexes_implicit_throwable_link() {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let code = class
            .methods
            .iter_mut()
            .find_map(|method| method.code.as_mut())
            .unwrap();
        code.try_regions.push(crate::native_dex::DexTryRegion {
            start: 0,
            end: 1,
            catches: Arc::from([(None, 0)]),
        });
        let mut symbols = HashSet::new();
        assert!(collect_class(&class, &mut symbols, &AtomicBool::new(false)));
        assert!(symbols.contains("java.lang.Throwable"));
    }

    #[test]
    fn generic_signature_fragments_index_nested_type_descriptors() {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let symbols = Arc::get_mut(&mut class.symbols).unwrap();
        let start = symbols.strings.len() as u32;
        symbols.strings.extend([
            "Ljava/util/List<".into(),
            "Lsample/Target;".into(),
            ">;".into(),
        ]);
        let value = DexValue::Array(vec![
            DexValue::String(start),
            DexValue::String(start + 1),
            DexValue::String(start + 2),
        ]);
        let mut found = HashSet::new();
        let cancel = AtomicBool::new(false);
        let mut guard = CollectionGuard {
            cancel: &cancel,
            steps: 0,
        };
        assert!(add_signature(&value, &class, &mut found, &mut guard));
        assert!(found.contains("java.util.List"));
        assert!(found.contains("sample.Target"));
    }

    #[test]
    fn completed_owner_cache_narrows_repeat_query_without_stopping_project_build() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.dex");
        let mut engine = NativeEngine::start().unwrap();
        engine.open(&path).unwrap();
        let handle = engine.usage_index_handle().unwrap();
        let cancel = AtomicBool::new(false);
        let original = handle
            .candidates("sample.Target", &cancel, |_, _| {})
            .unwrap();
        assert!(!original.is_empty());
        let narrowed = vec![original[0].clone()];
        handle.record_completed("sample.Target", narrowed.clone());
        assert_eq!(
            handle.candidates("sample.Target", &cancel, |_, _| {}),
            Some(narrowed)
        );
        cancel.store(true, Ordering::Relaxed);
        assert!(
            handle
                .candidates("sample.Target", &cancel, |_, _| {})
                .is_none()
        );
        assert!(!handle.cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn malformed_instruction_retains_owner_for_unknown_targets() {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let code = class
            .methods
            .iter_mut()
            .find_map(|method| method.code.as_mut())
            .unwrap();
        code.instructions = vec![0x001c, u16::MAX];
        let mut classes = BTreeMap::new();
        classes.insert("sample.Hello".into(), class);
        let handle = UsageIndexHandle::start(Arc::new(classes), Arc::new(BTreeMap::new()));
        let cancel = AtomicBool::new(false);
        assert_eq!(
            handle.candidates("unindexed.Symbol", &cancel, |_, _| {}),
            Some(vec!["sample.Hello".into()])
        );
        assert_eq!(handle.stats().unwrap().uncertain_owners, 1);
    }

    #[test]
    fn last_project_owner_drop_cancels_builder_lease() {
        let handle = UsageIndexHandle::start(Arc::new(BTreeMap::new()), Arc::new(BTreeMap::new()));
        let other_owner = handle.clone();
        let cancellation = Arc::clone(&handle.cancel);
        drop(handle);
        assert!(!cancellation.load(Ordering::Relaxed));
        drop(other_owner);
        assert!(cancellation.load(Ordering::Relaxed));
        let fallback = Index::fallback(vec!["first".into(), "second".into()]);
        assert!(fallback.stats.fallback);
        assert_eq!(fallback.uncertain, vec![0, 1]);
    }

    #[test]
    fn forced_spill_keeps_all_owners_and_nested_projection() {
        let handle = UsageIndexHandle::start(Arc::new(BTreeMap::new()), Arc::new(BTreeMap::new()));
        let classes = handle_fixture_classes();
        let nested = BTreeMap::from([("b.Root".into(), vec!["c.Child".into()])]);
        let names_bytes = classes.keys().map(String::len).sum::<usize>()
            + classes.len() * std::mem::size_of::<String>();
        let index = handle
            .build_with_limit(&classes, &nested, names_bytes + classes.len() * 4 + 64)
            .unwrap();
        assert!(index.stats.spill_runs > 1);
        assert_eq!(index.stats.indexed_owners, classes.len());
        assert!(!index.stats.partial);
        let cancel = AtomicBool::new(false);
        let in_memory = handle
            .build_with_limit(&classes, &nested, usize::MAX)
            .unwrap();
        for target in [
            "target.Unrelated",
            "target.Root",
            "target.Child",
            "target.Other",
        ] {
            let mut spilled = index.matches(target, &cancel).unwrap().unwrap();
            let mut memory = in_memory.matches(target, &cancel).unwrap().unwrap();
            spilled.sort_unstable();
            spilled.dedup();
            memory.sort_unstable();
            memory.dedup();
            assert_eq!(spilled, memory, "{target}");
        }
        let child = index.matches("target.Child", &cancel).unwrap().unwrap();
        let child_names: Vec<_> = child
            .iter()
            .map(|id| index.names[*id as usize].as_str())
            .collect();
        assert!(child_names.contains(&"b.Root"));
        assert!(child_names.contains(&"c.Child"));
        let other = index.matches("target.Other", &cancel).unwrap().unwrap();
        assert!(
            other
                .iter()
                .any(|id| index.names[*id as usize] == "z.Other")
        );
        cancel.store(true, Ordering::Relaxed);
        assert!(index.matches("target.Child", &cancel).unwrap().is_none());
        let spill_path = index._spill_dir.as_ref().unwrap().0.clone();
        assert!(spill_path.exists());
        drop(index);
        assert!(!spill_path.exists());
    }

    #[test]
    fn unreadable_spill_falls_back_to_all_owners() {
        let handle = UsageIndexHandle::start(Arc::new(BTreeMap::new()), Arc::new(BTreeMap::new()));
        let classes = handle_fixture_classes();
        let names_bytes = classes.keys().map(String::len).sum::<usize>()
            + classes.len() * std::mem::size_of::<String>();
        let index = handle
            .build_with_limit(
                &classes,
                &BTreeMap::new(),
                names_bytes + classes.len() * 4 + 64,
            )
            .unwrap();
        assert!(!index.runs.is_empty());
        let run_path = index.runs[0].path.clone();
        let file = OpenOptions::new().write(true).open(&run_path).unwrap();
        file.set_len(0).unwrap();
        assert!(
            index
                .matches("target.Child", &AtomicBool::new(false))
                .is_err()
        );
        handle.candidates("unused", &AtomicBool::new(false), |_, _| {});
        handle.state.0.lock().unwrap().ready = Some(Arc::new(index));
        let owners = handle
            .candidates("target.Child", &AtomicBool::new(false), |_, _| {})
            .unwrap();
        assert_eq!(owners.len(), classes.len());
    }

    #[test]
    fn forced_spill_preserves_rendered_source_links() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.dex");
        let mut engine = crate::native_engine::NativeDexEngine::default();
        let project = engine.open(&path).unwrap();
        let classes = engine.shared_classes();
        let nested = engine.shared_nested_children();
        let handle = UsageIndexHandle::start(Arc::new(BTreeMap::new()), Arc::new(BTreeMap::new()));
        let index = handle.build_with_limit(&classes, &nested, 16).unwrap();
        assert!(index.stats.spill_runs > 1);
        let cancel = AtomicBool::new(false);
        for owner in &project.classes {
            let code = engine.render(owner).unwrap();
            for link in &code.links {
                if code
                    .definitions
                    .iter()
                    .any(|definition| definition.start == link.start && definition.end == link.end)
                {
                    continue;
                }
                let mut ids = index.matches(&link.label, &cancel).unwrap().unwrap();
                ids.extend_from_slice(&index.uncertain);
                assert!(
                    ids.iter().any(|id| index.names[*id as usize] == *owner),
                    "missing source link {} in {owner}",
                    link.label
                );
            }
        }
    }

    fn handle_fixture_classes() -> BTreeMap<String, DexClass> {
        let mut classes = BTreeMap::new();
        for (name, target) in [
            ("a.Unrelated", "Unrelated"),
            ("b.Root", "Root"),
            ("c.Child", "Child"),
            ("z.Other", "Other"),
        ] {
            let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
                .unwrap()
                .classes
                .remove(0);
            class.superclass = Some(format!("Ltarget/{target};").into());
            classes.insert(name.to_owned(), class);
        }
        classes
    }
}
