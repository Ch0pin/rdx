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
}

/// A deterministic identity for locally generated source, not a security digest.
pub fn source_identity(source: &str) -> String {
    let mut hash = DefaultHasher::new();
    source.hash(&mut hash);
    format!("native-v1-{:016x}-{}", hash.finish(), source.len())
}

impl NativeEngine {
    pub fn start() -> Result<Self> {
        Ok(Self::default())
    }
    pub fn definition_names(&mut self, classes: &[String]) -> Result<DefinitionNameIndex> {
        self.native.definition_names(classes)
    }
    pub fn decompile_with_metadata(&mut self, class: &str) -> Result<DecompiledCode> {
        let mut code = self.native.render(class)?;
        code.source_hash = source_identity(&code.source);
        Ok(code)
    }
    pub fn decompile_for_search(&mut self, class: &str) -> Result<DecompiledCode> {
        self.decompile_with_metadata(class)
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
        let owner = self
            .owner(&symbol)
            .context("Symbol is external to this project")?;
        let code = self.decompile_with_metadata(&owner)?;
        let position = code
            .definitions
            .iter()
            .find(|d| self.definition_symbol(&owner, &code, d).as_deref() == Some(&symbol))
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
}
impl DecompilerEngine for NativeEngine {
    fn open(&mut self, path: &Path) -> Result<Project> {
        self.input = None;
        self.classes.clear();
        let path = path.canonicalize()?;
        let project = self.native.open(&path)?;
        self.input = Some(path);
        self.classes = project.classes.clone();
        Ok(project)
    }
    fn decompile(&mut self, class: &str) -> Result<String> {
        Ok(self.decompile_with_metadata(class)?.source)
    }
    fn read_resource(&mut self, name: &str) -> Result<String> {
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
        crate::native_resources::decode(&bytes)
    }
}
