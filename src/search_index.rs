//! Private, bounded, disposable project-session source index.
use crate::search::{CachedValue, SearchDocument, SearchTarget};
use rdx::engine::{CodeDefinition, CodeLink};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
const DISK_LIMIT: usize = 1024 * 1024 * 1024;
// Includes conservative hash-table slack, owned class-name capacity, and a free
// extent per live entry. The separate 256 MiB bound gives a 116,005-class
// project room for adaptive filters even when its source payload quota is full.
// Filter admission remains bounded and fails open when this budget is exhausted.
const META_LIMIT: usize = 256 * 1024 * 1024;
const ENTRY_LIMIT: usize = 150_000;
const ENTRY_BASE_METADATA: usize = 384;
const DOC_LIMIT: usize = 32 * 1024 * 1024;
// Session-only format: magic, little-endian metadata length, source length.
// Source stays raw UTF-8 instead of being escaped and decoded as JSON.
const RECORD_MAGIC: &[u8; 4] = b"RDX1";
const RECORD_HEADER: usize = 12;
const COMPRESSED_MAGIC: &[u8; 4] = b"RDXZ";
const COMPRESSED_HEADER: usize = 8;
static NEXT: AtomicU64 = AtomicU64::new(0);
#[derive(Default)]
struct OwnedDirectories {
    paths: HashSet<PathBuf>,
    shutting_down: bool,
}
fn registry() -> Arc<Mutex<OwnedDirectories>> {
    static REGISTRY: OnceLock<Arc<Mutex<OwnedDirectories>>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(OwnedDirectories::default())))
        .clone()
}
/// Install at process entry so normal GUI exit cleans indexes still owned by workers.
/// Forced termination/crashes can leave private temporary files for OS cleanup.
pub struct SessionCleanup {
    registry: Arc<Mutex<OwnedDirectories>>,
}
impl SessionCleanup {
    pub fn new() -> Self {
        Self {
            registry: registry(),
        }
    }
}
impl Drop for SessionCleanup {
    fn drop(&mut self) {
        let paths = {
            let mut owned = self
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            owned.shutting_down = true;
            owned.paths.drain().collect::<Vec<_>>()
        };
        for path in paths {
            let _ = fs::remove_dir_all(path);
        }
    }
}

// Deduplicate at most 64K folded trigrams; temporary storage stays bounded.
// Documents exceeding that diversity keep their cached source and fail open.
const FILTER_MAX_TRIGRAMS: usize = 64 * 1024;
const FILTER_MAX_BYTES: usize = 128 * 1024;
#[derive(Clone)]
struct Filter {
    bits: Vec<u64>,
    ascii_fold_exception: bool,
}
fn ascii_fold_exception(text: &str) -> bool {
    if text.is_ascii() {
        return false;
    }
    // Ask the search regex engine itself which non-ASCII scalars can match an
    // ASCII letter under its Unicode simple-case-folding rules.
    static ASCII_LETTER: OnceLock<regex::Regex> = OnceLock::new();
    let matcher = ASCII_LETTER.get_or_init(|| regex::Regex::new("(?i:[a-z])").unwrap());
    text.chars().filter(|ch| !ch.is_ascii()).any(|ch| {
        let mut buffer = [0; 4];
        matcher.is_match(ch.encode_utf8(&mut buffer))
    })
}
impl Filter {
    fn new(texts: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut unique = HashSet::new();
        let mut exception = false;
        for text in texts {
            let text = text.as_ref();
            exception |= ascii_fold_exception(text);
            for bytes in text.as_bytes().windows(3) {
                unique.insert(packed_trigram(bytes));
                if unique.len() > FILTER_MAX_TRIGRAMS {
                    return Self {
                        bits: Vec::new(),
                        ascii_fold_exception: exception,
                    };
                }
            }
        }
        let bit_count = (unique.len() * 10).clamp(256, FILTER_MAX_BYTES * 8);
        let mut value = Self {
            bits: vec![0; bit_count.div_ceil(64)],
            ascii_fold_exception: exception,
        };
        let bit_count = value.bits.len() * 64;
        for packed in unique {
            for hash in packed_hashes(packed, bit_count) {
                value.bits[hash / 64] |= 1 << (hash % 64);
            }
        }
        value
    }
    fn bytes(&self) -> usize {
        self.bits.capacity() * std::mem::size_of::<u64>()
    }
    fn may_match(&self, literal: &str, sensitive: bool, regex: bool) -> bool {
        if self.bits.is_empty()
            || regex
            || !literal.is_ascii()
            || literal.len() < 3
            || (!sensitive && self.ascii_fold_exception)
        {
            return true;
        }
        literal.as_bytes().windows(3).all(|bytes| {
            hashes(bytes, self.bits.len() * 64)
                .into_iter()
                .all(|h| self.bits[h / 64] & (1 << (h % 64)) != 0)
        })
    }
}
fn packed_trigram(bytes: &[u8]) -> u32 {
    u32::from(bytes[0].to_ascii_lowercase())
        | (u32::from(bytes[1].to_ascii_lowercase()) << 8)
        | (u32::from(bytes[2].to_ascii_lowercase()) << 16)
}
fn hashes(bytes: &[u8], bit_count: usize) -> [usize; 3] {
    packed_hashes(packed_trigram(bytes), bit_count)
}
fn packed_hashes(packed: u32, bit_count: usize) -> [usize; 3] {
    fn mix(mut value: u32) -> usize {
        value ^= value >> 16;
        value = value.wrapping_mul(0x7feb352d);
        value ^= value >> 15;
        value = value.wrapping_mul(0x846ca68b);
        (value ^ (value >> 16)) as usize
    }
    [
        mix(packed),
        mix(packed ^ 0x9e3779b9),
        mix(packed ^ 0x85ebca6b),
    ]
    .map(|hash| hash % bit_count)
}
struct Entry {
    offset: u64,
    bytes: usize,
    metadata: usize,
    digest: u64,
    filter: Filter,
    complete: bool,
    partial: bool,
}
pub struct DiskIndex {
    registry: Arc<Mutex<OwnedDirectories>>,
    directory: Option<PathBuf>,
    store: Option<fs::File>,
    store_len: usize,
    free: BTreeMap<u64, usize>,
    entries: HashMap<SearchTarget, Entry>,
    bytes: usize,
    metadata: usize,
    disabled: bool,
}
impl Default for DiskIndex {
    fn default() -> Self {
        Self {
            registry: registry(),
            directory: None,
            store: None,
            store_len: 0,
            free: BTreeMap::new(),
            entries: HashMap::new(),
            bytes: 0,
            metadata: 0,
            disabled: false,
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Stored {
    name: String,
    syntax: String,
    source_hash: Option<String>,
    metadata_complete: bool,
    links: Vec<(usize, usize, String)>,
    definitions: Vec<(usize, usize, String, String)>,
    note: Option<String>,
}
fn encode_record(stored: &Stored, source: &str) -> Option<Vec<u8>> {
    if source.len() > DOC_LIMIT {
        return None;
    }
    let mut bytes = vec![0; RECORD_HEADER];
    serde_json::to_writer(&mut bytes, stored).ok()?;
    let metadata_len = bytes.len() - RECORD_HEADER;
    if bytes.len().checked_add(source.len())? > DOC_LIMIT {
        return None;
    }
    bytes[..4].copy_from_slice(RECORD_MAGIC);
    bytes[4..8].copy_from_slice(&u32::try_from(metadata_len).ok()?.to_le_bytes());
    bytes[8..12].copy_from_slice(&u32::try_from(source.len()).ok()?.to_le_bytes());
    bytes.extend_from_slice(source.as_bytes());
    Some(bytes)
}
// Keep the raw record as the compatibility format and compress only when the
// complete container shrinks. Level 1 bounds first-scan CPU overhead.
fn encode_storage_record(stored: &Stored, source: &str) -> Option<Vec<u8>> {
    let raw = encode_record(stored, source)?;
    if raw.len() < 1024 {
        return Some(raw);
    }
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    if encoder.write_all(&raw).is_err() {
        return Some(raw);
    }
    let Ok(body) = encoder.finish() else {
        return Some(raw);
    };
    if body.len().checked_add(COMPRESSED_HEADER)? >= raw.len() {
        return Some(raw);
    }
    let mut compressed = Vec::with_capacity(COMPRESSED_HEADER + body.len());
    compressed.extend_from_slice(COMPRESSED_MAGIC);
    compressed.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    compressed.extend_from_slice(&body);
    Some(compressed)
}
fn decode_record(mut bytes: Vec<u8>) -> Option<(Stored, String)> {
    if bytes.len() > DOC_LIMIT {
        return None;
    }
    if bytes.starts_with(COMPRESSED_MAGIC) {
        let raw_len = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
        if !(RECORD_HEADER..=DOC_LIMIT).contains(&raw_len) {
            return None;
        }
        let body = bytes.get(COMPRESSED_HEADER..)?;
        let mut raw = vec![0; raw_len];
        let mut decoder = flate2::Decompress::new(false);
        let status = decoder
            .decompress(body, &mut raw, flate2::FlushDecompress::Finish)
            .ok()?;
        if status != flate2::Status::StreamEnd
            || decoder.total_in() != body.len() as u64
            || decoder.total_out() != raw_len as u64
        {
            return None;
        }
        bytes = raw;
    }
    if bytes.len() < RECORD_HEADER || bytes.len() > DOC_LIMIT || &bytes[..4] != RECORD_MAGIC {
        return None;
    }
    let metadata_len = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
    let source_len = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
    let source_start = RECORD_HEADER.checked_add(metadata_len)?;
    if source_start.checked_add(source_len)? != bytes.len() {
        return None;
    }
    let stored = serde_json::from_slice(bytes.get(RECORD_HEADER..source_start)?).ok()?;
    // Reuse the record allocation for the returned source rather than allocating
    // another full source buffer during JSON deserialization.
    bytes.drain(..source_start);
    let source = String::from_utf8(bytes).ok()?;
    Some((stored, source))
}
/// CPU-only preparation owned by a render worker until coordinator admission.
/// Cancellation drops the whole result, including negative filter evidence.
pub(super) struct PreparedEntry {
    filter: Filter,
    bytes: Option<Vec<u8>>,
    digest: u64,
    complete: bool,
    partial: bool,
}
impl PreparedEntry {
    pub(super) fn prepare(value: &CachedValue) -> Option<Self> {
        Self::prepare_cancellable(value, || false)
    }

    pub(super) fn prepare_cancellable(
        value: &CachedValue,
        cancelled: impl Fn() -> bool,
    ) -> Option<Self> {
        let (doc, defs, note) = value.as_ref()?;
        if cancelled() {
            return None;
        }
        let numeric_aliases: Vec<_> = doc
            .links
            .iter()
            .filter_map(rdx::resource_search::link_id)
            .flat_map(|id| [id.to_string(), format!("0x{id:08x}"), format!("0x{id:x}")])
            .collect();
        let filter = Filter::new(
            std::iter::once(doc.source.as_str())
                .chain(defs.iter().map(|d| d.name.as_str()))
                .chain(numeric_aliases.iter().map(String::as_str)),
        );
        if cancelled() {
            return None;
        }
        let stored = Stored {
            name: doc.name.clone(),
            syntax: doc.syntax.clone(),
            source_hash: doc.source_hash.clone(),
            metadata_complete: doc.metadata_complete,
            links: doc
                .links
                .iter()
                .map(|l| (l.start, l.end, l.label.clone()))
                .collect(),
            definitions: defs
                .iter()
                .map(|d| (d.start, d.end, d.kind.clone(), d.name.clone()))
                .collect(),
            note: note.clone(),
        };
        if cancelled() {
            return None;
        }
        let bytes = encode_storage_record(&stored, &doc.source);
        if cancelled() {
            return None;
        }
        let digest = bytes.as_deref().map(digest).unwrap_or(0);
        if cancelled() {
            return None;
        }
        Some(Self {
            filter,
            bytes,
            digest,
            complete: doc.metadata_complete,
            partial: note.is_some(),
        })
    }

    /// Owned allocations retained in a queued worker result.
    pub(super) fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.filter.bytes())
            .saturating_add(self.bytes.as_ref().map_or(0, Vec::capacity))
    }
}

impl DiskIndex {
    #[cfg(test)]
    pub(super) fn truncate_payload_for_test(&mut self) {
        self.store
            .as_mut()
            .expect("fixture has payload storage")
            .set_len(0)
            .unwrap();
    }
    /// Membership only: avoids allocating or reading cached source during prefetch.
    pub fn contains(&self, key: &SearchTarget) -> bool {
        self.entries.get(key).is_some_and(|entry| entry.bytes != 0)
    }

    /// Bounded-cache coverage for profiling: retained filters, payloads,
    /// charged metadata bytes, and stored payload bytes.
    pub(super) fn stats(&self) -> (usize, usize, usize, usize) {
        (
            self.entries.len(),
            self.entries
                .values()
                .filter(|entry| entry.bytes != 0)
                .count(),
            self.metadata,
            self.bytes,
        )
    }

    fn store(&mut self) -> Option<&mut fs::File> {
        if self.store.is_none() {
            let path = self.directory()?.join("sources.pack");
            let mut options = fs::OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            self.store = Some(options.open(path).ok()?);
        }
        self.store.as_mut()
    }

    fn release(&mut self, mut offset: u64, mut bytes: usize) {
        if let Some((&previous, &len)) = self.free.range(..offset).next_back()
            && previous + len as u64 == offset
        {
            self.free.remove(&previous);
            offset = previous;
            bytes += len;
        }
        if let Some((&next, &len)) = self.free.range(offset..).next()
            && offset + bytes as u64 == next
        {
            self.free.remove(&next);
            bytes += len;
        }
        if offset + bytes as u64 == self.store_len as u64 {
            self.store_len = offset as usize;
            if let Some(store) = &self.store {
                let _ = store.set_len(offset);
            }
        } else {
            self.free.insert(offset, bytes);
        }
    }

    fn directory(&mut self) -> Option<&PathBuf> {
        if self.disabled {
            return None;
        }
        if self.directory.is_none() {
            // Creation and registration share the shutdown lock: cleanup cannot miss a new directory.
            let mut owned = self
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if owned.shutting_down {
                self.disabled = true;
                return None;
            }
            for _ in 0..10 {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "rdx-search-{}-{}-{id}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()?
                        .as_nanos()
                ));
                let builder = fs::DirBuilder::new();
                #[cfg(unix)]
                let builder = {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = builder;
                    builder.mode(0o700);
                    builder
                };
                match builder.create(&path) {
                    Ok(()) => {
                        owned.paths.insert(path.clone());
                        self.directory = Some(path);
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(_) => {
                        self.disabled = true;
                        return None;
                    }
                }
            }
        }
        self.directory.as_ref()
    }
    pub fn rejects(
        &self,
        key: &SearchTarget,
        text: &str,
        sensitive: bool,
        regex: bool,
        definitions: bool,
    ) -> bool {
        self.entries.get(key).is_some_and(|entry| {
            !entry.partial
                && (!definitions || entry.complete)
                && !entry.filter.may_match(text, sensitive, regex)
                && (regex
                    || rdx::resource_search::query_id(text)
                        .is_none_or(|id| !entry.filter.may_match(&id.to_string(), true, false)))
        })
    }
    pub fn insert(&mut self, key: SearchTarget, value: &CachedValue) {
        if self.entries.contains_key(&key)
            || self.entries.len() >= ENTRY_LIMIT
            || self.metadata >= META_LIMIT
        {
            return;
        }
        if let Some(prepared) = PreparedEntry::prepare(value) {
            self.insert_prepared(key, prepared);
        }
    }

    pub(super) fn insert_prepared(&mut self, key: SearchTarget, prepared: PreparedEntry) {
        if self.entries.contains_key(&key) {
            return;
        }
        if self.entries.len() >= ENTRY_LIMIT || self.metadata >= META_LIMIT {
            return;
        }
        let PreparedEntry {
            mut filter,
            bytes,
            digest,
            complete,
            partial,
        } = prepared;
        let base_metadata = ENTRY_BASE_METADATA
            + match &key {
                SearchTarget::Class(name) => name.capacity(),
                _ => 0,
            };
        if self.metadata + base_metadata + filter.bytes() > META_LIMIT {
            filter.bits = Vec::new();
        }
        let metadata = base_metadata + filter.bytes();
        if self.metadata + metadata > META_LIMIT {
            return;
        }
        // Negative evidence is session metadata, independent of optional source
        // storage. A zero-byte entry never claims to contain a cached document.
        self.metadata += metadata;
        self.entries.insert(
            key.clone(),
            Entry {
                offset: 0,
                bytes: 0,
                metadata,
                digest: 0,
                filter,
                complete,
                partial,
            },
        );
        if self.bytes >= DISK_LIMIT {
            return;
        }
        let Some(bytes) = bytes else {
            return;
        };
        if bytes.len() > DOC_LIMIT || self.bytes + bytes.len() > DISK_LIMIT {
            return;
        }
        if self.store().is_none() {
            return;
        }
        let reused = self
            .free
            .iter()
            .find_map(|(&offset, &len)| (len >= bytes.len()).then_some((offset, len)));
        let offset = if let Some((offset, len)) = reused {
            self.free.remove(&offset);
            if len > bytes.len() {
                self.free
                    .insert(offset + bytes.len() as u64, len - bytes.len());
            }
            offset
        } else {
            if self.store_len + bytes.len() > DISK_LIMIT {
                return;
            }
            let offset = self.store_len as u64;
            self.store_len += bytes.len();
            offset
        };
        let store = self.store.as_mut().unwrap();
        if store
            .seek(SeekFrom::Start(offset))
            .and_then(|_| store.write_all(&bytes))
            .is_err()
        {
            self.release(offset, bytes.len());
            return;
        }
        self.bytes += bytes.len();
        let entry = self.entries.get_mut(&key).unwrap();
        entry.offset = offset;
        entry.bytes = bytes.len();
        entry.digest = digest;
    }
    pub fn remove(&mut self, key: &SearchTarget) {
        if let Some(entry) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
            self.metadata = self.metadata.saturating_sub(entry.metadata);
            if entry.bytes != 0 {
                self.release(entry.offset, entry.bytes);
            }
        }
    }
    pub fn load(&mut self, key: &SearchTarget) -> Option<CachedValue> {
        // A valid filter-only entry survives a source miss. Corrupt payloads
        // below invalidate the whole entry and force fresh source evidence.
        if !self.contains(key) {
            return None;
        }
        let result = (|| {
            let entry = self.entries.get(key)?;
            let store = self.store.as_mut()?;
            if entry.bytes > DOC_LIMIT
                || store.metadata().ok()?.len() < entry.offset + entry.bytes as u64
            {
                return None;
            }
            let mut bytes = vec![0; entry.bytes];
            store.seek(SeekFrom::Start(entry.offset)).ok()?;
            store.read_exact(&mut bytes).ok()?;
            if digest(&bytes) != entry.digest {
                return None;
            }
            let (stored, source) = decode_record(bytes)?;
            Some(Some((
                Arc::new(SearchDocument {
                    target: key.clone(),
                    name: stored.name,
                    source,
                    syntax: stored.syntax,
                    source_hash: stored.source_hash,
                    metadata_complete: stored.metadata_complete,
                    links: stored
                        .links
                        .into_iter()
                        .map(|(start, end, label)| CodeLink { start, end, label })
                        .collect(),
                }),
                Arc::new(
                    stored
                        .definitions
                        .into_iter()
                        .map(|(start, end, kind, name)| CodeDefinition {
                            start,
                            end,
                            kind,
                            name,
                        })
                        .collect(),
                ),
                stored.note,
            )))
        })();
        if result.is_none() {
            self.remove(key);
        }
        result
    }
}
fn digest(bytes: &[u8]) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    hash.finish()
}
impl Drop for DiskIndex {
    fn drop(&mut self) {
        // Windows requires the packed file handle closed before directory removal.
        self.store.take();
        if let Some(path) = &self.directory {
            let mut owned = self
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match fs::remove_dir_all(path) {
                Ok(()) => {
                    owned.paths.remove(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    owned.paths.remove(path);
                }
                Err(_) => {} // Keep ownership registered so shutdown can retry.
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn value(key: &SearchTarget) -> CachedValue {
        Some((
            Arc::new(SearchDocument {
                target: key.clone(),
                name: "Example".into(),
                source: "return 42;".into(),
                syntax: "java".into(),
                source_hash: None,
                metadata_complete: false,
                links: vec![],
            }),
            Arc::new(vec![]),
            None,
        ))
    }
    #[test]
    fn prepared_insert_matches_direct_insert_and_preserves_metadata() {
        let key = SearchTarget::Class("Prepared".into());
        let mut value = value(&key);
        let (doc, definitions, note) = value.as_mut().unwrap();
        let doc = Arc::get_mut(doc).unwrap();
        doc.source = "R.anim.fade\n".repeat(512);
        doc.metadata_complete = true;
        doc.source_hash = Some("hash".into());
        doc.links.push(CodeLink {
            start: 0,
            end: 11,
            label: "resource://7f010066/anim/fade".into(),
        });
        Arc::get_mut(definitions).unwrap().push(CodeDefinition {
            start: 0,
            end: 11,
            kind: "method".into(),
            name: "definitionOnly".into(),
        });
        *note = Some("partial evidence".into());
        let prepared = PreparedEntry::prepare(&value).unwrap();
        assert!(
            prepared
                .bytes
                .as_ref()
                .unwrap()
                .starts_with(COMPRESSED_MAGIC)
        );
        assert!(prepared.estimated_bytes() >= prepared.bytes.as_ref().unwrap().capacity());
        let mut direct = DiskIndex::default();
        let mut worker = DiskIndex::default();
        direct.insert(key.clone(), &value);
        worker.insert_prepared(key.clone(), prepared);
        assert_eq!(direct.stats(), worker.stats());
        for query in ["definitionOnly", "2130772070", "0x7f010066", "absent"] {
            for definitions in [false, true] {
                assert_eq!(
                    direct.rejects(&key, query, true, false, definitions),
                    worker.rejects(&key, query, true, false, definitions)
                );
            }
        }
        let loaded = worker.load(&key).unwrap().unwrap();
        let original = value.as_ref().unwrap();
        assert_eq!(loaded.0.source, original.0.source);
        assert_eq!(loaded.0.source_hash, original.0.source_hash);
        assert_eq!(loaded.0.metadata_complete, original.0.metadata_complete);
        assert_eq!(loaded.0.links[0].label, original.0.links[0].label);
        assert_eq!(loaded.1[0].name, original.1[0].name);
        assert_eq!(loaded.2, original.2);
        worker.insert_prepared(key.clone(), PreparedEntry::prepare(&value).unwrap());
        assert_eq!(direct.stats(), worker.stats());
    }

    #[test]
    fn prepared_admission_preserves_coordinator_budgets() {
        let key = SearchTarget::Class("Budget".into());
        let value = value(&key);
        let mut disk = DiskIndex::default();
        disk.bytes = DISK_LIMIT;
        disk.insert_prepared(key.clone(), PreparedEntry::prepare(&value).unwrap());
        assert!(!disk.contains(&key));
        assert!(disk.rejects(&key, "absent-boundary", true, false, false));
        assert!(disk.directory.is_none());

        let mut disk = DiskIndex::default();
        let base = ENTRY_BASE_METADATA
            + match &key {
                SearchTarget::Class(name) => name.capacity(),
                _ => 0,
            };
        disk.metadata = META_LIMIT - base;
        disk.insert_prepared(key.clone(), PreparedEntry::prepare(&value).unwrap());
        assert_eq!(disk.metadata, META_LIMIT);
        assert!(disk.entries[&key].filter.bits.is_empty());
        assert!(!disk.rejects(&key, "absent-boundary", true, false, false));
        let other = SearchTarget::Class("Rejected".into());
        disk.insert_prepared(other.clone(), PreparedEntry::prepare(&value).unwrap());
        assert!(!disk.entries.contains_key(&other));
    }

    #[test]
    fn cancelled_preparation_never_produces_partial_evidence() {
        let key = SearchTarget::Class("Cancelled".into());
        let value = value(&key);
        for stop in 0..5 {
            let checks = std::cell::Cell::new(0);
            let prepared = PreparedEntry::prepare_cancellable(&value, || {
                let current = checks.get();
                checks.set(current + 1);
                current == stop
            });
            assert!(prepared.is_none());
        }
        assert!(PreparedEntry::prepare(&None).is_none());
    }

    #[test]
    fn cached_resource_symbols_remain_searchable_by_numeric_alias() {
        let key = SearchTarget::Class("example.Resources".into());
        let mut doc = value(&key);
        let document = Arc::get_mut(&mut doc.as_mut().unwrap().0).unwrap();
        document.source = "R.anim.fade".into();
        document.links = vec![CodeLink {
            start: 0,
            end: 11,
            label: "resource://7f010066/anim/fade".into(),
        }];
        let mut disk = DiskIndex::default();
        disk.insert_prepared(key.clone(), PreparedEntry::prepare(&doc).unwrap());
        for query in ["2130772070", "0x7f010066", "0X7F010066", "02130772070"] {
            assert!(!disk.rejects(&key, query, true, false, false));
        }
        assert_eq!(
            disk.load(&key).unwrap().unwrap().0.links[0].label,
            "resource://7f010066/anim/fade"
        );
    }
    #[test]
    fn packed_index_admits_one_hundred_thousand_classes() {
        let mut disk = DiskIndex::default();
        for i in 0..100_000 {
            let key =
                SearchTarget::Class(format!("com.example.application.feature.Component{i:06}"));
            disk.insert(key.clone(), &value(&key));
        }
        assert_eq!(disk.entries.len(), 100_000);
        assert!(disk.metadata < META_LIMIT);
        assert!(disk.bytes < 32 * 1024 * 1024);
        let directory = disk.directory.as_ref().unwrap();
        assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
        // Reopen the file handle, retaining this session's metadata.
        disk.store = Some(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(directory.join("sources.pack"))
                .unwrap(),
        );
        for i in [0, 45_370, 99_999] {
            let key =
                SearchTarget::Class(format!("com.example.application.feature.Component{i:06}"));
            assert!(disk.contains(&key));
            assert_eq!(disk.load(&key).unwrap().unwrap().0.source, "return 42;");
        }
    }
    #[test]
    fn full_project_filters_survive_payload_quota() {
        let mut disk = DiskIndex::default();
        disk.bytes = DISK_LIMIT;
        for i in 0..116_005 {
            let key = SearchTarget::Class(format!("com.example.application.Component{i:06}"));
            disk.insert(key, &value(&SearchTarget::Class(String::new())));
        }
        assert_eq!(disk.entries.len(), 116_005);
        assert!(disk.metadata < META_LIMIT);
        assert!(disk.directory.is_none());
        let key = SearchTarget::Class("com.example.application.Component116004".into());
        assert!(disk.rejects(&key, "absent-boundary", true, false, false));
        assert!(!disk.contains(&key));
        assert!(disk.load(&key).is_none());
        // Possible matches must obtain source from the engine; a source miss
        // must not erase independent, valid negative evidence.
        assert!(!disk.rejects(&key, "return", true, false, false));
        assert!(disk.rejects(&key, "absent-boundary", true, false, false));
        assert!(!disk.rejects(&key, "absent-boundary", true, true, false));
        assert!(!disk.rejects(&key, "absent-boundary", true, false, true));
    }

    #[test]
    fn filter_only_entries_preserve_unicode_and_partial_fail_open() {
        let key = SearchTarget::Class("Unicode".into());
        let mut value = value(&key);
        Arc::get_mut(&mut value.as_mut().unwrap().0).unwrap().source = "ſymbol Kelvin".into();
        let mut disk = DiskIndex::default();
        disk.disabled = true; // Source-file creation failure retains the filter.
        disk.insert(key.clone(), &value);
        assert!(!disk.contains(&key));
        assert!(!disk.rejects(&key, "symbol", false, false, false));
        assert!(!disk.rejects(&key, "kelvin", false, false, false));
        assert!(!disk.rejects(&key, "ſymbol", true, false, false));
        disk.remove(&key);
        value.as_mut().unwrap().2 = Some("partial".into());
        disk.insert(key.clone(), &value);
        assert!(!disk.rejects(&key, "absent-boundary", true, false, false));
    }

    #[test]
    fn removed_records_reuse_space_and_coalesce() {
        let mut disk = DiskIndex::default();
        let keys: Vec<_> = (0..3)
            .map(|i| SearchTarget::Class(format!("C{i}")))
            .collect();
        for key in &keys {
            disk.insert(key.clone(), &value(key));
        }
        let original_len = disk.store_len;
        for _ in 0..100 {
            disk.remove(&keys[1]);
            disk.insert(keys[1].clone(), &value(&keys[1]));
            assert_eq!(disk.store_len, original_len);
            assert!(disk.load(&keys[0]).is_some());
            assert!(disk.load(&keys[2]).is_some());
        }
        disk.remove(&keys[0]);
        disk.remove(&keys[1]);
        disk.remove(&keys[2]);
        assert_eq!(disk.store_len, 0);
        assert_eq!(disk.bytes, 0);
        assert_eq!(disk.metadata, 0);
        assert!(disk.free.is_empty());
        assert_eq!(disk.store.as_ref().unwrap().metadata().unwrap().len(), 0);
    }
    #[test]
    fn filters_never_reject_matching_literals() {
        for source in [
            "class Example { String token = secret; }",
            "CAFÉ ſymbol Kelvin",
            "αβ😀 target",
        ] {
            let filter = Filter::new([source]);
            for (start, _) in source.char_indices() {
                for (relative, _) in source[start..].char_indices() {
                    let end = start + relative;
                    if end > start {
                        let literal = &source[start..end];
                        assert!(filter.may_match(literal, true, false));
                        assert!(filter.may_match(&literal.to_lowercase(), false, false));
                    }
                }
            }
            assert!(filter.may_match(".*", true, true));
        }
        assert!(Filter::new(["ſymbol"]).may_match("symbol", false, false));
        assert!(Filter::new(["Kelvin"]).may_match("kelvin", false, false));
        let unicode_identifiers = Filter::new(["class αβγ { String 😀 = target; }"]);
        assert!(!unicode_identifiers.ascii_fold_exception);
        assert!(!unicode_identifiers.may_match("absent_ascii_token", false, false));
        assert!(unicode_identifiers.may_match("TARGET", false, false));
        assert!(unicode_identifiers.may_match("target", true, false));
        assert!(unicode_identifiers.may_match("αβγ", false, false));
        // ASCII-sensitive queries can still reject even when Unicode folding
        // would require a conservative case-insensitive answer.
        assert!(!Filter::new(["ſymbol Kelvin"]).may_match("absent_ascii_token", true, false));
        assert!(!Filter::new(["ordinary text"]).may_match("zzzzzz", true, false));
    }
    #[test]
    fn adaptive_filters_reject_absent_text_in_diverse_large_sources() {
        let mut state = 17u32;
        let source: String = (0..200_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                char::from(b'a' + (state % 25) as u8)
            })
            .collect();
        let filter = Filter::new([source.as_str()]);
        assert!(filter.bytes() > 256);
        assert!(filter.bytes() <= FILTER_MAX_BYTES);
        assert!(!filter.may_match("~absent-boundary~", true, false));
        for offset in (0..source.len() - 32).step_by(997) {
            assert!(filter.may_match(&source[offset..offset + 32], true, false));
            assert!(filter.may_match(
                &source[offset..offset + 32].to_ascii_uppercase(),
                false,
                false
            ));
        }
        let large = Filter::new(["abc".repeat(400_000)]);
        assert!(large.bytes() < 1024);
        assert!(large.may_match("abcabc", true, false));
        assert!(!large.may_match("absent-boundary", true, false));
    }

    #[test]
    fn adaptive_filter_heap_is_charged_to_metadata_budget() {
        let mut disk = DiskIndex::default();
        let key = SearchTarget::Class("Adaptive".into());
        let value = Some((
            Arc::new(SearchDocument {
                target: key.clone(),
                name: "Adaptive".into(),
                source: "class Adaptive { int field; }".repeat(4096),
                syntax: "java".into(),
                links: vec![],
                source_hash: None,
                metadata_complete: true,
            }),
            Arc::new(vec![]),
            None,
        ));
        disk.insert(key.clone(), &value);
        let entry = &disk.entries[&key];
        assert_eq!(disk.metadata, entry.metadata);
        assert!(entry.metadata >= ENTRY_BASE_METADATA + entry.filter.bytes());
        assert!(entry.filter.bytes() <= FILTER_MAX_BYTES);
        assert!(disk.metadata <= META_LIMIT);
        disk.remove(&key);
        assert_eq!(disk.metadata, 0);
        // Preserve source admission when only the optional filter cannot fit.
        disk.metadata = META_LIMIT - 400;
        disk.insert(key.clone(), &value);
        assert!(disk.entries.contains_key(&key));
        assert!(disk.entries[&key].filter.bits.is_empty());
        assert!(disk.load(&key).is_some());
        assert!(disk.metadata <= META_LIMIT);
    }
    #[test]
    fn disk_roundtrip_corruption_and_cleanup() {
        let mut disk = DiskIndex::default();
        let key = SearchTarget::Class("Example".into());
        let value = Some((
            Arc::new(SearchDocument {
                target: key.clone(),
                name: "Example".into(),
                source: "class Example {}".into(),
                syntax: "java".into(),
                links: vec![],
                source_hash: Some("hash".into()),
                metadata_complete: true,
            }),
            Arc::new(vec![]),
            None,
        ));
        disk.insert(key.clone(), &value);
        let directory = disk.directory.clone().unwrap();
        assert_eq!(
            disk.load(&key).unwrap().unwrap().0.source,
            "class Example {}"
        );
        fs::write(directory.join("sources.pack"), b"corrupt").unwrap();
        assert!(disk.load(&key).is_none());
        assert_eq!(disk.bytes, 0);
        assert_eq!(disk.metadata, 0);
        assert!(!disk.rejects(&key, "absent-boundary", true, false, false));
        disk.insert(key.clone(), &value);
        drop(disk);
        assert!(!directory.exists());
    }
    #[test]
    fn raw_source_roundtrip_preserves_unicode_and_metadata() {
        let mut disk = DiskIndex::default();
        let key = SearchTarget::Class("example.Δοκιμή".into());
        let source = "class Δοκιμή {\n\tString s = \"😀\\n\"; /* café\0 */\n}";
        let value = Some((
            Arc::new(SearchDocument {
                target: key.clone(),
                name: "Δοκιμή".into(),
                source: source.into(),
                syntax: "java".into(),
                source_hash: Some("original-source-hash".into()),
                metadata_complete: true,
                links: vec![CodeLink {
                    start: 6,
                    end: 18,
                    label: "example.Δοκιμή".into(),
                }],
            }),
            Arc::new(vec![CodeDefinition {
                start: 6,
                end: 18,
                kind: "class".into(),
                name: "Δοκιμή".into(),
            }]),
            Some("partial αβ".into()),
        ));
        disk.insert(key.clone(), &value);
        let packed = fs::read(disk.directory.as_ref().unwrap().join("sources.pack")).unwrap();
        assert!(packed.ends_with(source.as_bytes()));
        let (doc, definitions, note) = disk.load(&key).unwrap().unwrap();
        assert_eq!(doc.source, source);
        assert_eq!(doc.name, "Δοκιμή");
        assert_eq!(doc.source_hash.as_deref(), Some("original-source-hash"));
        assert!(doc.metadata_complete);
        assert_eq!(doc.links[0].label, "example.Δοκιμή");
        assert_eq!((doc.links[0].start, doc.links[0].end), (6, 18));
        assert_eq!(definitions[0].name, "Δοκιμή");
        assert_eq!(note.as_deref(), Some("partial αβ"));
    }
    #[test]
    fn compressed_records_roundtrip_and_bound_inflation() {
        let stored = Stored {
            name: "Δοκιμή".into(),
            syntax: "java".into(),
            source_hash: Some("source-hash".into()),
            metadata_complete: true,
            links: vec![(1, 5, "example.Target".into())],
            definitions: vec![(2, 6, "method".into(), "μέθοδος".into())],
            note: None,
        };
        let source = "class Δοκιμή { String s = \"😀 café\"; }\n".repeat(4096);
        let raw = encode_record(&stored, &source).unwrap();
        let compressed = encode_storage_record(&stored, &source).unwrap();
        assert!(compressed.starts_with(COMPRESSED_MAGIC));
        assert!(compressed.len() < raw.len());
        for encoded in [raw, compressed.clone()] {
            let (decoded, text) = decode_record(encoded).unwrap();
            assert_eq!(text, source);
            assert_eq!(decoded.name, stored.name);
            assert_eq!(decoded.links, stored.links);
            assert_eq!(decoded.definitions, stored.definitions);
            assert!(decoded.metadata_complete);
        }
        for corruption in 0..7 {
            let mut bytes = compressed.clone();
            match corruption {
                0 => bytes.truncate(7),
                1 => bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes()),
                2 => bytes[4..8].copy_from_slice(&12u32.to_le_bytes()),
                3 => {
                    let raw_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
                    bytes[4..8].copy_from_slice(&(raw_len + 1).to_le_bytes());
                }
                4 => {
                    bytes.pop();
                }
                5 => bytes.push(0), // Trailing compressed bytes are invalid.
                _ => bytes[COMPRESSED_HEADER..].fill(0xff),
            }
            assert!(
                decode_record(bytes).is_none(),
                "compressed corruption {corruption}"
            );
        }
    }

    #[test]
    fn malformed_frames_are_removed_even_with_matching_digest() {
        let key = SearchTarget::Class("Example".into());
        for corruption in 0..6 {
            let mut disk = DiskIndex::default();
            disk.insert(key.clone(), &value(&key));
            let path = disk.directory.as_ref().unwrap().join("sources.pack");
            let mut bytes = fs::read(&path).unwrap();
            match corruption {
                0 => bytes[..4].copy_from_slice(b"BAD!"),
                1 => bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes()),
                2 => bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes()),
                3 => bytes[RECORD_HEADER] = b'!',
                4 => *bytes.last_mut().unwrap() = 0xff,
                _ => {
                    bytes.pop();
                }
            }
            fs::write(path, &bytes).unwrap();
            let entry = disk.entries.get_mut(&key).unwrap();
            entry.digest = digest(&bytes);
            // A truncated file must be caught independently of digest checking.
            assert!(disk.load(&key).is_none(), "corruption {corruption}");
            assert!(!disk.contains(&key));
            assert_eq!(disk.bytes, 0);
        }
        assert!(decode_record(vec![0; RECORD_HEADER - 1]).is_none());
    }
    #[test]
    fn index_budgets_stop_admission_and_partial_text_is_not_rejected() {
        let key = SearchTarget::Class("Example".into());
        let value = Some((
            Arc::new(SearchDocument {
                target: key.clone(),
                name: "Example".into(),
                source: "class Example {}".into(),
                syntax: "java".into(),
                links: vec![],
                source_hash: None,
                metadata_complete: true,
            }),
            Arc::new(vec![]),
            Some("Truncated".into()),
        ));
        let mut disk = DiskIndex::default();
        disk.bytes = DISK_LIMIT;
        disk.insert(key.clone(), &value);
        assert!(disk.directory.is_none());
        assert_eq!(disk.entries.len(), 1);
        assert!(!disk.contains(&key));
        disk.remove(&key);
        disk.bytes = 0;
        disk.metadata = META_LIMIT;
        disk.insert(key.clone(), &value);
        assert!(disk.entries.is_empty());
        disk.metadata = 0;
        disk.insert(key.clone(), &value);
        assert!(!disk.rejects(&key, "ZZZZZZZZ", true, false, false));
    }
    #[test]
    fn session_shutdown_cleans_live_indexes_and_prevents_creation() {
        let registry = Arc::new(Mutex::new(OwnedDirectories::default()));
        let guard = SessionCleanup {
            registry: registry.clone(),
        };
        let mut live = DiskIndex::default();
        live.registry = registry.clone();
        let path = live.directory().unwrap().clone();
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
        let mut late = DiskIndex::default();
        late.registry = registry;
        assert!(late.directory().is_none());
        drop(live);
    }
}
