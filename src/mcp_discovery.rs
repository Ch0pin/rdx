//! Identity-preserving discovery and collision-safe bulk exports.
use super::*;
use crate::native_dex::DexValue;

pub(super) fn check_export_parent(out: &Path) -> Result<()> {
    let parent = out
        .parent()
        .context("Export path has no parent directory")?;
    ensure!(
        parent.is_dir(),
        "Export parent directory missing or not a directory: {}",
        parent.display()
    );
    ensure!(
        !out.exists(),
        "Export directory must be new: {}",
        out.display()
    );
    Ok(())
}
pub(super) fn classes(project: &Project, args: &Value) -> Result<Value> {
    let query = args["query"].as_str().unwrap_or("");
    let sensitive = args["case_sensitive"].as_bool().unwrap_or(true);
    let folded = query.to_lowercase();
    let scope = crate::package_scope::PackageScope::from_args(args)?;
    let items: Vec<_> = project
        .classes
        .iter()
        .filter(|c| {
            scope.matches(c)
                && if sensitive {
                    c.contains(query)
                } else {
                    c.to_lowercase().contains(&folded)
                }
        })
        .map(|c| json!({"class_id":c,"name":c}))
        .collect();
    let absent = items.is_empty();
    let mut result = page(items, args)?;
    result["case_sensitive"] = json!(sensitive);
    result["package_scope"] = scope.describe();
    if absent {
        // Near matches stay project-wide so a class excluded by the scope is
        // still discoverable.
        result["suggestions_scope"] =
            json!("all loaded classes, case-insensitive, ignoring package_prefix");
        result["suggestions"] = json!(
            project
                .classes
                .iter()
                .filter(|c| c.to_lowercase().contains(&folded))
                .take(20)
                .collect::<Vec<_>>()
        );
        result["scope_note"] = json!(
            "No matching definition in the loaded DEX set. Runtime loading is possible but is not established by absence."
        );
    }
    Ok(result)
}
pub(super) fn types(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let requested = required(args, "class_name")?;
    let name =
        if requested.starts_with('[') || (requested.starts_with('L') && requested.ends_with(';')) {
            crate::native_engine::disassembly::type_name(requested)
        } else if requested.contains('.') || engine.dex_class(requested).is_some() {
            requested.to_owned()
        } else {
            crate::allocation_references::resolve_target(engine, project, requested, cancel)?
        };
    let mut query = args.clone();
    query["query"] = json!(name);
    query["symbol_kind"] = json!("class");
    query["include_array_components"] = json!(true);
    query["occurrence"] = json!("reference");
    query["match_mode"] = json!("exact");
    // Keep annotation references out of the general identifier index. The
    // decoded DEX directories already provide an exact, bounded projection.
    // Force the complete index/streaming path here: a resumable scan cursor
    // cannot be combined with the annotation page's hit offset.
    query["search_mode"] = json!("index");
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(100) as usize;
    let mut count_query = query.clone();
    count_query["offset"] = json!(0);
    count_query["limit"] = json!(limit);
    let count_result = engine.search_identifiers(&count_query, cancel)?;
    let identifier_count = count_result["total"]
        .as_u64()
        .context("Identifier count unavailable")? as usize;
    let mut annotation_query = json!({"annotation_type":format!("L{};",name.replace('.',"/")),
        "offset":0,"limit":1});
    if let Some(prefix) = args.get("package_prefix") {
        annotation_query["package_prefix"] = prefix.clone();
    }
    let mut result = if offset == 0 || offset >= identifier_count {
        count_result
    } else {
        query["offset"] = json!(offset);
        query["limit"] = json!(limit);
        engine.search_identifiers(&query, cancel)?
    };
    let mut items = if offset >= identifier_count && offset != 0 {
        Vec::new()
    } else {
        result["items"]
            .as_array()
            .context("Missing identifier items")?
            .clone()
    };
    for item in &mut items {
        item["reference_kind"] = item["context"].clone();
    }
    let include_annotations =
        items.len() < limit && offset.saturating_add(items.len()) >= identifier_count;
    if include_annotations {
        annotation_query["offset"] = json!(offset.saturating_sub(identifier_count));
        annotation_query["limit"] = json!(limit - items.len());
    }
    let annotations = engine.annotation_usages(project, &annotation_query, cancel)?;
    let annotation_count = annotations["total"]
        .as_u64()
        .context("Annotation count unavailable")? as usize;
    let total = identifier_count.saturating_add(annotation_count);
    ensure!(offset <= total, "Invalid matching-occurrence offset");
    if include_annotations {
        for annotation in annotations["items"]
            .as_array()
            .context("Missing annotation items")?
        {
            items.push(annotation_type_item(&name, annotation));
        }
    }
    // Both sources use a 1 MiB page budget. Apply that budget again to the
    // combined records, in their actual page order, before advancing offset.
    trim_type_page(&mut items, 1024 * 1024)?;
    let source_hash = type_source_hash(
        &name,
        args["package_prefix"].as_str().unwrap_or(""),
        count_result_hash(&result),
        annotations["source_hash"].as_str().unwrap_or(""),
    );
    let end = offset + items.len();
    result["items"] = json!(items);
    result["total"] = json!(total);
    result["offset"] = json!(offset);
    result["next_offset"] = json!((end < total).then_some(end));
    result["exhausted"] = json!(end == total);
    result["pagination_order"] = json!("identifier_references_then_annotation_declaration_order");
    result["annotation_reference_count"] = json!(annotation_count);
    result["source_hash"] = json!(source_hash);
    result["page_byte_budget"] = json!(1024 * 1024);
    result["scope"] = json!(
        "Original DEX instruction and declaration type references, followed by explicit class, field, method, and parameter annotation type references with decoded values. Annotation defaults, nested annotation value types, reflection, and runtime reachability are not inferred."
    );
    result["requested_type"] = json!(name);
    result["xml_reference_tool"] = json!("find_layout_references");
    Ok(result)
}
fn annotation_type_item(name: &str, annotation: &Value) -> Value {
    let mut item = annotation.clone();
    item["symbol_id"] = json!(name);
    item["symbol_name"] = json!(name.rsplit('.').next().unwrap_or(name));
    item["symbol_kind"] = json!("class");
    item["occurrence"] = json!("reference");
    item["context"] = json!("annotation_type");
    item["reference_kind"] = json!("annotation_type");
    item["code_unit_offset"] = Value::Null;
    item
}
fn trim_type_page(items: &mut Vec<Value>, budget: usize) -> Result<()> {
    let mut bytes = 0usize;
    let mut keep = 0usize;
    for item in items.iter() {
        let size = serde_json::to_vec(item)?.len();
        ensure!(
            size <= 1024 * 1024,
            "Type reference record exceeds output budget"
        );
        if bytes.saturating_add(size) > budget {
            break;
        }
        bytes += size;
        keep += 1;
    }
    items.truncate(keep);
    Ok(())
}
fn type_source_hash(
    name: &str,
    prefix: &str,
    identifier_hash: &str,
    annotation_hash: &str,
) -> String {
    let mut hash = Sha256::new();
    for part in [
        "type-references-v2",
        name,
        prefix,
        identifier_hash,
        annotation_hash,
    ] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    format!("{:x}", hash.finalize())
}
fn count_result_hash(result: &Value) -> &str {
    result["source_hash"]
        .as_str()
        .or_else(|| result["index_hash"].as_str())
        .unwrap_or("")
}
pub(super) fn metadata(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let query = required(args, "query")?;
    let sensitive = args["case_sensitive"].as_bool().unwrap_or(true);
    let folded = query.to_lowercase();
    let mut page = scan::Page::new(args)?;
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if !scan::package_matches(name, args) {
            continue;
        }
        let c = engine.dex_class(name).context("Missing project class")?;
        let Some(set) = c
            .symbols
            .annotations
            .get(&c.annotations_offset)
            .and_then(|d| d.class.as_ref())
        else {
            continue;
        };
        for a in set.iter().filter(|a| {
            c.symbols
                .types
                .get(a.type_idx as usize)
                .is_some_and(|t| t.as_ref() == "Lkotlin/Metadata;")
        }) {
            for (key, value) in &a.elements {
                if c.symbols
                    .strings
                    .get(*key as usize)
                    .is_none_or(|k| k != "d2")
                {
                    continue;
                }
                let DexValue::Array(values) = value else {
                    continue;
                };
                for (index, value) in values.iter().enumerate() {
                    let DexValue::String(i) = value else { continue };
                    let text = c
                        .symbols
                        .strings
                        .get(*i as usize)
                        .context("Invalid metadata string")?;
                    if if sensitive {
                        text.contains(query)
                    } else {
                        text.to_lowercase().contains(&folded)
                    } {
                        page.add(json!({"class_id":name,"descriptor":c.descriptor.as_ref(),"annotation":"kotlin.Metadata","string_table":"d2","index":index,"value":text.chars().take(4096).collect::<String>(),"value_truncated":text.chars().count()>4096,"relationship":"metadata_string_evidence","verified_declaration_alias":false}))?;
                    }
                }
            }
        }
    }
    let mut result = page.finish()?;
    result["scope"] = json!(
        "Class kotlin.Metadata d2 strings; table entries may name referenced types or members. Original declaration aliases require decoding d1 and are not inferred."
    );
    Ok(result)
}
pub(super) fn export_package(
    engine: &mut NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    exports::synchronous(engine, project, args, cancel)
}

#[cfg(test)]
mod type_reference_tests {
    use super::*;
    #[test]
    fn mixed_identifier_annotation_page_is_contiguous_and_byte_bounded() {
        let annotation = json!({"class_id":"wv.j1","method_id":"wv.j1.a()V","element":"parameter","parameter_index":0,"annotation_type":"Lkd0/y;","values":{"value":{"kind":"string","value":"x"}}});
        let mut page = vec![
            json!({"symbol_id":"kd0.y","context":"instruction"}),
            annotation_type_item("kd0.y", &annotation),
            annotation_type_item("kd0.y", &annotation),
        ];
        let budget = serde_json::to_vec(&page[0]).unwrap().len()
            + serde_json::to_vec(&page[1]).unwrap().len();
        trim_type_page(&mut page, budget).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0]["context"], "instruction");
        assert_eq!(page[1]["reference_kind"], "annotation_type");
        assert_eq!(page[1]["parameter_index"], 0);
        assert_eq!(page[1]["values"]["value"]["value"], "x");
        let offset = 1usize;
        let total = 4usize;
        assert_eq!(offset + page.len(), 3);
        assert!(offset + page.len() < total);
    }
    #[test]
    fn composite_hash_changes_with_annotation_values_and_scope() {
        let a = json!({"values":{"value":"one"}});
        let b = json!({"values":{"value":"two"}});
        let hash = |v: &Value| format!("{:x}", Sha256::digest(serde_json::to_vec(v).unwrap()));
        assert_ne!(
            type_source_hash("kd0.y", "wv", "identifiers", &hash(&a)),
            type_source_hash("kd0.y", "wv", "identifiers", &hash(&b))
        );
        assert_ne!(
            type_source_hash("kd0.y", "wv", "identifiers", &hash(&a)),
            type_source_hash("kd0.y", "yv", "identifiers", &hash(&a))
        );
    }
}
