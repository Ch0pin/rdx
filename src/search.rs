//! Bounded in-process native project search. Offsets are Unicode scalar indices.
use rdx::{
    apk::{Archive, Preview},
    engine::{CodeDefinition, CodeLink, DecompilerEngine, DefinitionNameIndex, NativeEngine},
};
use regex::{Regex, RegexBuilder};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_HITS: usize = 1000;
const MAX_RETAINED: usize = 32 * 1024 * 1024;
mod streaming;

#[derive(Clone, Debug, PartialEq)]
pub struct SearchQuery {
    pub text: String,
    pub package: String,
    pub case_sensitive: bool,
    pub regex: bool,
    pub classes: bool,
    pub methods: bool,
    pub fields: bool,
    pub code: bool,
    pub resources: bool,
    pub comments: bool,
    pub excluded_packages: Vec<String>,
}
impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            package: String::new(),
            case_sensitive: false,
            regex: false,
            classes: false,
            methods: false,
            fields: false,
            code: true,
            resources: false,
            comments: false,
            excluded_packages: crate::settings::default_excluded_packages(),
        }
    }
}
pub struct CompiledQuery {
    query: SearchQuery,
    matcher: Regex,
}
impl CompiledQuery {
    pub fn new(mut query: SearchQuery) -> Result<Self, String> {
        if query.text.is_empty() {
            return Err("Enter search text.".into());
        }
        if query.text.len() > 4096 {
            return Err("Search text is limited to 4096 bytes.".into());
        }
        if !(query.classes
            || query.methods
            || query.fields
            || query.code
            || query.resources
            || query.comments)
        {
            return Err("Select at least one search scope.".into());
        }
        query.package = query.package.trim().trim_end_matches('.').to_owned();
        let pattern = if query.regex {
            query.text.clone()
        } else {
            regex::escape(&query.text)
        };
        let matcher = RegexBuilder::new(&pattern)
            .case_insensitive(!query.case_sensitive)
            .size_limit(2 * 1024 * 1024)
            .dfa_size_limit(2 * 1024 * 1024)
            .build()
            .map_err(|e| format!("Invalid search expression: {e}"))?;
        Ok(Self { query, matcher })
    }
    fn accepts_class(&self, name: &str) -> bool {
        if self
            .query
            .excluded_packages
            .iter()
            .any(|pattern| package_matches(name, pattern))
        {
            return false;
        }
        let package = name.rsplit_once('.').map_or("", |(p, _)| p);
        self.query.package.is_empty()
            || package == self.query.package
            || package
                .strip_prefix(&self.query.package)
                .is_some_and(|suffix| suffix.starts_with('.'))
    }

    fn ordered_classes<'a>(&self, classes: &'a [String]) -> Vec<&'a String> {
        let mut selected: Vec<_> = classes
            .iter()
            .filter(|name| self.accepts_class(name))
            .collect();
        // Stable ordering within each group; app and other nonstandard classes
        // reach the UI before bundled platform libraries, including with a hit cap.
        selected.sort_by_key(|name| is_standard_package(name));
        selected
    }
}

fn is_standard_package(name: &str) -> bool {
    crate::settings::STANDARD_PACKAGES
        .iter()
        .any(|pattern| package_matches(name, pattern))
}

fn package_matches(name: &str, pattern: &str) -> bool {
    let root = pattern.strip_suffix(".*").unwrap_or(pattern);
    name.strip_prefix(root)
        .is_some_and(|suffix| suffix.starts_with('.'))
}

pub fn normalize_exclusion(input: &str) -> Option<String> {
    let root = input.trim().strip_suffix(".*").unwrap_or(input.trim());
    if root.len() > 256
        || root.split('.').any(|part| {
            part.is_empty()
                || !part
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
    {
        return None;
    }
    Some(format!("{root}.*"))
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SearchTarget {
    Class(String),
    Resource(usize),
}
pub struct SearchDocument {
    pub target: SearchTarget,
    pub name: String,
    pub source: String,
    pub syntax: String,
    pub links: Vec<CodeLink>,
    pub source_hash: Option<String>,
    pub metadata_complete: bool,
}
#[derive(Clone)]
pub struct SearchHit {
    pub document: Arc<SearchDocument>,
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub kind: String,
    pub preview: String,
    /// UTF-8 byte range of this hit within the bounded preview.
    pub preview_match: std::ops::Range<usize>,
}
pub enum SearchUpdate {
    Batch(Vec<SearchHit>),
    Progress {
        scanned: usize,
        total: usize,
        hits: usize,
        skipped: usize,
    },
}
#[derive(Default)]
pub struct SearchSummary {
    pub scanned: usize,
    pub total: usize,
    pub hits: usize,
    pub skipped: usize,
    pub cancelled: bool,
    pub limited: bool,
    pub errors: Vec<String>,
    retained_bytes: usize,
}

// Java lexical comments; strings, characters and text blocks shield comment markers.
fn comments(source: &str) -> Vec<(usize, usize)> {
    let b = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            let start = i;
            i += 2;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            out.push((start, i));
        } else if b[i..].starts_with(b"/*") {
            let start = i;
            i += 2;
            while i < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }
            i = (i + 2).min(b.len());
            out.push((start, i));
        } else if b[i..].starts_with(b"\"\"\"") {
            i += 3;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                } else if b[i..].starts_with(b"\"\"\"") {
                    i += 3;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if b[i] == b'\"' || b[i] == b'\'' {
            let quote = b[i];
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                } else if b[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

fn matches(
    document: Arc<SearchDocument>,
    definitions: &[CodeDefinition],
    query: &CompiledQuery,
    remaining: usize,
) -> Vec<SearchHit> {
    let source = &document.source;
    let char_count = if definitions.is_empty() {
        0
    } else {
        source.chars().count()
    };
    let mut found = BTreeMap::<(usize, usize), String>::new();
    for d in definitions {
        let selected = match d.kind.as_str() {
            "class" => query.query.classes,
            "method" => query.query.methods,
            "field" => query.query.fields,
            _ => false,
        };
        if selected && d.start < d.end && d.end <= char_count && query.matcher.is_match(&d.name) {
            found.insert((d.start, d.end), d.kind.clone());
            if found.len() >= remaining {
                break;
            }
        }
    }
    let is_resource = matches!(document.target, SearchTarget::Resource(_));
    if found.len() < remaining && (is_resource || query.query.code || query.query.comments) {
        let mut lazy_spans = None;
        let all_text = is_resource || (query.query.code && query.query.comments);
        let mut span_index = 0;
        let (mut byte_cursor, mut char_cursor) = (0, 0);
        for m in query.matcher.find_iter(source) {
            if m.is_empty() {
                continue;
            }
            let spans = lazy_spans.get_or_insert_with(|| {
                if all_text {
                    Vec::new()
                } else {
                    comments(source)
                }
            });
            while span_index < spans.len() && spans[span_index].1 <= m.start() {
                span_index += 1;
            }
            let in_comment = spans
                .get(span_index)
                .is_some_and(|&(start, end)| start <= m.start() && m.end() <= end);
            let overlaps_comment = spans
                .get(span_index)
                .is_some_and(|&(start, end)| start < m.end() && end > m.start());
            if !(all_text
                || (query.query.comments && in_comment)
                || (query.query.code && !overlaps_comment))
            {
                continue;
            }
            char_cursor += source[byte_cursor..m.start()].chars().count();
            let start = char_cursor;
            let end = start + m.as_str().chars().count();
            byte_cursor = m.end();
            char_cursor = end;
            if found.keys().any(|&(a, b)| a < end && start < b) {
                continue;
            }
            found.insert(
                (start, end),
                if is_resource {
                    "resource"
                } else if in_comment {
                    "comment"
                } else {
                    "code"
                }
                .into(),
            );
            if found.len() >= remaining {
                break;
            }
        }
    }
    let (mut byte_cursor, mut char_cursor, mut line, mut row_start) = (0, 0, 1, 0);
    found
        .into_iter()
        .map(|((start, end), kind)| {
            let byte = byte_cursor
                + source[byte_cursor..]
                    .char_indices()
                    .nth(start - char_cursor)
                    .map_or(source.len() - byte_cursor, |(i, _)| i);
            for (i, _) in source[byte_cursor..byte].match_indices('\n') {
                line += 1;
                row_start = byte_cursor + i + 1;
            }
            byte_cursor = byte;
            char_cursor = start;
            let before: Vec<char> = source[row_start..byte].chars().rev().take(61).collect();
            let mut preview = String::new();
            if before.len() > 60 {
                preview.push('…');
            }
            preview.extend(before.iter().take(60).rev());
            let mut preview = preview.trim_start().to_owned();
            let match_start = preview.len();
            let tail: String = source[byte..]
                .chars()
                .take_while(|c| *c != '\n')
                .take(180)
                .collect();
            let match_end = match_start
                + tail
                    .char_indices()
                    .nth(end - start)
                    .map_or(tail.len(), |(i, _)| i);
            preview.push_str(&tail);
            SearchHit {
                document: document.clone(),
                start,
                end,
                line,
                kind,
                preview,
                preview_match: match_start..match_end,
            }
        })
        .collect()
}

pub(crate) type CachedValue = Option<(
    Arc<SearchDocument>,
    Arc<Vec<CodeDefinition>>,
    Option<String>,
)>;

/// Query-independent snapshots, invalidated whenever the caller's project generation changes.
/// Stop admission at capacity rather than evicting the entire cache on each sequential scan.
pub struct SearchCache {
    project_id: Option<u64>,
    entries: HashMap<SearchTarget, CachedValue>,
    bytes: usize,
    budget: usize,
    pub hits: usize,
    pub disk_hits: usize,
    pub index_rejections: usize,
    pub source_fetches: usize,
    disk: crate::search_index::DiskIndex,
    legacy: bool,
}
impl Default for SearchCache {
    fn default() -> Self {
        Self {
            project_id: None,
            entries: HashMap::new(),
            bytes: 0,
            budget: 64 * 1024 * 1024,
            hits: 0,
            disk_hits: 0,
            index_rejections: 0,
            source_fetches: 0,
            disk: crate::search_index::DiskIndex::default(),
            legacy: false,
        }
    }
}
impl SearchCache {
    pub fn legacy_for_benchmark() -> Self {
        Self {
            legacy: true,
            ..Self::default()
        }
    }
    pub fn reset(&mut self, project_id: u64) {
        if self.project_id != Some(project_id) {
            self.entries.clear();
            self.disk = crate::search_index::DiskIndex::default();
            self.disk_hits = 0;
            self.index_rejections = 0;
            self.source_fetches = 0;
            self.bytes = 0;
            self.hits = 0;
            self.project_id = Some(project_id);
        }
    }
    fn get(&mut self, key: &SearchTarget) -> Option<CachedValue> {
        let entry = self.entries.get(key).cloned();
        if entry.is_some() {
            self.hits += 1;
        }
        entry
    }
    fn remove(&mut self, key: &SearchTarget) {
        if let Some(value) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(cached_cost(key, &value));
        }
    }
    fn insert(&mut self, key: SearchTarget, value: CachedValue) {
        if !self.legacy {
            self.disk.insert(key.clone(), &value);
        }
        let cost = cached_cost(&key, &value);
        if self.entries.len() < 10_000
            && self.bytes.saturating_add(cost) <= self.budget
            && !self.entries.contains_key(&key)
        {
            self.bytes += cost;
            self.entries.insert(key, value);
        }
    }

    fn contains(&self, key: &SearchTarget) -> bool {
        self.entries.contains_key(key) || self.disk.contains(key)
    }

    fn prefetch_sources(
        &mut self,
        engine: &mut NativeEngine,
        names: &[String],
    ) -> anyhow::Result<HashMap<String, Result<CachedValue, String>>> {
        let batch = engine.decompile_sources(names)?;
        self.source_fetches += batch.items.len();
        let mut ready = HashMap::new();
        for item in batch.items {
            let name = item.class;
            let result = match item.code {
                Some(code) => {
                    let key = SearchTarget::Class(name.clone());
                    let document = Arc::new(SearchDocument {
                        target: key.clone(),
                        name: name.clone(),
                        source: code.source,
                        syntax: "java".into(),
                        links: Vec::new(),
                        source_hash: Some(code.source_hash),
                        metadata_complete: false,
                    });
                    let value = Some((document, Arc::new(Vec::new()), None));
                    // Preserve completed work even if a query is cancelled before
                    // every prefetched document is scanned.
                    self.insert(key, value.clone());
                    Ok(value)
                }
                None => Err(format!(
                    "{name}: {}",
                    item.error.unwrap_or_else(|| "Missing batch source".into())
                )),
            };
            ready.insert(name, result);
        }
        // Deferred (oversized) documents retain the ordinary single-source path.
        Ok(ready)
    }
}

fn cached_cost(key: &SearchTarget, value: &CachedValue) -> usize {
    128 + match &key {
        SearchTarget::Class(name) => name.len(),
        _ => 0,
    } + value.as_ref().map_or(0, |(doc, defs, note)| {
        doc.source.len()
            + doc.name.len()
            + doc.syntax.len()
            + doc.source_hash.as_ref().map_or(0, String::len)
            + doc
                .links
                .iter()
                .map(|l| std::mem::size_of::<CodeLink>() + l.label.len())
                .sum::<usize>()
            + defs
                .iter()
                .map(|d| std::mem::size_of::<CodeDefinition>() + d.kind.len() + d.name.len())
                .sum::<usize>()
            + note.as_ref().map_or(0, String::len)
    })
}

type LoadedDocument = Result<Option<(SearchDocument, Vec<CodeDefinition>, Option<String>)>, String>;

// Retry page overflows one owner at a time; only genuinely oversized owners fall back.
fn definition_candidates(
    names: &[String],
    query: &CompiledQuery,
    cancel: &AtomicBool,
    mut lookup: impl FnMut(&[String]) -> anyhow::Result<DefinitionNameIndex>,
) -> anyhow::Result<HashSet<String>> {
    let mut pending = vec![names.to_vec()];
    let mut candidates = HashSet::new();
    while let Some(page) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let index = lookup(&page)?;
        if page.len() > 1 {
            pending.extend(index.unindexed_classes.into_iter().map(|name| vec![name]));
        } else {
            candidates.extend(index.unindexed_classes);
        }
        candidates.extend(
            index
                .entries
                .into_iter()
                .filter(|definition| {
                    let scope = match definition.kind.as_str() {
                        "class" => query.query.classes,
                        "method" => query.query.methods,
                        "field" => query.query.fields,
                        _ => false,
                    };
                    scope && query.matcher.is_match(&definition.name)
                })
                .map(|definition| definition.class),
        );
    }
    Ok(candidates)
}

// Keep the engine, cache identity, input scope and cancellation explicit at this boundary.
#[allow(clippy::too_many_arguments)]
pub fn run_search(
    engine: &mut NativeEngine,
    cache: &mut SearchCache,
    project_id: u64,
    classes: &[String],
    archive: Option<&Archive>,
    query: &CompiledQuery,
    cancel: &AtomicBool,
    emit: impl FnMut(SearchUpdate),
) -> SearchSummary {
    run_search_interactive(
        engine,
        cache,
        project_id,
        classes,
        archive,
        query,
        cancel,
        emit,
        |_| {},
    )
}

/// Service foreground requests between native engine operations.
#[allow(clippy::too_many_arguments)]
pub fn run_search_interactive(
    engine: &mut NativeEngine,
    cache: &mut SearchCache,
    project_id: u64,
    classes: &[String],
    archive: Option<&Archive>,
    query: &CompiledQuery,
    cancel: &AtomicBool,
    mut emit: impl FnMut(SearchUpdate),
    mut interactive: impl FnMut(&mut NativeEngine),
) -> SearchSummary {
    cache.reset(project_id);
    let q = &query.query;
    let mut selected = if q.classes || q.methods || q.fields || q.code || q.comments {
        query.ordered_classes(classes)
    } else {
        Vec::new()
    };
    if cancel.load(Ordering::Relaxed) {
        return SearchSummary {
            cancelled: true,
            total: selected.len(),
            ..Default::default()
        };
    }
    let mut class_summary = None;
    if !cache.legacy && !q.classes && !q.methods && !q.fields {
        let resource_count = if q.resources {
            archive.map_or(0, |a| a.entries.len())
        } else {
            0
        };
        let mut result = streaming::run(
            engine,
            cache,
            &selected,
            query,
            cancel,
            &mut |update| {
                emit(match update {
                    SearchUpdate::Progress {
                        scanned,
                        total,
                        hits,
                        skipped,
                    } => SearchUpdate::Progress {
                        scanned,
                        total: total + resource_count,
                        hits,
                        skipped,
                    },
                    other => other,
                });
            },
            &mut interactive,
        );
        if !q.resources || result.cancelled || result.limited {
            result.total += resource_count;
            return result;
        }
        selected.clear();
        class_summary = Some(result);
    }
    // Names are cheap metadata: only decompile owners whose definitions can match.
    if !q.code && !q.comments && !selected.is_empty() {
        let mut candidates = HashSet::new();
        for page in selected.chunks(128) {
            interactive(engine);
            if cancel.load(Ordering::Relaxed) {
                return SearchSummary {
                    cancelled: true,
                    total: selected.len(),
                    ..Default::default()
                };
            }
            let names: Vec<String> = page.iter().map(|name| (*name).clone()).collect();
            match definition_candidates(&names, query, cancel, |names| {
                engine.definition_names(names)
            }) {
                Ok(found) => candidates.extend(found),
                // An unavailable index must never suppress possible results.
                Err(_) => {
                    candidates.extend(names);
                }
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return SearchSummary {
                cancelled: true,
                total: selected.len(),
                ..Default::default()
            };
        }
        selected.retain(|name| candidates.contains(*name));
    }
    let entries: Vec<_> = archive
        .into_iter()
        .flat_map(|a| &a.entries)
        .filter(|_| q.resources)
        .collect();
    let mut summary = class_summary.unwrap_or_default();
    summary.total = summary.scanned + selected.len() + entries.len();
    let mut retained = summary.retained_bytes;
    let mut last_progress = std::time::Instant::now();
    let mut prefetched = HashMap::new();
    let mut batches_available = true;
    let definitions_requested = q.classes || q.methods || q.fields;
    for index in 0..selected.len() + entries.len() {
        interactive(engine);
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        let key = if index < selected.len() {
            SearchTarget::Class(selected[index].clone())
        } else {
            SearchTarget::Resource(entries[index - selected.len()].index)
        };
        if !cache.legacy
            && !definitions_requested
            && index < selected.len()
            && batches_available
            && !cache.contains(&key)
            && !prefetched.contains_key(selected[index])
        {
            let names: Vec<String> = selected[index..]
                .iter()
                .take(32)
                .filter(|name| !cache.contains(&SearchTarget::Class((***name).to_owned())))
                .map(|name| (*name).clone())
                .collect();
            match cache.prefetch_sources(engine, &names) {
                Ok(ready) => prefetched.extend(ready),
                // A rejected native batch still has a complete single-
                // source path; do not repeatedly retry an unavailable operation.
                Err(_) => batches_available = false,
            }
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }
        }
        let preloaded = if index < selected.len() {
            prefetched.remove(selected[index])
        } else {
            None
        };
        let rejected = !cache.legacy
            && cache.disk.rejects(
                &key,
                &q.text,
                q.case_sensitive,
                q.regex,
                definitions_requested,
            );
        let mut result = if rejected {
            cache.index_rejections += 1;
            Ok(None)
        } else if let Some(loaded) = preloaded {
            loaded
        } else if let Some(cached) = cache.get(&key) {
            Ok(cached)
        } else if !cache.legacy
            && let Some(cached) = cache.disk.load(&key)
        {
            cache.disk_hits += 1;
            Ok(cached)
        } else {
            let loaded: LoadedDocument = if index < selected.len() {
                let name = selected[index];
                cache.source_fetches += 1;
                if !cache.legacy && !definitions_requested {
                    engine
                        .decompile_source(name)
                        .map(|code| {
                            Some((
                                SearchDocument {
                                    target: SearchTarget::Class(name.clone()),
                                    name: name.clone(),
                                    source: code.source,
                                    syntax: "java".into(),
                                    links: Vec::new(),
                                    source_hash: Some(code.source_hash),
                                    metadata_complete: false,
                                },
                                Vec::new(),
                                None,
                            ))
                        })
                        .map_err(|e| format!("{name}: {e}"))
                } else {
                    (if cache.legacy {
                        engine.decompile_with_metadata(name)
                    } else {
                        engine.decompile_for_search(name)
                    })
                    .map(|code| {
                        Some((
                            SearchDocument {
                                target: SearchTarget::Class(name.clone()),
                                name: name.clone(),
                                source: code.source,
                                syntax: "java".into(),
                                links: code.links,
                                source_hash: Some(code.source_hash),
                                metadata_complete: true,
                            },
                            code.definitions,
                            None,
                        ))
                    })
                    .map_err(|e| format!("{name}: {e}"))
                }
            } else {
                let entry = entries[index - selected.len()];
                let decoded_xml = entry.path == "AndroidManifest.xml"
                    || (entry.path.starts_with("res/") && entry.path.ends_with(".xml"));
                let preview = if decoded_xml {
                    engine
                        .read_resource(&entry.path)
                        .map(|text| Preview::Text {
                            text,
                            syntax: "xml".into(),
                            note: None,
                        })
                        .or_else(|error| {
                            match archive.expect("entry archive").preview(entry.index) {
                                Ok(preview @ Preview::Text { .. }) => Ok(preview),
                                _ => Err(error),
                            }
                        })
                } else if is_binary_path(&entry.path) {
                    Ok(Preview::Binary {
                        text: String::new(),
                        note: String::new(),
                    })
                } else {
                    archive.expect("entry archive").preview(entry.index)
                };
                preview
                    .map(|preview| match preview {
                        Preview::Text { text, syntax, note } => Some((
                            SearchDocument {
                                target: SearchTarget::Resource(entry.index),
                                name: entry.path.clone(),
                                source: text,
                                syntax,
                                links: Vec::new(),
                                source_hash: None,
                                metadata_complete: true,
                            },
                            Vec::new(),
                            note,
                        )),
                        _ => None,
                    })
                    .map_err(|e| format!("{}: {e}", entry.path))
            };
            let loaded = loaded
                .map(|entry| entry.map(|(doc, defs, note)| (Arc::new(doc), Arc::new(defs), note)));
            if let Ok(value) = &loaded {
                cache.insert(key.clone(), value.clone());
            }
            loaded
        };
        if let Ok(Some((document, _, _))) = &result
            && !document.metadata_complete
            && definitions_requested
        {
            let name = document.name.clone();
            result = engine
                .decompile_for_search(&name)
                .map(|code| {
                    Some((
                        Arc::new(SearchDocument {
                            target: SearchTarget::Class(name.clone()),
                            name: name.clone(),
                            source: code.source,
                            syntax: "java".into(),
                            links: code.links,
                            source_hash: Some(code.source_hash),
                            metadata_complete: true,
                        }),
                        Arc::new(code.definitions),
                        None,
                    ))
                })
                .map_err(|e| format!("{name}: {e}"));
            // Match the upgraded source again; never apply old offsets to changed output.
            if let Ok(value) = &result {
                cache.remove(&key);
                cache.disk.remove(&key);
                cache.insert(key.clone(), value.clone());
            }
        }
        summary.scanned += 1;
        match result {
            Ok(Some((document, definitions, note))) => {
                if let Some(note) = note {
                    summary.skipped += 1;
                    record_error(&mut summary, format!("{}: {note}", document.name));
                }
                let cost = document.source.len()
                    + document
                        .links
                        .iter()
                        .map(|link| std::mem::size_of::<CodeLink>() + link.label.len())
                        .sum::<usize>();
                let batch = matches(document, &definitions, query, MAX_HITS - summary.hits);
                if !batch.is_empty() {
                    if retained + cost > MAX_RETAINED {
                        summary.limited = true;
                    } else {
                        retained += cost;
                        summary.hits += batch.len();
                        emit(SearchUpdate::Batch(batch));
                    }
                }
            }
            Ok(None) => {
                if !rejected {
                    summary.skipped += 1;
                }
            }
            Err(error) => {
                summary.skipped += 1;
                record_error(&mut summary, error);
            }
        }
        if last_progress.elapsed() >= std::time::Duration::from_millis(50)
            || summary.scanned == summary.total
            || summary.hits >= MAX_HITS
            || summary.limited
        {
            emit(SearchUpdate::Progress {
                scanned: summary.scanned,
                total: summary.total,
                hits: summary.hits,
                skipped: summary.skipped,
            });
            last_progress = std::time::Instant::now();
        }
        if summary.hits >= MAX_HITS || summary.limited {
            summary.limited = true;
            break;
        }
    }
    summary
}
fn record_error(summary: &mut SearchSummary, error: String) {
    if summary.errors.len() < 8 {
        summary.errors.push(error.chars().take(400).collect());
    }
}
fn is_binary_path(path: &str) -> bool {
    let extension = path
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "dex"
            | "so"
            | "arsc"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "ico"
            | "ttf"
            | "otf"
            | "woff"
            | "woff2"
            | "mp3"
            | "mp4"
            | "ogg"
            | "wav"
            | "zip"
            | "jar"
            | "apk"
            | "pdf"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document(source: &str) -> Arc<SearchDocument> {
        Arc::new(SearchDocument {
            target: SearchTarget::Class("example.Target".into()),
            name: "example.Target".into(),
            source: source.into(),
            syntax: "java".into(),
            links: Vec::new(),
            source_hash: None,
            metadata_complete: true,
        })
    }
    #[test]
    fn unicode_offsets_literal_and_regex() {
        let doc = document("😀 café\nCAFÉ");
        let q = CompiledQuery::new(SearchQuery {
            text: "café".into(),
            ..Default::default()
        })
        .unwrap();
        let hits = matches(doc.clone(), &[], &q, 100);
        assert_eq!(&hits[0].preview[hits[0].preview_match.clone()], "café");
        assert_eq!(&hits[1].preview[hits[1].preview_match.clone()], "CAFÉ");
        assert_eq!(
            hits.iter()
                .map(|h| (h.start, h.end, h.line))
                .collect::<Vec<_>>(),
            vec![(2, 6, 1), (7, 11, 2)]
        );
        let q = CompiledQuery::new(SearchQuery {
            text: "caf.".into(),
            regex: true,
            case_sensitive: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(matches(doc, &[], &q, 100).len(), 1);
        let long = document(&format!("{}café", "x".repeat(2000)));
        let hits = matches(long, &[], &q, 100);
        assert!(hits[0].preview.contains("café"));
        assert!(hits[0].preview.chars().count() <= 241);
        assert!(
            CompiledQuery::new(SearchQuery {
                text: "[".into(),
                regex: true,
                ..Default::default()
            })
            .is_err()
        );
    }
    #[test]
    fn standard_packages_are_optional_and_searched_last() {
        let classes: Vec<String> = [
            "android.app.Activity",
            "sample.Z",
            "androidx.core.Helper",
            "kotlin.Unit",
            "sample.A",
            "kotlinx.coroutines.Job",
            "com.android.internal.Helper",
            "androidish.Main",
            "com.androidapp.Main",
            "kotlinextra.Main",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let query = |exclude, package: &str| {
            CompiledQuery::new(SearchQuery {
                text: "x".into(),
                excluded_packages: if exclude {
                    crate::settings::default_excluded_packages()
                } else {
                    Vec::new()
                },
                package: package.into(),
                ..Default::default()
            })
            .unwrap()
        };
        let app = vec![
            &classes[1],
            &classes[4],
            &classes[7],
            &classes[8],
            &classes[9],
        ];
        assert_eq!(query(true, "").ordered_classes(&classes), app);
        let mut all = app;
        all.extend([
            &classes[0],
            &classes[2],
            &classes[3],
            &classes[5],
            &classes[6],
        ]);
        assert_eq!(query(false, "").ordered_classes(&classes), all);
        assert!(query(true, "androidx").ordered_classes(&classes).is_empty());
        assert_eq!(
            query(false, "androidx").ordered_classes(&classes),
            vec![&classes[2]]
        );
        assert_eq!(
            query(true, "sample").ordered_classes(&classes),
            vec![&classes[1], &classes[4]]
        );
    }

    #[test]
    fn exclusions_can_be_removed_individually_and_use_package_boundaries() {
        let mut q = SearchQuery {
            text: "x".into(),
            ..Default::default()
        };
        q.excluded_packages.retain(|p| p != "java.*");
        q.excluded_packages
            .push(normalize_exclusion(" custom.library ").unwrap());
        let query = CompiledQuery::new(q).unwrap();
        assert!(query.accepts_class("java.lang.String"));
        assert!(!query.accepts_class("javax.crypto.Cipher"));
        assert!(!query.accepts_class("android.app.Activity"));
        assert!(!query.accepts_class("custom.library.Foo"));
        assert!(!query.accepts_class("custom.library.sub.Foo"));
        assert!(query.accepts_class("custom.libraryish.Foo"));
        assert!(normalize_exclusion("*").is_none());
        assert!(normalize_exclusion("android..*").is_none());
        assert_eq!(
            normalize_exclusion(" kotlin.* ").as_deref(),
            Some("kotlin.*")
        );
    }
    #[test]
    fn package_boundary() {
        let q = CompiledQuery::new(SearchQuery {
            text: "x".into(),
            package: "com.app".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(q.accepts_class("com.app.Main"));
        assert!(q.accepts_class("com.app.sub.Main"));
        assert!(!q.accepts_class("com.apple.Main"));
        assert!(!q.accepts_class("com.app"));
    }
    #[test]
    fn comments_respect_java_literals() {
        let source = "String a = \"// hit\"; char q = '\"'; /* hit */\nString b = \"\"\"\n// hit\n\"\"\"; // hit";
        let doc = document(source);
        let q = CompiledQuery::new(SearchQuery {
            text: "hit".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(matches(doc.clone(), &[], &q, 100).len(), 2);
        let q = CompiledQuery::new(SearchQuery {
            text: "hit".into(),
            code: false,
            comments: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(matches(doc, &[], &q, 100).len(), 2);
    }
    #[test]
    fn definitions_and_hit_bound() {
        let doc = document("class Target { Target field; }");
        let definitions = vec![CodeDefinition {
            start: 6,
            end: 12,
            kind: "class".into(),
            name: "example.Target".into(),
        }];
        let q = CompiledQuery::new(SearchQuery {
            text: "Target".into(),
            classes: true,
            ..Default::default()
        })
        .unwrap();
        let hits = matches(doc.clone(), &definitions, &q, 100);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].kind, "class");
        assert_eq!(matches(doc, &definitions, &q, 1).len(), 1);
    }
    #[test]
    fn cache_reuses_no_hit_documents_and_invalidates_project() {
        let mut cache = SearchCache::default();
        cache.reset(1);
        let key = SearchTarget::Class("example.Target".into());
        let doc = document("class Target {}");
        cache.insert(key.clone(), Some((doc.clone(), Arc::new(Vec::new()), None)));
        let query = CompiledQuery::new(SearchQuery {
            text: "absent".into(),
            ..Default::default()
        })
        .unwrap();
        let cached = cache.get(&key).unwrap().unwrap();
        assert!(Arc::ptr_eq(&doc, &cached.0));
        assert!(matches(cached.0, &[], &query, 100).is_empty());
        cache.reset(1);
        assert!(cache.get(&key).is_some());
        assert_eq!(cache.hits, 2);
        cache.remove(&key);
        assert_eq!(cache.bytes, 0);
        cache.reset(2);
        assert!(cache.get(&key).is_none());
        assert_eq!(cache.bytes, 0);
        cache.budget = 1;
        cache.insert(key.clone(), Some((doc, Arc::new(Vec::new()), None)));
        assert!(cache.get(&key).is_none());
        assert_eq!(cache.bytes, 0);
        cache.budget = 128;
        let binary = SearchTarget::Resource(0);
        cache.insert(binary.clone(), None);
        assert!(matches!(cache.get(&binary), Some(None)));
        cache.insert(SearchTarget::Resource(1), None);
        assert_eq!(cache.entries.len(), 1);
    }
    #[test]
    fn definition_page_overflow_retries_individual_owners() {
        use rdx::engine::DefinitionName;
        let names = vec!["small".to_owned(), "huge".to_owned(), "match".to_owned()];
        let query = CompiledQuery::new(SearchQuery {
            text: "wanted".into(),
            methods: true,
            code: false,
            ..Default::default()
        })
        .unwrap();
        let cancel = AtomicBool::new(false);
        let mut calls = Vec::new();
        let candidates = definition_candidates(&names, &query, &cancel, |page| {
            calls.push(page.to_vec());
            Ok(if page.len() > 1 || page[0] == "huge" {
                DefinitionNameIndex {
                    entries: vec![],
                    unindexed_classes: page.to_vec(),
                }
            } else {
                DefinitionNameIndex {
                    entries: vec![DefinitionName {
                        class: page[0].clone(),
                        kind: "method".into(),
                        name: if page[0] == "match" {
                            "wanted"
                        } else {
                            "other"
                        }
                        .into(),
                    }],
                    unindexed_classes: vec![],
                }
            })
        })
        .unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(candidates, HashSet::from(["huge".into(), "match".into()]));
        let calls_before = calls.len();
        let _ = definition_candidates(&names, &query, &cancel, |page| {
            calls.push(page.to_vec());
            cancel.store(true, Ordering::Relaxed);
            Ok(DefinitionNameIndex {
                entries: vec![],
                unindexed_classes: page.to_vec(),
            })
        })
        .unwrap();
        assert_eq!(
            calls.len(),
            calls_before + 1,
            "cancel stops individual retries"
        );
    }
    #[test]
    fn native_search_reuses_code_preserves_links_and_cancels() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
        let mut engine = NativeEngine::start().unwrap();
        let project = engine.open(&path).unwrap();
        let archive = Archive::open(&path).unwrap();
        let mut cache = SearchCache::default();
        let cancel = AtomicBool::new(false);
        let query = CompiledQuery::new(SearchQuery {
            text: "doubleValue".into(),
            ..Default::default()
        })
        .unwrap();
        for round in 0..2 {
            let before = cache.source_fetches;
            let mut hits = Vec::new();
            let result = run_search(
                &mut engine,
                &mut cache,
                1,
                &project.classes,
                Some(&archive),
                &query,
                &cancel,
                |update| {
                    if let SearchUpdate::Batch(batch) = update {
                        hits.extend(batch);
                    }
                },
            );
            assert!(result.errors.is_empty(), "{:?}", result.errors);
            assert_eq!(result.scanned, project.classes.len());
            assert!(hits.len() >= 4);
            assert!(hits.iter().all(|h| h.document.metadata_complete));
            let hit = hits
                .iter()
                .find(|h| h.document.name == "sample.Caller")
                .unwrap();
            assert!(
                engine
                    .navigate(
                        &hit.document.name,
                        hit.start,
                        hit.document.source_hash.as_deref().unwrap()
                    )
                    .is_ok()
            );
            if round == 1 {
                assert_eq!(cache.source_fetches, before);
            }
        }
        cancel.store(true, Ordering::Relaxed);
        let stopped = run_search(
            &mut engine,
            &mut cache,
            1,
            &project.classes,
            None,
            &query,
            &cancel,
            |_| panic!("cancel before work"),
        );
        assert!(stopped.cancelled);
        assert_eq!(stopped.scanned, 0);
        cancel.store(false, Ordering::Relaxed);
        let query = CompiledQuery::new(SearchQuery {
            text: "doubleValue".into(),
            code: false,
            methods: true,
            ..Default::default()
        })
        .unwrap();
        let result = run_search(
            &mut engine,
            &mut cache,
            1,
            &project.classes,
            None,
            &query,
            &cancel,
            |_| {},
        );
        assert_eq!(result.hits, 2);
        assert!(result.errors.is_empty());
    }
}
