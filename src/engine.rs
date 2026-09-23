use crate::native_engine::NativeDexEngine;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io::Read,
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

#[derive(Clone, Debug, Deserialize)]
pub struct NavigationResult {
    pub class: String,
    pub code: DecompiledCode,
    pub position: usize,
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
#[derive(Default)]
pub struct NativeEngine {
    native: NativeDexEngine,
    input: Option<PathBuf>,
    classes: Vec<String>,
    resources: crate::resource_table::ResourceTable,
    resource_error: Option<String>,
}

/// A deterministic identity for locally generated source, not a security digest.
pub fn source_identity(source: &str) -> String {
    let mut hash = DefaultHasher::new();
    source.hash(&mut hash);
    format!("native-v1-{:016x}-{}", hash.finish(), source.len())
}

impl NativeEngine {
    pub fn dex_class(&self, name: &str) -> Option<&crate::native_dex::DexClass> {
        self.native.class(name)
    }

    pub fn direct_subclass_names(&self, name: &str) -> Vec<String> {
        self.native.direct_subclasses(name)
    }
    fn resource_bytes(&self, name: &str) -> Result<Vec<u8>> {
        let path = self.input.as_ref().context("Open an APK first")?;
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let entry = zip.by_name(name)?;
        ensure!(
            entry.size() <= 2 * 1024 * 1024,
            "Resource exceeds 2 MiB decoding limit"
        );
        let mut bytes = Vec::new();
        entry.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 2 * 1024 * 1024,
            "Resource exceeds decoding limit"
        );
        Ok(bytes)
    }
    pub fn resource_error(&self) -> Option<&str> {
        self.resource_error.as_deref()
    }
    pub fn resource_table(&self) -> &crate::resource_table::ResourceTable {
        &self.resources
    }
    pub fn read_resource_with_metadata(&mut self, name: &str) -> Result<DecompiledCode> {
        let bytes = self.resource_bytes(name)?;
        let (source, references) = crate::native_resources::decode_with_references(&bytes)?;
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
        self.resources.document(id)
    }
    pub fn start() -> Result<Self> {
        Ok(Self::default())
    }
    pub fn definition_names(&mut self, classes: &[String]) -> Result<DefinitionNameIndex> {
        self.native.definition_names(classes)
    }
    pub fn decompile_with_metadata(&mut self, class: &str) -> Result<DecompiledCode> {
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
        self.resources.decorate(&mut code, false);
        code.source_hash = source_identity(&code.source);
        Ok(code)
    }
    pub fn decompile_for_search(&mut self, class: &str) -> Result<DecompiledCode> {
        self.decompile_with_metadata(class)
    }
    pub fn decompile_search_cancellable(
        &mut self,
        class: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<DecompiledCode> {
        let mut code = self
            .native
            .render_cancellable(class, &|| cancel.load(std::sync::atomic::Ordering::Relaxed))?;
        ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "Search cancelled"
        );
        self.resources.decorate(&mut code, false);
        Ok(code)
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
        let code = self.decompile_with_metadata(class)?;
        let position = code
            .definitions
            .iter()
            .find(|d| d.kind == "class")
            .map_or(0, |d| d.start);
        Ok(NavigationResult {
            class: class.into(),
            code,
            position,
        })
    }
    fn definition_symbol(
        &self,
        class: &str,
        code: &DecompiledCode,
        def: &CodeDefinition,
    ) -> Option<String> {
        let class = class.strip_prefix("dex://").unwrap_or(class);
        if def.kind == "method" && code.source.starts_with("// Native DEX disassembly.") {
            return Some(
                code.source
                    .chars()
                    .skip(def.start)
                    .take(def.end - def.start)
                    .collect(),
            );
        }
        if def.kind == "class" {
            return Some(class.into());
        }
        if let Some(link) = code
            .links
            .iter()
            .find(|link| link.start == def.start && link.end == def.end)
        {
            return Some(link.label.clone());
        }
        let span: String = code
            .source
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
        let code = self.decompile_with_metadata(class)?;
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
                    .and_then(|d| self.definition_symbol(class, &code, d))
            })
            .context("No native symbol at this position")
    }
    fn owner(&self, symbol: &str) -> Option<String> {
        self.classes
            .iter()
            .filter(|name| {
                symbol == name.as_str()
                    || symbol
                        .strip_prefix(name.as_str())
                        .is_some_and(|s| s.starts_with('.'))
            })
            .max_by_key(|name| name.len())
            .cloned()
    }
    pub fn navigate(&mut self, class: &str, offset: usize, hash: &str) -> Result<NavigationResult> {
        let symbol = self.symbol(class, offset, hash)?;
        if symbol.starts_with(crate::resource_table::PREFIX) {
            let target = symbol.split(" | ").next().unwrap().to_owned();
            let code = self.resource_document(&target)?;
            return Ok(NavigationResult {
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
        let code = self.decompile_with_metadata(&owner)?;
        let position = code
            .definitions
            .iter()
            .find(|d| {
                self.definition_symbol(&owner, &code, d).as_deref() == Some(&declaration_symbol)
            })
            .context("Native declaration mapping unavailable")?
            .start;
        Ok(NavigationResult {
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
            classes: self.classes.clone(),
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
                    && self.definition_symbol(&owner, &code, d).as_deref() == Some(symbol)
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
        self.input = None;
        self.classes.clear();
        self.resources = Default::default();
        self.resource_error = None;
        let path = path.canonicalize()?;
        let project = self.native.open(&path)?;
        if project
            .resources
            .iter()
            .any(|name| name == "resources.arsc")
        {
            let loaded = (|| -> Result<crate::resource_table::ResourceTable> {
                let mut zip = zip::ZipArchive::new(std::fs::File::open(&path)?)?;
                let entry = match zip.by_name("resources.arsc") {
                    Ok(entry) => entry,
                    Err(zip::result::ZipError::FileNotFound) => return Ok(Default::default()),
                    Err(error) => return Err(error.into()),
                };
                ensure!(
                    entry.size() <= 64 * 1024 * 1024,
                    "Resource table exceeds 64 MiB"
                );
                let mut bytes = Vec::new();
                entry.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                crate::resource_table::ResourceTable::parse(&bytes)
            })();
            match loaded {
                Ok(resources) => self.resources = resources,
                Err(error) => {
                    self.resource_error = Some(format!("Resource names unavailable: {error:#}"))
                }
            }
        }
        self.input = Some(path);
        self.classes = project.classes.clone();
        Ok(project)
    }
    fn decompile(&mut self, class: &str) -> Result<String> {
        Ok(self.decompile_with_metadata(class)?.source)
    }
    fn read_resource(&mut self, name: &str) -> Result<String> {
        crate::native_resources::decode(&self.resource_bytes(name)?)
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
