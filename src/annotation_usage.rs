//! Direct bounded DEX annotation usage search shared by GUI and MCP.
use crate::{
    annotation_values::elements_value,
    engine::{NativeEngine, Project},
    native_dex::{DexAnnotationSet, DexClass},
    native_engine::disassembly,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
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
pub(crate) fn scan_classes<'a>(
    classes: impl IntoIterator<Item = Result<(&'a str, &'a crate::native_dex::DexClass)>>,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let requested = args["annotation_type"]
        .as_str()
        .context("Missing annotation_type")?;
    let descriptor = if requested.starts_with('L') && requested.ends_with(';') {
        requested.to_owned()
    } else {
        format!("L{};", requested.replace('.', "/"))
    };
    let element = args["element"].as_str().unwrap_or("all");
    let mut page = Page::new(args)?;
    let mut seen_tables = HashSet::new();
    let mut type_present = false;
    for class in classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let (name, c) = class?;
        // Presence is project-wide, independent of result filters. Each shared
        // DEX type table is checked once; no Java reconstruction or index build.
        if !type_present && seen_tables.insert(Arc::as_ptr(&c.symbols)) {
            type_present = c.symbols.types.iter().any(|ty| ty.as_ref() == descriptor);
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
                if c.symbols
                    .types
                    .get(annotation.type_idx as usize)
                    .is_none_or(|t| t.as_ref() != descriptor)
                {
                    continue;
                }
                let mut item = owner.clone();
                item["annotation_type"] = json!(descriptor);
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
    result["annotation_type"] = json!(descriptor);
    result["match_mode"] = json!("exact_descriptor");
    result["requested_type_present"] = json!(type_present);
    result["type_presence_scope"] =
        json!("DEX type tables of all loaded classes, before result filters");
    result["warnings"] = if type_present {
        json!([])
    } else {
        json!([{
            "code": "ANNOTATION_TYPE_ABSENT",
            "message": "The requested annotation descriptor is absent from the loaded DEX type tables. Obfuscation, removal, or an incorrect name may explain this. Zero exact-descriptor usages do not establish that equivalent annotations or API endpoints are absent. Query the actual DEX annotation name; no semantic alias was inferred."
        }])
    };
    result["scope"] = json!(
        "Complete covers explicit exact-descriptor DEX annotation usages within the supplied filters, not semantic equivalents under obfuscation. Parameter_index excludes the receiver. No inherited annotations or annotation defaults inferred."
    );
    Ok(result)
}
