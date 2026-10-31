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
    let items: Vec<_> = project
        .classes
        .iter()
        .filter(|c| {
            if sensitive {
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
    if absent {
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
    let mut result = engine.search_identifiers(&query, cancel)?;
    result["requested_type"] = json!(name);
    result["xml_reference_tool"] = json!("find_layout_references");
    Ok(result)
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
