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
// extent per live entry. 100,000 ordinary class names fit without evicting data.
const META_LIMIT: usize = 96 * 1024 * 1024;
const DOC_LIMIT: usize = 32 * 1024 * 1024;
// Session-only format: magic, little-endian metadata length, source length.
// Source stays raw UTF-8 instead of being escaped and decoded as JSON.
const RECORD_MAGIC: &[u8; 4] = b"RDX1";
const RECORD_HEADER: usize = 12;
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

#[derive(Clone)]
struct Filter {
    bits: [u64; 32],
    unicode: bool,
}
impl Filter {
    fn new(texts: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut value = Self {
            bits: [0; 32],
            unicode: false,
        };
        for text in texts {
            let text = text.as_ref();
            value.unicode |= !text.is_ascii();
            for bytes in text.as_bytes().windows(3) {
                for hash in hashes(bytes) {
                    value.bits[hash / 64] |= 1 << (hash % 64);
                }
            }
        }
        value
    }
    fn may_match(&self, literal: &str, sensitive: bool, regex: bool) -> bool {
        if regex || !literal.is_ascii() || literal.len() < 3 || (!sensitive && self.unicode) {
            return true;
        }
        literal.as_bytes().windows(3).all(|bytes| {
            hashes(bytes)
                .into_iter()
                .all(|h| self.bits[h / 64] & (1 << (h % 64)) != 0)
        })
    }
}
fn hashes(bytes: &[u8]) -> [usize; 2] {
    let a = usize::from(bytes[0].to_ascii_lowercase());
    let b = usize::from(bytes[1].to_ascii_lowercase());
    let c = usize::from(bytes[2].to_ascii_lowercase());
    [
        (a * 31 * 31 + b * 31 + c) % 2048,
        (a * 131 + b * 17 + c * 7) % 2048,
    ]
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
fn decode_record(mut bytes: Vec<u8>) -> Option<(Stored, String)> {
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
impl DiskIndex {
    /// Membership only: avoids allocating or reading cached source during prefetch.
    pub fn contains(&self, key: &SearchTarget) -> bool {
        self.entries.contains_key(key)
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
        })
    }
    pub fn insert(&mut self, key: SearchTarget, value: &CachedValue) {
        let Some((doc, defs, note)) = value else {
            return;
        };
        if self.entries.contains_key(&key) {
            return;
        }
        let metadata = 768
            + match &key {
                SearchTarget::Class(name) => name.capacity(),
                _ => 0,
            };
        if self.entries.len() >= 100_000
            || self.metadata + metadata > META_LIMIT
            || self.bytes >= DISK_LIMIT
            || doc.source.len() > DOC_LIMIT
        {
            return;
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
        let Some(bytes) = encode_record(&stored, &doc.source) else {
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
        let filter = Filter::new(
            std::iter::once(doc.source.as_str()).chain(defs.iter().map(|d| d.name.as_str())),
        );
        self.bytes += bytes.len();
        self.metadata += metadata;
        self.entries.insert(
            key,
            Entry {
                offset,
                bytes: bytes.len(),
                metadata,
                digest: digest(&bytes),
                filter,
                complete: doc.metadata_complete,
                partial: note.is_some(),
            },
        );
    }
    pub fn remove(&mut self, key: &SearchTarget) {
        if let Some(entry) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
            self.metadata = self.metadata.saturating_sub(entry.metadata);
            self.release(entry.offset, entry.bytes);
        }
    }
    pub fn load(&mut self, key: &SearchTarget) -> Option<CachedValue> {
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
        assert!(!Filter::new(["ordinary text"]).may_match("zzzzzz", true, false));
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
        assert!(disk.entries.is_empty());
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
