//! Direct bounded DEX annotation usage search shared by GUI and MCP.
use crate::{
    annotation_values::elements_value,
    engine::{NativeEngine, Project},
    native_dex::{DexAnnotationSet, DexClass},
    native_engine::disassembly,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) fn search(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    scan_classes(
        project.classes.iter().map(|name| {
            Ok((
                name.as_str(),
                engine
                    .dex_class(name)
                    .context("Missing class during annotation scan")?,
            ))
        }),
        args,
        cancel,
    )
}

pub(super) struct Page {
    start: usize,
    limit: usize,
    total: usize,
    bytes: usize,
    full: bool,
    items: Vec<Value>,
    hash: Sha256,
}
impl Page {
    pub(super) fn new(args: &Value) -> Result<Self> {
        let limit = args["limit"].as_u64().unwrap_or(100) as usize;
        ensure!((1..=500).contains(&limit), "limit must be 1..500");
        Ok(Self {
            start: args["offset"].as_u64().unwrap_or(0) as usize,
            limit,
            total: 0,
            bytes: 0,
            full: false,
            items: Vec::new(),
            hash: Sha256::new(),
        })
    }
    pub(super) fn add(&mut self, item: Value) -> Result<()> {
        let encoded = serde_json::to_vec(&item)?;
        self.hash.update(&encoded);
        self.hash.update(b"\n");
        if self.total >= self.start && self.items.len() < self.limit && !self.full {
            if self.bytes + encoded.len() > 1024 * 1024 {
                ensure!(
                    !self.items.is_empty(),
                    "RESULT_TOO_LARGE: one record exceeds 1 MiB"
                );
                self.full = true;
            } else {
                self.bytes += encoded.len();
                self.items.push(item);
            }
        }
        self.total += 1;
        Ok(())
    }
    pub(super) fn finish(self) -> Result<Value> {
        ensure!(self.start <= self.total, "Invalid offset");
        let end = self.start + self.items.len();
        Ok(
            json!({"items":self.items,"total":self.total,"offset":self.start,"next_offset":(end<self.total).then_some(end),"complete":true,"source_hash":format!("{:x}",self.hash.finalize()),"page_byte_budget":1048576}),
        )
    }
}
pub(super) fn package_matches(name: &str, args: &Value) -> bool {
    let prefix = args["package_prefix"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.');
    prefix.is_empty()
        || name == prefix
        || name
            .strip_prefix(prefix)
            .is_some_and(|s| s.starts_with('.'))
}
/// Enumerate annotation owners in declaration order, including parameter slots.
pub(super) fn annotation_sets(
    class: &DexClass,
    name: &str,
    mut emit: impl FnMut(Value, &DexAnnotationSet) -> Result<()>,
) -> Result<()> {
    let Some(d) = class.symbols.annotations.get(&class.annotations_offset) else {
        return Ok(());
    };
    if let Some(set) = &d.class {
        emit(json!({"class_id":name,"element":"class"}), set)?;
    }
    for (i, f) in class.fields.iter().enumerate() {
        if let Some(Some(set)) = d.fields.get(i) {
            emit(
                json!({"class_id":name,"field_id":format!("{name}.{}:{}",f.name,f.field_type),"element":"field"}),
                set,
            )?;
        }
    }
    for (i, m) in class.methods.iter().enumerate() {
        let id = disassembly::method_id(m);
        if let Some(Some(set)) = d.methods.get(i) {
            emit(
                json!({"class_id":name,"method_id":id,"element":"method"}),
                set,
            )?;
        }
        if let Some(params) = d.parameters.get(i) {
            for (p, set) in params.iter().enumerate() {
                if let Some(set) = set {
                    emit(
                        json!({"class_id":name,"method_id":id,"element":"parameter","parameter_index":p}),
                        set,
                    )?;
                }
            }
        }
    }
    Ok(())
}
/// How `annotation_type` selects DEX annotation types.
enum AnnotationQuery {
    /// The complete descriptor, `Lretrofit2/http/GET;`.
    Exact(String),
    /// The simple class name after the last package separator, `GET`.
    SimpleName(String),
    /// A literal prefix of the internal name, `retrofit2/http`.
    Prefix(String),
}
impl AnnotationQuery {
    fn parse(args: &Value) -> Result<Self> {
        let requested = args["annotation_type"]
            .as_str()
            .context("Missing annotation_type")?;
        let internal = requested
            .strip_prefix('L')
            .and_then(|s| s.strip_suffix(';'))
            .unwrap_or(requested)
            .replace('.', "/");
        ensure!(!internal.is_empty(), "annotation_type must not be empty");
        Ok(
            match args["match_mode"].as_str().unwrap_or("exact_descriptor") {
                "exact_descriptor" => Self::Exact(format!("L{internal};")),
                "simple_name" => {
                    let name = internal
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or_default();
                    ensure!(!name.is_empty(), "annotation_type has no simple name");
                    Self::SimpleName(name.to_owned())
                }
                "prefix" => Self::Prefix(internal),
                other => bail!(
                    "Invalid match_mode {other:?}; use exact_descriptor, simple_name or prefix"
                ),
            },
        )
    }
    fn mode(&self) -> &'static str {
        match self {
            Self::Exact(_) => "exact_descriptor",
            Self::SimpleName(_) => "simple_name",
            Self::Prefix(_) => "prefix",
        }
    }
    fn pattern(&self) -> &str {
        match self {
            Self::Exact(s) | Self::SimpleName(s) | Self::Prefix(s) => s,
        }
    }
    fn matches(&self, descriptor: &str) -> bool {
        match self {
            Self::Exact(d) => descriptor == d,
            Self::SimpleName(name) => {
                descriptor
                    .strip_prefix('L')
                    .and_then(|s| s.strip_suffix(';'))
                    .and_then(|s| s.rsplit('/').next())
                    == Some(name)
            }
            Self::Prefix(prefix) => descriptor
                .strip_prefix('L')
                .and_then(|s| s.strip_suffix(';'))
                .is_some_and(|s| s.starts_with(prefix)),
        }
    }
    fn absence_message(&self) -> String {
        let selector = match self {
            Self::Exact(d) => format!("The annotation descriptor {d} is absent from"),
            Self::SimpleName(n) => format!("No annotation type with simple name {n:?} exists in"),
            Self::Prefix(p) => format!("No class type with internal-name prefix {p:?} exists in"),
        };
        format!(
            "{selector} the loaded DEX type tables. Obfuscation, removal, or an incorrect name may explain this. Zero matches do not establish that equivalent annotations or API endpoints are absent. Retry with match_mode simple_name or prefix, inspect get_class_disassembly annotation directives on a known owner, or use find_network_stacks, which recovers renamed Retrofit annotation roles."
        )
    }
}
/// Bounded distinct annotation types matched in the current scan.
const MATCHED_TYPE_LIMIT: usize = 200;
pub(crate) fn scan_classes<'a>(
    classes: impl IntoIterator<Item = Result<(&'a str, &'a crate::native_dex::DexClass)>>,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let query = AnnotationQuery::parse(args)?;
    let element = args["element"].as_str().unwrap_or("all");
    let mut page = Page::new(args)?;
    let mut seen_tables = HashSet::new();
    let mut type_present = false;
    let mut matched_types = BTreeSet::new();
    let mut matched_types_complete = true;
    for class in classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let (name, c) = class?;
        // Presence is project-wide, independent of result filters. Each shared
        // DEX type table is checked once; no Java reconstruction or index build.
        if !type_present && seen_tables.insert(Arc::as_ptr(&c.symbols)) {
            type_present = c.symbols.types.iter().any(|ty| query.matches(ty));
        }
        if !package_matches(name, args) {
            continue;
        }
        annotation_sets(c, name, |owner, set| {
            if element != "all" && owner["element"] != element {
                return Ok(());
            }
            for annotation in set.iter() {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                let Some(descriptor) = c
                    .symbols
                    .types
                    .get(annotation.type_idx as usize)
                    .filter(|t| query.matches(t))
                else {
                    continue;
                };
                if matched_types.len() < MATCHED_TYPE_LIMIT
                    || matched_types.contains(descriptor.as_ref())
                {
                    matched_types.insert(descriptor.to_string());
                } else {
                    matched_types_complete = false;
                }
                let mut item = owner.clone();
                item["annotation_type"] = json!(descriptor.as_ref());
                item["visibility"] = json!(annotation.visibility);
                item["values"] =
                    elements_value(&annotation.elements, &c.symbols, &mut (256 * 1024))?;
                item["dex_entry"] = json!(c.symbols.origin.get());
                page.add(item)?;
            }
            Ok(())
        })?;
    }
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    let mut result = page.finish()?;
    result["annotation_type"] = json!(query.pattern());
    result["match_mode"] = json!(query.mode());
    result["matched_types"] = json!(matched_types);
    result["matched_types_complete"] = json!(matched_types_complete);
    result["requested_type_present"] = json!(type_present);
    result["type_presence_scope"] =
        json!("DEX type tables of all loaded classes, before result filters");
    result["warnings"] = if type_present {
        json!([])
    } else {
        json!([{
            "code": "ANNOTATION_TYPE_ABSENT",
            "message": query.absence_message()
        }])
    };
    result["scope"] = json!(
        "Complete covers explicit DEX annotation usages whose type matches match_mode (exact descriptor, simple name after the last package separator, or literal internal-name prefix) within the supplied filters, not semantic equivalents under obfuscation. Parameter_index excludes the receiver. No inherited annotations or annotation defaults inferred."
    );
    Ok(result)
}
