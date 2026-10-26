//! Bounded, deterministic native metadata scans. Never retain the full match set.
use super::*;
use crate::native_dex::{DexAnnotationSet, DexClass, DexValue};
use crate::native_engine::disassembly;

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
fn strings(
    value: &DexValue,
    cancel: &AtomicBool,
    emit: &mut impl FnMut(u32) -> Result<()>,
) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    match value {
        DexValue::String(i) => emit(*i)?,
        DexValue::Array(v) => {
            for x in v {
                strings(x, cancel, emit)?;
            }
        }
        DexValue::Annotation { elements, .. } => {
            for (_, x) in elements {
                strings(x, cancel, emit)?;
            }
        }
        _ => {}
    }
    Ok(())
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
pub(super) fn literals(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let query = required(args, "query")?;
    ensure!(query.len() <= 4096, "Query exceeds 4096 bytes");
    let mut page = Page::new(args)?;
    let mut scanned_classes = 0;
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if !package_matches(name, args) {
            continue;
        }
        scanned_classes += 1;
        let c = engine
            .dex_class(name)
            .context("Missing class during literal scan")?;
        let mut emit = |index: u32, owner: &Value, pc: Option<usize>| -> Result<()> {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            let value = c
                .symbols
                .strings
                .get(index as usize)
                .context("Invalid string operand")?;
            if !(if args["match_mode"] == "exact" {
                value == query
            } else {
                value.contains(query)
            }) {
                return Ok(());
            }
            let mut item = owner.clone();
            item["class_id"] = json!(name);
            item["dex_class_id"] = json!(name);
            item["string_index"] = json!(index);
            item["code_unit_offset"] = json!(pc);
            item["value"] = json!(value.chars().take(4096).collect::<String>());
            item["value_chars"] = json!(value.chars().count());
            item["value_truncated"] = json!(value.chars().count() > 4096);
            page.add(item)
        };
        let mut statics = c.static_values.iter();
        for f in &c.fields {
            if f.is_static
                && let Some(v) = statics.next()
            {
                let owner = json!({"field_id":format!("{name}.{}:{}",f.name,f.field_type),"method_id":null,"kind":"static_value","parameter_index":null});
                strings(v, cancel, &mut |i| emit(i, &owner, None))?;
            }
        }
        annotation_sets(c, name, |mut owner, set| {
            owner["kind"] = json!(format!("{}_annotation", owner["element"].as_str().unwrap()));
            owner.as_object_mut().unwrap().remove("element");
            for a in set.iter() {
                for (_, v) in &a.elements {
                    strings(v, cancel, &mut |i| emit(i, &owner, None))?;
                }
            }
            Ok(())
        })?;
        for m in &c.methods {
            let Some(code) = &m.code else {
                continue;
            };
            let owner = json!({"method_id":disassembly::method_id(m),"field_id":null,"kind":"instruction","parameter_index":null});
            let words = &code.instructions;
            let mut pc = 0;
            while pc < words.len() {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                let width = disassembly::width(words, pc)
                    .context("Malformed instruction during literal scan")?;
                ensure!(
                    pc + width <= words.len(),
                    "Truncated instruction during literal scan"
                );
                match words[pc] as u8 {
                    0x1a => emit(u32::from(words[pc + 1]), &owner, Some(pc))?,
                    0x1b => emit(
                        u32::from(words[pc + 1]) | (u32::from(words[pc + 2]) << 16),
                        &owner,
                        Some(pc),
                    )?,
                    _ => {}
                }
                pc += width;
            }
        }
    }
    let mut result = page.finish()?;
    result["search_mode"] = json!("streaming");
    result["index_complete"] = json!(false);
    result["scan_complete"] = json!(true);
    result["scanned_project_classes"] = json!(scanned_classes);
    result["project_classes"] = json!(project.classes.len());
    result["coverage"] = json!(
        "All loaded classes matching package_prefix; const-string/jumbo and encoded static/annotation strings. No dynamic string reconstruction."
    );
    result["ordering"] = json!(
        "class declaration order, static values, annotations, instructions; stable while project is unchanged"
    );
    Ok(result)
}

/// Stream asset contents one archive member at a time. Never retain a corpus.
pub(super) fn assets(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    ensure!(
        args.get("config").is_none(),
        "config applies to scope=resource_table; asset files have no ARSC configuration"
    );
    let query = required(args, "query")?;
    let sensitive = args["case_sensitive"].as_bool().unwrap_or(true);
    let needle = if sensitive {
        query.to_owned()
    } else {
        query.to_lowercase()
    };
    let matches = |s: &str| {
        if sensitive {
            s.contains(&needle)
        } else {
            s.to_lowercase().contains(&needle)
        }
    };
    let mut files: Vec<_> = project
        .resources
        .iter()
        .filter(|p| {
            p.split('/').any(|s| s == "assets")
                && p.starts_with(args["asset_prefix"].as_str().unwrap_or(""))
        })
        .collect();
    files.sort();
    files.dedup();
    let total_files = files.len();
    let mut reader = engine.resource_reader()?;
    let mut page = Page::new(args)?;
    let mut scanned = 0;
    let mut skipped = 0;
    let mut diagnostics = Vec::new();
    for path in files {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let content = reader
            .read_path(path, 8 * 1024 * 1024)
            .and_then(|b| Ok(crate::engine::archive_text(&b)?.to_owned()));
        match content {
            Ok(content) => {
                scanned += 1;
                let mut count = 0;
                let mut preview = Vec::new();
                for (i, line) in content.lines().enumerate() {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    if matches(line) {
                        count += 1;
                        if preview.len() < 8 {
                            preview.push(json!({"line":i+1,"text":line.chars().take(512).collect::<String>(),"text_truncated":line.chars().count()>512}));
                        }
                    }
                }
                if count > 0 || matches(path) {
                    page.add(json!({"resource_path":path,"matched_path":matches(path),"matching_lines":count,"preview":preview,"preview_truncated":count>8,"source_hash":crate::engine::source_identity(&content),"size_bytes":content.len()}))?;
                }
            }
            Err(e) => {
                skipped += 1;
                if diagnostics.len() < 100 {
                    diagnostics.push(json!({"resource_path":path,"reason":e.to_string()}));
                }
            }
        }
    }
    let mut result = page.finish()?;
    result["scope"] = json!("assets");
    result["complete"] = json!(skipped == 0);
    result["coverage"] = json!({"files_total":total_files,"text_files_scanned":scanned,"files_skipped":skipped,"skipped_files":diagnostics,"diagnostics_truncated":skipped>100,"per_file_byte_limit":8*1024*1024});
    Ok(result)
}
