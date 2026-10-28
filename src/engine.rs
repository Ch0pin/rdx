mod source_cache;

use crate::native_engine::NativeDexEngine;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
pub struct Project {
    pub classes: Vec<String>,
    pub resources: Vec<String>,
}

/// Offsets are Unicode scalar (Rust `char`) indices, with an exclusive end.
#[derive(Clone, Debug, Deserialize)]
pub struct CodeLink {
    pub start: usize,
    pub end: usize,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CodeDefinition {
    pub start: usize,
    pub end: usize,
    pub kind: String,
    pub name: String,
}

/// A declaration name read from DEX metadata without generating source.
#[derive(Clone, Debug, Deserialize)]
pub struct DefinitionName {
    pub class: String,
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DefinitionNameIndex {
    pub entries: Vec<DefinitionName>,
    /// These classes exceeded the response budget and must remain candidates.
    pub unindexed_classes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DecompiledSource {
    pub source: String,
    pub source_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct SourceBatchItem {
    pub class: String,
    pub code: Option<DecompiledSource>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SourceBatch {
    pub items: Vec<SourceBatchItem>,
    /// Retry individually: these sources did not fit the bounded batch response.
    pub deferred: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DecompiledCode {
    pub source: String,
    pub links: Vec<CodeLink>,
    #[serde(default)]
    pub definitions: Vec<CodeDefinition>,
    pub source_hash: String,
}

/// Immutable source payload shared by the cache, GUI and service consumers.
#[derive(Clone, Debug)]
pub struct SharedCode {
    retention: Option<std::sync::Arc<crate::retained_budget::Reservation>>,
    pub source: std::sync::Arc<String>,
    pub links: std::sync::Arc<Vec<CodeLink>>,
    pub definitions: std::sync::Arc<Vec<CodeDefinition>>,
    pub source_hash: String,
}
impl From<DecompiledCode> for SharedCode {
    fn from(code: DecompiledCode) -> Self {
        Self {
            retention: None,
            source: code.source.into(),
            links: code.links.into(),
            definitions: code.definitions.into(),
            source_hash: code.source_hash,
        }
    }
}
impl From<std::sync::Arc<SharedCode>> for SharedCode {
    fn from(code: std::sync::Arc<SharedCode>) -> Self {
        (*code).clone()
    }
}
impl SharedCode {
    pub fn retained_bytes(&self) -> usize {
        self.source.capacity()
            + self.source_hash.capacity()
            + self.links.capacity() * std::mem::size_of::<CodeLink>()
            + self.definitions.capacity() * std::mem::size_of::<CodeDefinition>()
            + self.links.iter().map(|l| l.label.capacity()).sum::<usize>()
            + self
                .definitions
                .iter()
                .map(|d| d.name.capacity() + d.kind.capacity())
                .sum::<usize>()
            + std::mem::size_of::<Self>()
    }

    pub fn into_owned(self) -> DecompiledCode {
        DecompiledCode {
            source: std::sync::Arc::unwrap_or_clone(self.source),
            links: std::sync::Arc::unwrap_or_clone(self.links),
            definitions: std::sync::Arc::unwrap_or_clone(self.definitions),
            source_hash: self.source_hash,
        }
    }
}

/// Requested representation for one exact method; Auto preserves Java-first recovery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MethodSourceFormat {
    #[default]
    Auto,
    Dex,
}
impl MethodSourceFormat {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "dex" => Ok(Self::Dex),
            _ => anyhow::bail!("Invalid method source format: expected auto or dex"),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dex => "dex",
        }
    }
}

/// One method's representation and whether Java reconstruction was attempted.
#[derive(Clone, Debug)]
pub struct MethodSource {
    pub code: DecompiledCode,
    pub format: &'static str,
    pub reconstruction_attempted: bool,
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NavigationResult {
    pub class: String,
    pub code: DecompiledCode,
    pub position: usize,
}

#[derive(Clone, Debug)]
pub struct SharedNavigationResult {
    pub class: String,
    pub code: std::sync::Arc<SharedCode>,
    pub position: usize,
}
impl SharedNavigationResult {
    fn into_owned(self) -> NavigationResult {
        NavigationResult {
            class: self.class,
            code: (*self.code).clone().into_owned(),
            position: self.position,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageTarget {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub classes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageOccurrence {
    pub start: usize,
    pub end: usize,
    pub enclosing: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ClassUsages {
    pub class: String,
    pub code: Option<DecompiledCode>,
    pub occurrences: Vec<UsageOccurrence>,
    pub limited: bool,
}

pub trait DecompilerEngine: Send {
    fn open(&mut self, path: &Path) -> Result<Project>;
    fn decompile(&mut self, class: &str) -> Result<String>;
    fn read_resource(&mut self, path: &str) -> Result<String>;
}

/// Native-only application boundary. No subprocess, JVM or fallback engine.
#[derive(Clone, Default)]
pub struct NativeEngine {
    native: NativeDexEngine,
    pub(crate) retained_budget: std::sync::Arc<crate::retained_budget::RetainedBudget>,
    source_cache: source_cache::SourceCache,
    input: Option<PathBuf>,
    prepared: Option<std::sync::Arc<crate::package_container::PackageContainer>>,
    classes: std::sync::Arc<Vec<String>>,
    resources: std::sync::Arc<crate::resource_table::ResourceTable>,
    resource_error: Option<String>,
    usage_index: Option<crate::usage_index::UsageIndexHandle>,
    identifier_index: crate::identifier_search::Cache,
    pub(crate) invoke_index: crate::call_graph::QueryCache,
}

/// A deterministic identity for locally generated source, not a security digest.
pub fn source_identity(source: &str) -> String {
    let mut hash = DefaultHasher::new();
    source.hash(&mut hash);
    format!("native-v1-{:016x}-{}", hash.finish(), source.len())
}

impl NativeEngine {
    pub(crate) fn opened_path(&self) -> Option<&Path> {
        self.input.as_deref()
    }

    pub fn open_prepared(
        &mut self,
        container: std::sync::Arc<crate::package_container::PackageContainer>,
    ) -> Result<Project> {
        let path = container.source_path().to_path_buf();
        self.open_internal(&path, Some(container))
    }
    fn open_internal(
        &mut self,
        path: &Path,
        supplied: Option<std::sync::Arc<crate::package_container::PackageContainer>>,
    ) -> Result<Project> {
        self.input = None;
        self.prepared = None;
        self.native = NativeDexEngine::default();
        self.source_cache = Default::default();
        self.retained_budget = Default::default();
        self.identifier_index =
            crate::identifier_search::Cache::with_budget(self.retained_budget.clone());
        self.invoke_index = Default::default();
        self.classes = Default::default();
        self.resources = Default::default();
        self.resource_error = None;
        if let Some(index) = self.usage_index.take() {
            index.cancel_project();
        }
        let path = path.canonicalize()?;
        let prepared = match supplied {
            Some(container) => Some(container),
            None if path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("dex")) =>
            {
                None
            }
            None => Some(crate::package_container::PackageContainer::open(&path)?),
        };
        let project = if let Some(container) = &prepared {
            self.native.open_prepared(container.clone())?
        } else {
            self.native.open(&path)?
        };
        if let Some(container) = &prepared {
            let mut merged = crate::resource_table::ResourceTable::default();
            let mut total_bytes = 0u64;
            for entry in container
                .entries()
                .iter()
                .filter(|entry| entry.is_apk_member && entry.member_path == "resources.arsc")
            {
                let loaded = (|| -> Result<()> {
                    total_bytes = total_bytes
                        .checked_add(entry.size)
                        .context("Resource table size overflow")?;
                    ensure!(
                        total_bytes <= 256 * 1024 * 1024,
                        "Combined resource tables exceed 256 MiB"
                    );
                    let bytes = container.read_member(entry.index, 64 * 1024 * 1024)?;
                    let table = crate::resource_table::ResourceTable::parse(&bytes)?;
                    let prefix = entry.path.strip_suffix("resources.arsc").unwrap_or("");
                    merged.merge_split(table, prefix)?;
                    Ok(())
                })();
                if let Err(error) = loaded {
                    let warning =
                        format!("Resource names unavailable for {}: {error:#}", entry.path);
                    if let Some(existing) = &mut self.resource_error {
                        if existing.len() < 4096 {
                            existing.push_str("; ");
                            existing.push_str(&warning);
                        }
                    } else {
                        self.resource_error = Some(warning);
                    }
                    if total_bytes > 256 * 1024 * 1024 {
                        break;
                    }
                }
            }
            self.resources = std::sync::Arc::new(merged);
        }
        self.input = Some(path);
        self.prepared = prepared;
        self.usage_index = Some(crate::usage_index::UsageIndexHandle::start(
            self.native.shared_classes(),
            self.native.shared_nested_children(),
        ));
        self.classes = std::sync::Arc::new(project.classes.clone());
        Ok(project)
    }
    pub fn prepared_container(
        &self,
    ) -> Option<std::sync::Arc<crate::package_container::PackageContainer>> {
        self.prepared.clone()
    }
    pub fn usage_index_handle(&self) -> Option<crate::usage_index::UsageIndexHandle> {
        self.usage_index.clone()
    }
    pub fn usage_index_stats(&self) -> Option<crate::usage_index::UsageIndexStats> {
        self.usage_index.as_ref()?.stats()
    }
    pub fn usage_index_candidates(
        &self,
        target: &str,
        query_cancel: &std::sync::atomic::AtomicBool,
        progress: impl FnMut(usize, usize),
    ) -> Option<Vec<String>> {
        self.usage_index
            .as_ref()?
            .candidates(target, query_cancel, progress)
    }
    pub fn dex_class(&self, name: &str) -> Option<&crate::native_dex::DexClass> {
        self.native.class(name)
    }

    pub fn search_identifiers(
        &self,
        args: &serde_json::Value,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<serde_json::Value> {
        self.identifier_index
            .search(self.native.shared_classes(), args, cancel)
    }

    pub fn hierarchy_name_candidates(&self, short: &str) -> Vec<String> {
        self.native.hierarchy_name_candidates(short)
    }
    pub fn references_supertype(&self, name: &str) -> bool {
        self.native.references_supertype(name)
    }

    pub fn references_type(&self, name: &str) -> bool {
        let descriptor = format!("L{};", name.replace('.', "/"));
        let mut seen = std::collections::HashSet::new();
        self.native.shared_classes().values().any(|class| {
            seen.insert(std::sync::Arc::as_ptr(&class.symbols) as usize)
                && class
                    .symbols
                    .types
                    .iter()
                    .any(|ty| ty.as_ref() == descriptor)
        })
    }

    pub fn direct_subclass_names(&self, name: &str) -> Vec<String> {
        self.native.direct_subclasses(name)
    }
    pub(crate) fn resource_reader(&self) -> Result<crate::package_container::ResourceReader<'_>> {
        Ok(self
            .prepared
            .as_ref()
            .context("Open an APK first")?
            .resource_reader())
    }
    fn resource_bytes(&self, name: &str) -> Result<Vec<u8>> {
        self.prepared
            .as_ref()
            .context("Open an APK first")?
            .read_path(name, crate::native_resources::MAX_XML_BYTES as u64)
    }
    pub fn resource_error(&self) -> Option<&str> {
        self.resource_error.as_deref()
    }
    pub fn resource_table(&self) -> &crate::resource_table::ResourceTable {
        &self.resources
    }
    pub fn read_resource_with_metadata(&mut self, name: &str) -> Result<DecompiledCode> {
        let bytes = self.resource_bytes(name)?;
        if !name.ends_with(".xml") && !bytes.starts_with(&[3, 0, 8, 0]) {
            let source = archive_text(&bytes)?.to_owned();
            return Ok(DecompiledCode {
                source_hash: source_identity(&source),
                source,
                links: vec![],
                definitions: vec![],
            });
        }
        let (source, references) =
            crate::native_resources::decode_with_attribute_resolver(&bytes, &|id| {
                self.resources
                    .entries
                    .get(&id)
                    .filter(|r| r.kind == "attr")
                    .map(|r| r.name.as_str())
            })?;
        let mut code = DecompiledCode {
            source_hash: source_identity(&source),
            source,
            links: vec![],
            definitions: vec![],
        };
        self.resources.decorate_xml(&mut code, &references);
        Ok(code)
    }
    fn resource_document(&mut self, name: &str) -> Result<DecompiledCode> {
        let target = name
            .strip_prefix(crate::resource_table::PREFIX)
            .context("Invalid resource target")?;
        if let Some(path) = target.strip_prefix("file/") {
            ensure!(
                self.resources
                    .entries
                    .values()
                    .any(|r| r.variants.iter().any(|v| v.file.as_deref() == Some(path))),
                "Resource file is not in this APK's table"
            );
            return self.read_resource_with_metadata(path);
        }
        let id = u32::from_str_radix(target.split('/').next().unwrap(), 16)
            .context("Invalid resource ID")?;
        if target
            .split_once('/')
            .is_some_and(|(_, suffix)| suffix == "unresolved")
            && !self.resources.entries.contains_key(&id)
        {
            let source = format!(
                "Resource ID {id} / 0x{id:08x}\n\nNo entry exists in the loaded APK's resource table.\nThe original numeric ID is preserved; its resource name and value are unavailable.\n"
            );
            return Ok(DecompiledCode {
                source_hash: source_identity(&source),
                source,
                links: vec![],
                definitions: vec![],
            });
        }
        self.resources.document(id)
    }
    pub fn start() -> Result<Self> {
        Ok(Self::default())
    }
    pub fn definition_names(&mut self, classes: &[String]) -> Result<DefinitionNameIndex> {
        self.native.definition_names(classes)
    }
    pub fn decompile_with_metadata(&mut self, class: &str) -> Result<DecompiledCode> {
        Ok((*self.decompile_shared(class)?).clone().into_owned())
    }
    pub fn trim_source_cache(&mut self, bytes: usize) {
        self.source_cache.trim(bytes);
    }
    pub fn source_cache_bytes(&self) -> usize {
        self.source_cache.bytes()
    }
    pub fn decompile_shared(&mut self, class: &str) -> Result<std::sync::Arc<SharedCode>> {
        if let Some(code) = self.source_cache.get(class) {
            return Ok(code);
        }
        let mut code = SharedCode::from(self.render_with_metadata(class)?);
        code.retention = self.retained_budget.reserve(code.retained_bytes());
        if code.retention.is_none() {
            self.source_cache.trim(0);
            code.retention = self.retained_budget.reserve(code.retained_bytes());
        }
        let code = std::sync::Arc::new(code);
        if code.retention.is_some() {
            self.source_cache.insert(class, code.clone());
        }
        Ok(code)
    }
    fn render_with_metadata(&mut self, class: &str) -> Result<DecompiledCode> {
        if class.starts_with(crate::resource_table::PREFIX) {
            return self.resource_document(class);
        }
        if let Some(owner) = class.strip_prefix("dex://") {
            let metadata = self.native.class(owner).context("Unknown DEX class")?;
            let mut code = crate::native_engine::disassembly::render_call_sites(owner, metadata);
            code.source_hash = source_identity(&code.source);
            return Ok(code);
        }
        let mut code = self.native.render(class)?;
        self.resources
            .decorate_java(&mut code, &self.native.shared_classes());
        code.source_hash = source_identity(&code.source);
        Ok(code)
    }
    pub fn search_work_bytes(&self, class: &str) -> usize {
        self.native.search_work_bytes(class)
    }
    pub fn decompile_for_search(&mut self, class: &str) -> Result<DecompiledCode> {
        self.render_with_metadata(class)
    }
    pub fn decompile_search_cancellable(
        &mut self,
        class: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<DecompiledCode> {
        self.decompile_search_until(class, &|| cancel.load(std::sync::atomic::Ordering::Relaxed))
    }
    /// Search workers combine user cancellation with local pool shutdown.
    /// Interactive source caches remain untouched by bulk rendering.
    pub fn decompile_search_until(
        &mut self,
        class: &str,
        cancelled: &impl Fn() -> bool,
    ) -> Result<DecompiledCode> {
        let mut code = self.native.render_cancellable(class, cancelled)?;
        ensure!(!cancelled(), "Search cancelled");
        self.resources
            .decorate_java(&mut code, &self.native.shared_classes());
        Ok(code)
    }
    /// Render a class once with whole-class representation coverage.
    pub fn decompile_class_source_report(
        &mut self,
        class: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(DecompiledCode, serde_json::Value)> {
        let cancelled = || cancel.load(std::sync::atomic::Ordering::Relaxed);
        let (mut code, mut coverage) = self.native.render_reported(class, &cancelled)?;
        ensure!(!cancelled(), "CANCELLED");
        self.resources
            .decorate_java(&mut code, &self.native.shared_classes());
        crate::source_coverage::refresh_offsets(&mut coverage, &code);
        code.source_hash = source_identity(&code.source);
        Ok((code, coverage))
    }
    /// Render exactly one method using the existing Java-first representation.
    pub fn decompile_method_source(
        &mut self,
        id: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(DecompiledCode, Option<String>)> {
        let report = self.method_source(id, MethodSourceFormat::Auto, cancel)?;
        Ok((report.code, report.fallback_reason))
    }
    /// Render one exact DEX method ID; Dex bypasses Java reconstruction entirely.
    /// Offsets are method-local characters. Neither mode populates class caches.
    pub fn method_source(
        &mut self,
        id: &str,
        format: MethodSourceFormat,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<MethodSource> {
        ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "CANCELLED"
        );
        let normalized = crate::method_lookup::normalize(id)?;
        let id = normalized.as_str();
        let owner = crate::method_lookup::owner(id)?;
        let class = self
            .native
            .class(owner)
            .with_context(|| format!("SYMBOL_NOT_FOUND: class_id={owner}"))?;
        let method = crate::method_lookup::resolve(class, id)?;
        let (mut code, fallback) = if format == MethodSourceFormat::Dex {
            (
                crate::native_engine::disassembly::render_method(method, &class.symbols),
                None,
            )
        } else {
            match crate::native_java::render_method(owner, class, method) {
                Ok(code) => (
                    crate::native_java::present_method_source_types(class, code),
                    None,
                ),
                Err(error) => (
                    crate::native_engine::disassembly::render_method(method, &class.symbols),
                    Some(error.to_string()),
                ),
            }
        };
        if let Some(reason) = &fallback {
            crate::source_coverage::prepend_method_fallback_header(&mut code, id, reason);
        }
        ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "CANCELLED"
        );
        if format == MethodSourceFormat::Auto {
            self.resources
                .decorate_java(&mut code, &self.native.shared_classes());
        }
        code.source_hash = source_identity(&code.source);
        ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "CANCELLED"
        );
        Ok(MethodSource {
            code,
            format: if format == MethodSourceFormat::Dex || fallback.is_some() {
                "rdx-dex"
            } else {
                "java"
            },
            reconstruction_attempted: format == MethodSourceFormat::Auto,
            fallback_reason: fallback,
        })
    }
    pub fn decompile_source(&mut self, class: &str) -> Result<DecompiledSource> {
        let code = self.decompile_with_metadata(class)?;
        Ok(DecompiledSource {
            source: code.source,
            source_hash: code.source_hash,
        })
    }
    pub fn decompile_sources(&mut self, classes: &[String]) -> Result<SourceBatch> {
        Ok(SourceBatch {
            items: classes
                .iter()
                .map(|class| match self.decompile_source(class) {
                    Ok(code) => SourceBatchItem {
                        class: class.clone(),
                        code: Some(code),
                        error: None,
                    },
                    Err(error) => SourceBatchItem {
                        class: class.clone(),
                        code: None,
                        error: Some(error.to_string()),
                    },
                })
                .collect(),
            deferred: Vec::new(),
        })
    }
    pub fn navigate_class(&mut self, class: &str) -> Result<NavigationResult> {
        self.navigate_class_shared(class)
            .map(SharedNavigationResult::into_owned)
    }
    pub fn navigate_class_shared(&mut self, class: &str) -> Result<SharedNavigationResult> {
        let code = self.decompile_shared(class)?;
        let position = code
            .definitions
            .iter()
            .find(|d| d.kind == "class")
            .map_or(0, |d| d.start);
        Ok(SharedNavigationResult {
            class: class.into(),
            code,
            position,
        })
    }
    fn definition_symbol(
        &self,
        class: &str,
        source: &str,
        links: &[CodeLink],
        def: &CodeDefinition,
    ) -> Option<String> {
        let class = class.strip_prefix("dex://").unwrap_or(class);
        if def.kind == "method" {
            let declaration_byte = source
                .char_indices()
                .nth(def.start)
                .map_or(source.len(), |(byte, _)| byte);
            let declaration_prefix = source[..declaration_byte].rsplit('\n').next().unwrap_or("");
            if declaration_prefix.trim_start() == ".method " {
                return Some(
                    source
                        .chars()
                        .skip(def.start)
                        .take(def.end - def.start)
                        .collect(),
                );
            }
        }
        if def.kind == "class" {
            return Some(class.into());
        }
        if let Some(link) = links
            .iter()
            .find(|link| link.start == def.start && link.end == def.end)
        {
            return Some(link.label.clone());
        }
        let span: String = source
            .chars()
            .skip(def.start)
            .take(def.end - def.start)
            .collect();
        if span.starts_with(&format!("{class}.")) {
            return Some(span);
        }
        let metadata = self.native.class(class)?;
        if def.kind == "method" {
            let mut methods = metadata
                .methods
                .iter()
                .filter(|m| m.name.as_ref() == def.name);
            let method = methods.next()?;
            if methods.next().is_some() {
                return None;
            }
            return Some(format!(
                "{class}.{}({}){}",
                method.name,
                method.parameters.join(""),
                method.return_type
            ));
        }
        metadata
            .fields
            .iter()
            .find(|f| f.name.as_ref() == def.name)
            .map(|f| format!("{class}.{}:{}", f.name, f.field_type))
    }
    fn symbol(&mut self, class: &str, offset: usize, hash: &str) -> Result<String> {
        let code = self.decompile_shared(class)?;
        ensure!(
            code.source_hash == hash,
            "Source changed; reopen this class"
        );
        code.links
            .iter()
            .find(|l| l.start <= offset && offset < l.end)
            .map(|l| l.label.clone())
            .or_else(|| {
                code.definitions
                    .iter()
                    .find(|d| d.start <= offset && offset < d.end)
                    .and_then(|d| self.definition_symbol(class, &code.source, &code.links, d))
            })
            .context("No native symbol at this position")
    }
    fn owner(&self, symbol: &str) -> Option<String> {
        // Probe exact class names from longest to shortest rather than scanning
        // every class in the APK on each click. Descriptors may also contain dots.
        let mut candidate = symbol;
        loop {
            if self.native.class(candidate).is_some() {
                return Some(candidate.to_owned());
            }
            candidate = candidate.rsplit_once('.')?.0;
        }
    }
    pub fn navigate(&mut self, class: &str, offset: usize, hash: &str) -> Result<NavigationResult> {
        self.navigate_shared(class, offset, hash)
            .map(SharedNavigationResult::into_owned)
    }
    pub fn navigate_shared(
        &mut self,
        class: &str,
        offset: usize,
        hash: &str,
    ) -> Result<SharedNavigationResult> {
        let symbol = self.symbol(class, offset, hash)?;
        if symbol.starts_with(crate::resource_table::PREFIX) {
            let target = symbol.split(" | ").next().unwrap().to_owned();
            let code = std::sync::Arc::new(SharedCode::from(self.resource_document(&target)?));
            return Ok(SharedNavigationResult {
                class: target,
                code,
                position: 0,
            });
        }
        let mut owner = self
            .owner(&symbol)
            .context("Symbol is external to this project")?;
        let mut declaration_symbol = symbol.clone();
        if let Some(member) = symbol.strip_prefix(&format!("{owner}."))
            && member.contains('(')
            && let Some(declaring_owner) =
                inherited_method_owner(&owner, member, |name| self.native.class(name))?
        {
            declaration_symbol = format!("{declaring_owner}.{member}");
            owner = declaring_owner;
        }
        let code = self.decompile_shared(&owner)?;
        let position = code
            .definitions
            .iter()
            .find(|d| {
                self.definition_symbol(&owner, &code.source, &code.links, d)
                    .as_deref()
                    == Some(&declaration_symbol)
            })
            .context("Native declaration mapping unavailable")?
            .start;
        Ok(SharedNavigationResult {
            class: owner,
            code,
            position,
        })
    }
    pub fn resolve_usage_target(
        &mut self,
        class: &str,
        offset: usize,
        hash: &str,
    ) -> Result<UsageTarget> {
        let symbol = self.symbol(class, offset, hash)?;
        let kind = if symbol.contains('(') {
            "method"
        } else if symbol.contains(':') {
            "field"
        } else {
            "class"
        };
        Ok(UsageTarget {
            id: symbol.clone(),
            label: symbol,
            kind: kind.into(),
            classes: self.classes.as_ref().clone(),
        })
    }
    pub fn usages_in_class(&mut self, target: &str, class: &str) -> Result<ClassUsages> {
        let code = self.decompile_with_metadata(class)?;
        let occurrences: Vec<_> = code
            .links
            .iter()
            .filter(|l| l.label == target)
            .filter(|l| {
                !code
                    .definitions
                    .iter()
                    .any(|d| d.start == l.start && d.end == l.end)
            })
            .map(|link| {
                let enclosing = code
                    .definitions
                    .iter()
                    .filter(|d| d.kind == "method" && d.start <= link.start)
                    .max_by_key(|d| d.start)
                    .map_or_else(|| class.into(), |d| d.name.clone());
                UsageOccurrence {
                    start: link.start,
                    end: link.end,
                    enclosing,
                }
            })
            .collect();
        Ok(ClassUsages {
            class: class.into(),
            code: (!occurrences.is_empty()).then_some(code),
            occurrences,
            limited: false,
        })
    }
    pub fn resolve_method_xref_target(
        &mut self,
        class: &str,
        offset: usize,
        hash: &str,
        callers: bool,
    ) -> Result<UsageTarget> {
        let mut target = self.resolve_usage_target(class, offset, hash)?;
        ensure!(
            target.kind == "method",
            "Select a method declaration or call"
        );
        if !callers {
            let owner = self
                .owner(&target.id)
                .context("Callee body is external to this project")?;
            let member = target
                .id
                .strip_prefix(&format!("{owner}."))
                .context("Invalid method")?;
            let owner = inherited_method_owner(&owner, member, |name| self.native.class(name))?
                .unwrap_or(owner);
            target.id = format!("{owner}.{member}");
            target.label = target.id.clone();
            target.classes = vec![owner];
        }
        Ok(target)
    }

    pub fn call_graph_by_id(
        &self,
        method: &str,
        depth: usize,
        direction: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<crate::call_graph::CallGraph> {
        let method = crate::method_lookup::graph_selector(method, |name| self.dex_class(name))?;
        crate::call_graph::build_direction(&self.native, &method, depth, direction, cancel)
    }

    /// Query DEX call sites and filtered graphs without Java reconstruction.
    pub fn call_graph_query_by_id(
        &self,
        method: &str,
        args: &serde_json::Value,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<serde_json::Value> {
        crate::call_graph::query_cached(&self.native, method, args, cancel, &self.invoke_index)
    }

    pub fn call_graph_at(
        &mut self,
        class: &str,
        offset: usize,
        hash: &str,
        depth: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<crate::call_graph::CallGraph> {
        let target = self.resolve_method_xref_target(class, offset, hash, false)?;
        crate::call_graph::build(&self.native, &target.id, depth, cancel)
    }

    pub fn navigate_graph_method(&mut self, symbol: &str) -> Result<NavigationResult> {
        self.navigate_graph_method_shared(symbol)
            .map(SharedNavigationResult::into_owned)
    }
    pub fn navigate_graph_method_shared(&mut self, symbol: &str) -> Result<SharedNavigationResult> {
        let owner = self
            .owner(symbol)
            .context("Method is external to this project")?;
        let member = symbol
            .strip_prefix(&format!("{owner}."))
            .context("Invalid method")?;
        let owner = inherited_method_owner(&owner, member, |name| self.native.class(name))?
            .unwrap_or(owner);
        let declaration = format!("{owner}.{member}");
        let code = self.decompile_shared(&owner)?;
        let position = code
            .definitions
            .iter()
            .find(|d| {
                self.definition_symbol(&owner, &code.source, &code.links, d)
                    .as_deref()
                    == Some(&declaration)
            })
            .context("Method declaration unavailable")?
            .start;
        Ok(SharedNavigationResult {
            class: owner,
            code,
            position,
        })
    }

    pub fn method_xrefs_in_class(
        &mut self,
        target: &str,
        class: &str,
        callers: bool,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ClassUsages> {
        let metadata = self.native.class(class).context("Unknown call owner")?;
        let sites =
            crate::native_xrefs::call_sites(metadata, (!callers).then_some(target), cancel)?;
        let sites: Vec<_> = sites
            .into_iter()
            .filter(|site| !callers || site.callee == target)
            .collect();
        let document = format!("dex://{class}");
        if sites.is_empty() {
            return Ok(ClassUsages {
                class: document,
                code: None,
                occurrences: vec![],
                limited: false,
            });
        }
        // Prefer Java only when every selected instruction has a unique source
        // reference within its caller. Repeated/rewritten calls retain exact DEX.
        if let Ok(code) = self.decompile_search_cancellable(class, cancel) {
            let mut definitions: Vec<_> = code
                .definitions
                .iter()
                .filter(|d| d.kind == "method")
                .filter_map(|d| {
                    self.definition_symbol(class, &code.source, &code.links, d)
                        .map(|id| (d.start, d.end, id))
                })
                .collect();
            definitions.sort_by_key(|d| d.0);
            let mut occurrences = Vec::new();
            let mut used = std::collections::HashSet::new();
            for site in &sites {
                let Some(index) = definitions.iter().position(|d| d.2 == site.caller) else {
                    break;
                };
                let start = definitions[index].1;
                let end = definitions.get(index + 1).map_or(usize::MAX, |d| d.0);
                let mut matches = code.links.iter().filter(|link| {
                    link.start >= start && link.start < end && link.label == site.callee
                });
                let Some(link) = matches.next() else { break };
                if matches.next().is_some() || !used.insert(link.start) {
                    break;
                }
                occurrences.push(UsageOccurrence {
                    start: link.start,
                    end: link.end,
                    enclosing: format!(
                        "{} → {} [{}; DEX @{:04x}]",
                        site.caller, site.callee, site.dispatch, site.pc
                    ),
                });
            }
            if occurrences.len() == sites.len() {
                let limited = occurrences.len() > 1000;
                occurrences.truncate(1000);
                return Ok(ClassUsages {
                    class: class.into(),
                    code: Some(code),
                    occurrences,
                    limited,
                });
            }
        }
        // DEX call-site views provide exact instruction locations even when Java
        // emission inlines calls, rewrites constructors, or falls back.
        let code = self.decompile_with_metadata(&document)?;
        let mut locations = std::collections::HashMap::new();
        let mut caller = "";
        let mut offset = 0;
        for line in code.source.split_inclusive('\n') {
            if let Some(header) = line.strip_prefix(".method ") {
                caller = header.split(" //").next().unwrap().trim();
            } else if let Some((address, _)) = line.trim_start().split_once(':')
                && let Ok(pc) = usize::from_str_radix(address, 16)
            {
                locations.insert(
                    (caller.to_owned(), pc),
                    (offset, offset + line.chars().count()),
                );
            }
            offset += line.chars().count();
        }
        let limited = sites.len() > 1000;
        let mut occurrences = Vec::new();
        for site in sites.into_iter().take(1000) {
            let &(start, end) = locations
                .get(&(site.caller.clone(), site.pc))
                .context("Call instruction mapping unavailable")?;
            let link_index = code.links.partition_point(|link| link.start < start);
            let link = code.links[link_index..]
                .iter()
                .take_while(|link| link.start < end)
                .find(|link| link.label == site.callee)
                .context("Call target mapping unavailable")?;
            occurrences.push(UsageOccurrence {
                start: link.start,
                end: link.end,
                enclosing: format!(
                    "{} → {} [{}; DEX @{:04x}]",
                    site.caller, site.callee, site.dispatch, site.pc
                ),
            });
        }
        Ok(ClassUsages {
            class: document,
            code: Some(code),
            occurrences,
            limited,
        })
    }

    pub fn direct_subclasses_at(
        &mut self,
        class: &str,
        offset: usize,
        hash: &str,
    ) -> Result<(String, Vec<String>)> {
        let symbol = self.symbol(class, offset, hash)?;
        ensure!(
            !symbol.contains(['(', ':']),
            "Select a class, not a method or field"
        );
        let children = self.native.direct_subclasses(&symbol);
        Ok((symbol, children))
    }
    pub fn class_declaration_result(&mut self, class: &str) -> Result<ClassUsages> {
        let code = self.decompile_with_metadata(class)?;
        let declaration = code
            .definitions
            .iter()
            .find(|d| d.kind == "class")
            .context("Class declaration is unavailable")?;
        let occurrence = UsageOccurrence {
            start: declaration.start,
            end: declaration.end,
            enclosing: class.into(),
        };
        Ok(ClassUsages {
            class: class.into(),
            code: Some(code),
            occurrences: vec![occurrence],
            limited: false,
        })
    }
    pub fn implementation_names(
        &self,
        class: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<String>> {
        ensure!(
            self.native.class(class).is_some() || self.references_type(class),
            "SYMBOL_NOT_FOUND: no loaded definition or app-declared superclass/interface edge; framework definitions may be absent from the APK; use an exact referenced FQCN or DEX descriptor"
        );
        self.native.implementations(class, None, cancel)
    }

    pub fn implementations_at(
        &mut self,
        class: &str,
        offset: usize,
        hash: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(String, Vec<String>)> {
        let symbol = self.symbol(class, offset, hash)?;
        ensure!(
            !symbol.contains(':'),
            "Select a class or method, not a field"
        );
        let (owner, member) = if let Some((head, tail)) = symbol.split_once('(') {
            let (owner, name) = head.rsplit_once('.').context("Invalid method symbol")?;
            let member = format!("{name}({tail}");
            let owner = inherited_method_owner(owner, &member, |name| self.native.class(name))?
                .unwrap_or_else(|| owner.into());
            (owner, Some(member))
        } else {
            (symbol.clone(), None)
        };
        let results = self
            .native
            .implementations(&owner, member.as_deref(), cancel)?;
        Ok((symbol, results))
    }
    pub fn implementation_result(&mut self, symbol: &str) -> Result<ClassUsages> {
        if !symbol.contains('(') {
            return self.class_declaration_result(symbol);
        }
        let owner = self
            .owner(symbol)
            .context("Implementation class unavailable")?;
        let code = self.decompile_with_metadata(&owner)?;
        let declaration = code
            .definitions
            .iter()
            .find(|d| {
                d.kind == "method"
                    && self
                        .definition_symbol(&owner, &code.source, &code.links, d)
                        .as_deref()
                        == Some(symbol)
            })
            .context("Implementation declaration unavailable")?;
        let occurrence = UsageOccurrence {
            start: declaration.start,
            end: declaration.end,
            enclosing: symbol.into(),
        };
        Ok(ClassUsages {
            class: owner,
            code: Some(code),
            occurrences: vec![occurrence],
            limited: false,
        })
    }
}
impl DecompilerEngine for NativeEngine {
    fn open(&mut self, path: &Path) -> Result<Project> {
        self.open_internal(path, None)
    }
    fn decompile(&mut self, class: &str) -> Result<String> {
        Ok(self.decompile_with_metadata(class)?.source)
    }
    fn read_resource(&mut self, name: &str) -> Result<String> {
        Ok(crate::native_resources::decode_with_attribute_resolver(
            &self.resource_bytes(name)?,
            &|id| {
                self.resources
                    .entries
                    .get(&id)
                    .filter(|r| r.kind == "attr")
                    .map(|r| r.name.as_str())
            },
        )?
        .0)
    }
}

/// Resolve declaration ownership without changing the call-site symbol identity.
/// Class declarations take precedence; interface candidates must have a unique
/// most-specific owner. Missing external declarations are never fabricated.
fn inherited_method_owner<'a>(
    owner: &str,
    member: &str,
    lookup: impl Fn(&str) -> Option<&'a crate::native_dex::DexClass>,
) -> Result<Option<String>> {
    use std::collections::{HashMap, HashSet, VecDeque};
    let Some((name, descriptor)) = member.split_once('(') else {
        return Ok(None);
    };
    if name.starts_with('<') {
        return Ok(None);
    }
    let Some((parameters, result)) = descriptor.split_once(')') else {
        return Ok(None);
    };
    let class_name = |descriptor: &str| {
        descriptor
            .strip_prefix('L')
            .and_then(|s| s.strip_suffix(';'))
            .map(|s| s.replace('/', "."))
    };
    let matches = |method: &crate::native_dex::DexMethod| {
        method.name.as_ref() == name
            && method.return_type.as_ref() == result
            && method.parameters.join("") == parameters
    };
    let mut seen = HashSet::new();
    let mut interfaces = VecDeque::new();
    let mut current = owner.to_owned();
    let mut work = 0usize;
    loop {
        ensure!(
            seen.len() < 131_072 && seen.insert(current.clone()),
            "Invalid or excessive superclass chain"
        );
        let Some(class) = lookup(&current) else {
            break;
        };
        work = work.saturating_add(class.methods.len());
        ensure!(
            work <= 4_000_000,
            "Inherited declaration lookup exceeds work limit"
        );
        if class
            .methods
            .iter()
            .any(|method| matches(method) && (current == owner || method.access_flags & 2 == 0))
        {
            return Ok(Some(current));
        }
        interfaces.extend(class.interfaces.iter().filter_map(|s| class_name(s)));
        if class.access_flags & 0x200 != 0 {
            break;
        }
        let Some(parent) = class.superclass.as_deref().and_then(class_name) else {
            break;
        };
        current = parent;
    }
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut candidates = Vec::new();
    while let Some(current) = interfaces.pop_front() {
        if graph.contains_key(&current) {
            continue;
        }
        ensure!(
            graph.len() < 131_072,
            "Inherited interface lookup exceeds node limit"
        );
        let Some(class) = lookup(&current) else {
            graph.insert(current, vec![]);
            continue;
        };
        work = work.saturating_add(class.methods.len() + class.interfaces.len());
        ensure!(
            work <= 4_000_000,
            "Inherited declaration lookup exceeds work limit"
        );
        if class
            .methods
            .iter()
            .any(|method| matches(method) && method.access_flags & (2 | 8) == 0)
        {
            candidates.push(current.clone());
            ensure!(
                candidates.len() <= 256,
                "Too many inherited declaration candidates"
            );
        }
        let parents: Vec<_> = class
            .interfaces
            .iter()
            .filter_map(|s| class_name(s))
            .collect();
        interfaces.extend(parents.iter().cloned());
        graph.insert(current, parents);
    }
    if candidates.len() <= 1 {
        return Ok(candidates.pop());
    }
    let mut best = Vec::new();
    for candidate in &candidates {
        let mut ancestors = HashSet::new();
        let mut pending = vec![candidate.as_str()];
        while let Some(node) = pending.pop() {
            if !ancestors.insert(node) {
                continue;
            }
            if let Some(parents) = graph.get(node) {
                work = work.saturating_add(parents.len() + 1);
                ensure!(
                    work <= 4_000_000,
                    "Inherited declaration lookup exceeds work limit"
                );
                pending.extend(parents.iter().map(String::as_str));
            }
        }
        if candidates
            .iter()
            .all(|other| ancestors.contains(other.as_str()))
        {
            best.push(candidate.clone());
        }
    }
    ensure!(best.len() == 1, "Ambiguous inherited method declaration");
    Ok(best.pop())
}

#[cfg(test)]
mod inherited_navigation_tests {
    use super::*;
    use crate::native_dex::DexClass;
    use std::collections::BTreeMap;
    fn class(
        name: &str,
        parent: Option<&str>,
        interfaces: &[&str],
        method: Option<(&str, &str)>,
        interface: bool,
    ) -> DexClass {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.descriptor = format!("L{name};").into();
        class.superclass = parent.map(|p| format!("L{p};").into());
        class.interfaces = interfaces.iter().map(|p| format!("L{p};").into()).collect();
        class.access_flags = if interface { 0x601 } else { 1 };
        if let Some((name, parameter)) = method {
            class.methods.truncate(1);
            let method = &mut class.methods[0];
            method.declaring_type = class.descriptor.clone();
            method.name = name.into();
            method.return_type = "Z".into();
            method.parameters = if parameter.is_empty() {
                vec![]
            } else {
                vec![parameter.into()]
            };
            method.access_flags = 1;
        } else {
            class.methods.clear();
        }
        class
    }
    fn resolve(classes: &[(&str, DexClass)], member: &str) -> Result<Option<String>> {
        let map: BTreeMap<_, _> = classes.iter().map(|(name, class)| (*name, class)).collect();
        inherited_method_owner("Child", member, |name| map.get(name).copied())
    }
    #[test]
    fn parent_interface_and_superclass_use_exact_signature() {
        for interface in [true, false] {
            let classes = [
                (
                    "Child",
                    class(
                        "Child",
                        (!interface).then_some("Parent"),
                        if interface { &["Parent"] } else { &[] },
                        Some(("e", "I")),
                        interface,
                    ),
                ),
                (
                    "Parent",
                    class("Parent", None, &[], Some(("e", "")), interface),
                ),
            ];
            assert_eq!(
                resolve(&classes, "e()Z").unwrap().as_deref(),
                Some("Parent")
            );
            assert_eq!(
                resolve(&classes, "e(I)Z").unwrap().as_deref(),
                Some("Child")
            );
            assert_eq!(resolve(&classes, "e(J)Z").unwrap(), None);
            assert_eq!(resolve(&classes, "<init>()V").unwrap(), None);
        }
    }
    #[test]
    fn diamonds_choose_most_specific_but_unrelated_declarations_are_ambiguous() {
        let classes = [
            (
                "Child",
                class("Child", None, &["Left", "Right"], None, true),
            ),
            (
                "Left",
                class("Left", None, &["Root"], Some(("e", "")), true),
            ),
            ("Right", class("Right", None, &["Root"], None, true)),
            ("Root", class("Root", None, &[], Some(("e", "")), true)),
        ];
        assert_eq!(resolve(&classes, "e()Z").unwrap().as_deref(), Some("Left"));
        let ambiguous = [
            (
                "Child",
                class("Child", None, &["Left", "Right"], None, true),
            ),
            ("Left", class("Left", None, &[], Some(("e", "")), true)),
            ("Right", class("Right", None, &[], Some(("e", "")), true)),
        ];
        assert!(
            resolve(&ambiguous, "e()Z")
                .unwrap_err()
                .to_string()
                .contains("Ambiguous")
        );
    }
    #[test]
    fn private_static_interface_methods_and_missing_external_types_are_not_inherited() {
        for flags in [2, 9] {
            let mut parent = class("Parent", None, &[], Some(("e", "")), true);
            parent.methods[0].access_flags = flags;
            let classes = [
                (
                    "Child",
                    class("Child", None, &["Parent", "External"], None, true),
                ),
                ("Parent", parent),
            ];
            assert_eq!(resolve(&classes, "e()Z").unwrap(), None);
        }
    }

    #[test]
    fn superclass_wins_over_interface_and_cycles_are_bounded() {
        let classes = [
            (
                "Child",
                class("Child", Some("Parent"), &["Contract"], None, false),
            ),
            ("Parent", class("Parent", None, &[], Some(("e", "")), false)),
            (
                "Contract",
                class("Contract", None, &[], Some(("e", "")), true),
            ),
        ];
        assert_eq!(
            resolve(&classes, "e()Z").unwrap().as_deref(),
            Some("Parent")
        );
        let cyclic = [
            ("Child", class("Child", Some("Parent"), &[], None, false)),
            ("Parent", class("Parent", Some("Child"), &[], None, false)),
        ];
        assert!(
            resolve(&cyclic, "e()Z")
                .unwrap_err()
                .to_string()
                .contains("superclass chain")
        );
    }
}

#[cfg(test)]
mod navigation_cache_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn memory_shared_navigation_uses_the_cached_payload_and_owned_compatibility_is_isolated() {
        let mut engine = NativeEngine::start().unwrap();
        engine
            .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
            .unwrap();
        let shared = engine.decompile_shared("sample.Target").unwrap();
        let hit = engine.decompile_shared("sample.Target").unwrap();
        assert!(std::sync::Arc::ptr_eq(&shared, &hit));
        let navigation = engine.navigate_class_shared("sample.Target").unwrap();
        assert!(std::sync::Arc::ptr_eq(&shared, &navigation.code));
        let mut owned = engine.decompile_with_metadata("sample.Target").unwrap();
        owned.source.clear();
        owned.links.clear();
        assert!(!shared.source.is_empty());
        engine.trim_source_cache(0);
        assert_eq!(engine.source_cache_bytes(), 0);
        assert!(!navigation.code.source.is_empty());
    }
    #[test]
    fn memory_retention_pressure_serves_complete_uncached_source() {
        let mut engine = NativeEngine::start().unwrap();
        engine
            .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
            .unwrap();
        let expected = engine.decompile_shared("sample.Target").unwrap();
        engine.trim_source_cache(0);
        engine.retained_budget =
            std::sync::Arc::new(crate::retained_budget::RetainedBudget::new(0));
        let actual = engine.decompile_shared("sample.Target").unwrap();
        assert_eq!(actual.source, expected.source);
        assert_eq!(actual.source_hash, expected.source_hash);
        assert_eq!(engine.source_cache_bytes(), 0);
    }
    #[test]
    fn raw_method_identity_uses_declaration_line_after_diagnostic_header() {
        let id = "sample.Caller.f()V";
        let source = format!(
            "// RDX Java reconstruction incomplete: 1 DEX fallback method(s).\n// Native DEX disassembly.\n.method {id}\n.end method\n"
        );
        let byte = source.find(id).unwrap();
        let start = source[..byte].chars().count();
        let definition = CodeDefinition {
            start,
            end: start + id.chars().count(),
            kind: "method".into(),
            name: "f".into(),
        };
        let code = DecompiledCode {
            source_hash: source_identity(&source),
            source,
            links: vec![],
            definitions: vec![definition],
        };
        assert_eq!(
            NativeEngine::default()
                .definition_symbol(
                    "sample.Caller",
                    &code.source,
                    &code.links,
                    &code.definitions[0]
                )
                .as_deref(),
            Some(id)
        );
    }

    fn opened() -> NativeEngine {
        let mut engine = NativeEngine::default();
        engine
            .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
            .unwrap();
        engine
    }

    #[test]
    fn navigation_reuses_source_and_destination_without_regenerating_metadata() {
        let mut engine = opened();
        let code = engine.decompile_with_metadata("sample.Caller").unwrap();
        let original = engine.source_cache.get("sample.Caller").unwrap();
        let link = code
            .links
            .iter()
            .find(|link| link.label == "sample.Target.doubleValue(I)I")
            .unwrap();
        let first = engine
            .navigate("sample.Caller", link.start, &code.source_hash)
            .unwrap();
        let destination = engine.source_cache.get("sample.Target").unwrap();
        for _ in 0..3 {
            let repeated = engine
                .navigate("sample.Caller", link.start, &code.source_hash)
                .unwrap();
            assert_eq!(first.position, repeated.position);
            assert_eq!(first.code.source_hash, repeated.code.source_hash);
            assert!(Arc::ptr_eq(
                &original,
                &engine.source_cache.get("sample.Caller").unwrap()
            ));
            assert!(Arc::ptr_eq(
                &destination,
                &engine.source_cache.get("sample.Target").unwrap()
            ));
            let same = engine
                .navigate("sample.Target", first.position, &first.code.source_hash)
                .unwrap();
            assert_eq!(same.position, first.position);
        }
        assert!(
            engine
                .navigate("sample.Caller", link.start, "stale")
                .is_err()
        );
        // Searches cannot replace or evict interactive sources.
        engine.decompile_for_search("sample.Target").unwrap();
        assert!(Arc::ptr_eq(
            &destination,
            &engine.source_cache.get("sample.Target").unwrap()
        ));
        let dex = engine
            .decompile_with_metadata("dex://sample.Caller")
            .unwrap();
        assert_ne!(dex.source_hash, code.source_hash);
        assert!(Arc::ptr_eq(
            &original,
            &engine.source_cache.get("sample.Caller").unwrap()
        ));
    }

    #[test]
    fn method_source_formats_preserve_interactive_class_cache() {
        let mut engine = opened();
        engine.decompile_with_metadata("sample.Target").unwrap();
        let original = engine.source_cache.get("sample.Target").unwrap();
        for format in [MethodSourceFormat::Dex, MethodSourceFormat::Auto] {
            let report = engine
                .method_source(
                    "sample.Target.doubleValue(I)I",
                    format,
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .unwrap();
            assert_eq!(
                report.reconstruction_attempted,
                format == MethodSourceFormat::Auto
            );
            assert!(Arc::ptr_eq(
                &original,
                &engine.source_cache.get("sample.Target").unwrap()
            ));
        }
    }

    #[test]
    fn reopening_detaches_project_cache_even_when_open_fails() {
        let mut engine = opened();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let args = serde_json::json!({"direction":"callers","projection":"call_sites"});
        engine
            .call_graph_query_by_id("sample.Target.doubleValue(I)I", &args, &cancel)
            .unwrap();
        assert_eq!(engine.invoke_index.status()["state"], "ready");
        engine.decompile_with_metadata("sample.Target").unwrap();
        let original = engine.source_cache.get("sample.Target").unwrap();
        let mut clone = engine.clone();
        assert!(Arc::ptr_eq(
            &original,
            &clone.source_cache.get("sample.Target").unwrap()
        ));
        assert!(
            engine
                .open(Path::new("/nonexistent/rdx-cache-test.apk"))
                .is_err()
        );
        assert!(engine.source_cache.get("sample.Target").is_none());
        assert_eq!(engine.invoke_index.status()["state"], "not_built");
        assert_eq!(
            clone
                .call_graph_query_by_id("sample.Target.doubleValue(I)I", &args, &cancel)
                .unwrap()["invoke_index_cached"],
            true
        );
        assert!(engine.decompile_with_metadata("sample.Target").is_err());
        assert!(clone.decompile_with_metadata("sample.Target").is_ok());
        engine
            .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
            .unwrap();
        engine.decompile_with_metadata("sample.Target").unwrap();
        assert!(!Arc::ptr_eq(
            &original,
            &engine.source_cache.get("sample.Target").unwrap()
        ));
    }
}

/// UTF-8 asset text only: do not turn binary data into misleading source.
pub(crate) fn archive_text(bytes: &[u8]) -> Result<&str> {
    let text = std::str::from_utf8(bytes).context("BINARY_RESOURCE: resource is not UTF-8 text")?;
    anyhow::ensure!(
        !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
        "BINARY_RESOURCE: resource contains binary control characters"
    );
    Ok(text)
}
