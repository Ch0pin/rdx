//! Cheap query feedback: no source rendering or additional indexes.
use super::*;

/// Canonical parameter names with the aliases each tool accepts. Schema
/// generation and request normalization share this table, so an alias is
/// always advertised, never inferred. Aliases never change meaning.
pub(super) fn argument_aliases(name: &str) -> Vec<(&'static str, &'static [&'static str])> {
    let mut table: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if matches!(
        name,
        "get_method_source"
            | "get_call_graph"
            | "export_source"
            | "resolve_reference"
            | "resolve_switch_case_callers"
    ) {
        table.push(("method_id", &["method_signature", "signature"]));
    }
    match name {
        "get_call_graph_batch" => {
            table.push((
                "method_ids",
                &["method_id", "method_signature", "signature"],
            ));
        }
        "find_string_references" => table.push(("query", &["value", "string", "literal", "text"])),
        "find_type_references" | "find_layout_references" => {
            table.push(("class_name", &["type_name", "class_id", "type"]));
        }
        "find_integer_references" => table.push(("values", &["value"])),
        "find_config_key_consumers" => table.push(("key", &["query", "value", "config_key"])),
        "search_classes_by_keyword"
        | "search_code"
        | "search_kotlin_metadata"
        | "search_resource_xml"
        | "search_resources"
        | "get_all_classes"
        | "get_all_resource_file_names"
        | "get_strings"
        | "get_dex_strings" => table.push(("query", &["keyword", "text", "value"])),
        _ => {}
    }
    table
}

/// Rewrite aliases to their canonical parameter. A bare value for a list
/// parameter becomes a one-element list. An alias supplied alongside its
/// canonical parameter must carry the same value.
pub(super) fn aliases(name: &str, args: &Value) -> Result<Value> {
    let mut args = args.clone();
    let table = argument_aliases(name);
    if table.is_empty() {
        return Ok(args);
    }
    let object = args
        .as_object_mut()
        .context("Arguments must be an object")?;
    for (canonical, names) in table {
        let list = matches!(canonical, "method_ids" | "values");
        if list && let Some(single) = object.get(canonical).filter(|v| !v.is_array()).cloned() {
            object.insert(canonical.into(), json!([single]));
        }
        let code = if canonical.starts_with("method") {
            "CONFLICTING_METHOD_SELECTORS"
        } else {
            "CONFLICTING_ARGUMENTS"
        };
        for alias in names {
            let Some(value) = object.remove(*alias) else {
                continue;
            };
            let value = if list && !value.is_array() {
                json!([value])
            } else {
                value
            };
            ensure!(
                if list {
                    value.as_array().is_some_and(|items| {
                        !items.is_empty()
                            && items
                                .iter()
                                .all(|item| item.as_str().is_none_or(|s| !s.is_empty()))
                    })
                } else {
                    value.as_str().is_some_and(|s| !s.is_empty())
                },
                "{alias} must be a nonempty value for {canonical}"
            );
            match object.get(canonical) {
                Some(existing) => ensure!(
                    existing == &value,
                    "{code}: {canonical} and its alias {alias} must agree (aliases: {})",
                    names.join(", ")
                ),
                None => {
                    object.insert(canonical.into(), value);
                }
            }
        }
    }
    Ok(args)
}

pub(super) fn selection(items: &[Value], args: &Value, all_registrations: bool) -> Value {
    let pinned = args["apk_sha256"].as_str().is_some_and(|s| !s.is_empty())
        || args["apk_path"]
            .as_str()
            .is_some_and(|s| Path::new(s).is_absolute());
    let candidates: Vec<_> = items
        .iter()
        .filter(|i| {
            i["state"] == "Running" && i["project_id"].as_str().is_some_and(|s| !s.is_empty())
        })
        .map(|i| json!({"instance_id":i["instance_id"],"project_id":i["project_id"]}))
        .collect();
    let unique = pinned && all_registrations && candidates.len() == 1;
    json!({"status": if !pinned {"identity_filter_required"} else if candidates.len()>1 {"ambiguous"}
        else if unique {"unique_healthy_match"} else if !candidates.is_empty() {"matching_candidate_discovery_incomplete"}
        else if all_registrations {"no_healthy_match"} else {"discovery_incomplete"},
        "selected": if unique {candidates[0].clone()} else {Value::Null},
        "candidates":candidates,
        "exact_match_unique":if !pinned {Value::Null} else if all_registrations || candidates.len()>1 {json!(unique)} else {Value::Null},
        "uniqueness_scope":"All registrations, independently of result pagination; null means unknown",
        "hint":"Candidates are reachable matches in this registration batch. You may explicitly pin an instance_id/project_id pair; multiple instances can open the same APK. Continue registration discovery to establish global uniqueness."})
}

pub(super) fn component_page(mut records: Vec<Value>, args: &Value) -> Result<Value> {
    records.retain(|record| crate::manifest_summary::exported_matches(record, args));
    crate::manifest_summary::project_records(&mut records, args);
    let hash = crate::engine::source_identity(&serde_json::to_string(&records)?);
    let total = records.len();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(100) as usize;
    let budget = args["max_bytes"].as_u64().unwrap_or(32768) as usize;
    ensure!(
        offset <= total && (1..=500).contains(&limit),
        "Invalid component page"
    );
    ensure!(
        (1024..=1048576).contains(&budget),
        "max_bytes must be 1024..1048576"
    );
    let mut items = Vec::new();
    let mut bytes = 2;
    for record in records.into_iter().skip(offset).take(limit) {
        let size = serde_json::to_vec(&record)?.len() + usize::from(!items.is_empty());
        if bytes + size > budget {
            ensure!(
                !items.is_empty(),
                "MANIFEST_RECORD_TOO_LARGE: component {} exceeds max_bytes={budget}; use projection=compact, fields, or increase max_bytes (maximum 1048576)",
                record["name"]
            );
            break;
        }
        bytes += size;
        items.push(record);
    }
    let end = offset + items.len();
    Ok(
        json!({"items":items,"total":total,"offset":offset,"next_offset":(end<total).then_some(end),
        "source_hash":hash,"items_bytes":bytes,"max_bytes":budget,"byte_budget_scope":"Serialized items array; response metadata is additional"}),
    )
}

pub(super) fn caller_status(result: &mut Value) {
    if let Some(sites) = result["call_sites"].as_array_mut() {
        for site in sites {
            site["caller_reconstruction"] = site["source_mapping"]["reconstruction_status"]
                .as_str()
                .map(|s| json!(s))
                .unwrap_or(json!("not_attempted"));
        }
        result["reconstruction_note"] = json!(
            "DEX call sites establish invokes, not absence of caller guards. not_attempted means this query did not reconstruct the caller; include_source_locations=true requests bounded reconstruction with reasons and locations."
        );
    }
}

pub(super) fn route_hints(
    result: &mut Value,
    resolved_xml: &str,
    args: &Value,
    component_mode: bool,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut diagnostics = if result["total"] == 0 {
        crate::manifest_summary::near_matches(resolved_xml, args, component_mode, || {
            cancel.load(Ordering::Relaxed)
        })
        .map_err(anyhow::Error::msg)?
    } else {
        json!({"status":"not_needed","warnings":[]})
    };
    result["warnings"] = diagnostics
        .as_object_mut()
        .unwrap()
        .remove("warnings")
        .unwrap();
    result["near_match_diagnostics"] = diagnostics;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_does_not_claim_global_uniqueness_from_one_batch() {
        let args = json!({"apk_path":"/tmp/app.apk","apk_sha256":"abc"});
        let items = vec![json!({"state":"Running","instance_id":"i","project_id":"p"})];
        let partial = selection(&items, &args, false);
        assert_eq!(partial["status"], "matching_candidate_discovery_incomplete");
        assert!(partial["selected"].is_null());
        assert!(partial["exact_match_unique"].is_null());
        assert_eq!(partial["candidates"][0]["project_id"], "p");
        assert_eq!(selection(&items, &args, true)["exact_match_unique"], true);
        assert_eq!(
            selection(&[items[0].clone(), items[0].clone()], &args, false)["exact_match_unique"],
            false
        );
        assert!(selection(&items, &json!({}), true)["selected"].is_null());
        assert_eq!(selection(&[], &args, true)["status"], "no_healthy_match");
    }
    #[test]
    fn signature_aliases_validate_and_dispatch() {
        let mut engine = NativeEngine::default();
        let project = engine
            .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
            .unwrap();
        let cancel = AtomicBool::new(false);
        for tool in ["get_method_source", "get_call_graph"] {
            let original = json!({"instance_id":"i","project_id":"p","method_id":"sample.Target.doubleValue(I)I"});
            let expected = dispatch(&mut engine, &project, tool, &original, &cancel).unwrap();
            for alias in ["method_signature", "signature"] {
                let mut args = original.clone();
                args[alias] = args.as_object_mut().unwrap().remove("method_id").unwrap();
                validate(tool, &args).unwrap();
                assert_eq!(
                    dispatch(&mut engine, &project, tool, &args, &cancel).unwrap(),
                    expected
                );
                args["method_id"] = json!("different");
                assert!(
                    validate(tool, &args)
                        .unwrap_err()
                        .to_string()
                        .contains("CONFLICTING_METHOD_SELECTORS")
                );
                args[alias] = Value::Null;
                assert!(validate(tool, &args).is_err());
            }
        }
        for tool in [
            "export_source",
            "resolve_reference",
            "resolve_switch_case_callers",
        ] {
            assert_eq!(
                aliases(tool, &json!({"signature":"example.m()V"})).unwrap()["method_id"],
                "example.m()V"
            );
        }
    }
    #[test]
    fn argument_aliases_normalize_lists_and_reject_conflicts() {
        let typed = aliases(
            "find_type_references",
            &json!({"type_name":"sample.Target","limit":5}),
        )
        .unwrap();
        assert_eq!(typed, json!({"class_name":"sample.Target","limit":5}));
        assert_eq!(
            aliases("find_string_references", &json!({"value":"token"})).unwrap()["query"],
            "token"
        );
        assert_eq!(
            aliases("find_config_key_consumers", &json!({"query":"api_key"})).unwrap()["key"],
            "api_key"
        );
        assert_eq!(
            aliases("search_code", &json!({"keyword":"decrypt"})).unwrap()["query"],
            "decrypt"
        );
        assert_eq!(
            aliases("get_dex_strings", &json!({"value":"http"})).unwrap()["query"],
            "http"
        );
        assert_eq!(
            aliases("find_integer_references", &json!({"value":42})).unwrap()["values"],
            json!([42])
        );
        assert_eq!(
            aliases("find_integer_references", &json!({"values":"0x10"})).unwrap()["values"],
            json!(["0x10"])
        );
        assert_eq!(
            aliases(
                "get_call_graph_batch",
                &json!({"method_id":"LandingActivity.m2"})
            )
            .unwrap()["method_ids"],
            json!(["LandingActivity.m2"])
        );
        assert_eq!(
            aliases("get_call_graph_batch", &json!({"signature":["a.B.c()V"]})).unwrap()["method_ids"],
            json!(["a.B.c()V"])
        );
        let same = aliases(
            "find_type_references",
            &json!({"type_name":"sample.Target","class_name":"sample.Target"}),
        )
        .unwrap();
        assert_eq!(same, json!({"class_name":"sample.Target"}));
        let conflict = aliases(
            "find_type_references",
            &json!({"type_name":"sample.Target","class_name":"sample.Other"}),
        )
        .unwrap_err()
        .to_string();
        assert!(conflict.contains("CONFLICTING_ARGUMENTS"), "{conflict}");
        assert!(conflict.contains("type_name"), "{conflict}");
        let conflict = aliases(
            "get_call_graph_batch",
            &json!({"method_id":"a.B.c()V","method_ids":["a.B.d()V"]}),
        )
        .unwrap_err()
        .to_string();
        assert!(
            conflict.contains("CONFLICTING_METHOD_SELECTORS"),
            "{conflict}"
        );
        assert!(
            aliases("find_string_references", &json!({"value":""}))
                .unwrap_err()
                .to_string()
                .contains("nonempty")
        );
        // Aliases are advertised, validated and explained when misspelled.
        let scoped = json!({"instance_id":"i","project_id":"p"});
        let mut args = scoped.clone();
        args["type_name"] = json!("sample.Target");
        validate("find_type_references", &args).unwrap();
        let mut args = scoped.clone();
        args["value"] = json!(7);
        validate("find_integer_references", &args).unwrap();
        let mut args = scoped.clone();
        args["method_id"] = json!("sample.Caller.helper()I");
        validate("get_call_graph_batch", &args).unwrap();
        let mut args = scoped.clone();
        args["typename"] = json!("sample.Target");
        let error = validate("find_type_references", &args)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Unknown argument: typename"), "{error}");
        assert!(
            error.contains("accepted arguments:") && error.contains("type_name"),
            "{error}"
        );
        let list = tools_list();
        let tools = list["tools"].as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            let properties = tool["inputSchema"]["properties"].as_object().unwrap();
            for (canonical, names) in argument_aliases(name) {
                assert!(properties.contains_key(canonical), "{name} {canonical}");
                for alias in names {
                    let description = properties[*alias]["description"].as_str().unwrap();
                    assert!(
                        description.starts_with(&format!("Alias for {canonical}")),
                        "{name} {alias}: {description}"
                    );
                }
            }
        }
    }
    #[test]
    fn short_method_selectors_resolve_to_unique_declarations() {
        let mut engine = NativeEngine::default();
        let project = engine
            .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
            .unwrap();
        let cancel = AtomicBool::new(false);
        let source = dispatch(
            &mut engine,
            &project,
            "get_method_source",
            &json!({"method_id":"Caller.helper"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(source["method_id"], "sample.Caller.helper()I");
        assert_eq!(source["requested_method_id"], "Caller.helper");
        let selection = &source["method_selection"];
        assert_eq!(selection["status"], "unique_candidate");
        assert_eq!(selection["owner_qualified"], true);
        assert_eq!(selection["parameter"], "method_id");
        assert_eq!(selection["method_id"], "sample.Caller.helper()I");
        assert!(
            selection["scope"]
                .as_str()
                .unwrap()
                .contains("superclass chain")
        );
        let exact = dispatch(
            &mut engine,
            &project,
            "get_method_source",
            &json!({"method_id":"sample.Caller.helper()I"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(exact["method_selection"]["status"], "exact");
        assert_eq!(exact["method_selection"]["owner_qualified"], false);
        assert_eq!(exact["text"], source["text"]);
        let ambiguous = dispatch(
            &mut engine,
            &project,
            "get_method_source",
            &json!({"method_id":"Target.doubleValue"}),
            &cancel,
        )
        .unwrap_err()
        .to_string();
        assert!(ambiguous.contains("AMBIGUOUS_METHOD"), "{ambiguous}");
        assert!(
            ambiguous.contains("sample.Target.doubleValue(I)I"),
            "{ambiguous}"
        );
        assert!(
            ambiguous.contains("sample.Target.doubleValue(Ljava/lang/String;)Ljava/lang/String;"),
            "{ambiguous}"
        );
        let narrowed = dispatch(
            &mut engine,
            &project,
            "get_method_source",
            &json!({"method_id":"Target.doubleValue(I)"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(narrowed["method_id"], "sample.Target.doubleValue(I)I");
        let constructor = dispatch(
            &mut engine,
            &project,
            "get_method_source",
            &json!({"method_id":"Lsample/Target;-><init>"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(constructor["method_id"], "sample.Target.<init>()V");
        assert_eq!(constructor["method_selection"]["owner_qualified"], false);
        for (selector, needle) in [
            ("Target.missing", "get_methods_of_class"),
            ("Nope.m", "SYMBOL_NOT_FOUND"),
            (
                "Target.doubleValue(Z)",
                "No declaration matches the supplied parameters",
            ),
        ] {
            let error = dispatch(
                &mut engine,
                &project,
                "get_method_source",
                &json!({"method_id":selector}),
                &cancel,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(needle), "{selector}: {error}");
        }
        let graph = dispatch(
            &mut engine,
            &project,
            "get_call_graph",
            &json!({"method_id":"Caller.compute"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(graph["root_method_id"], "sample.Caller.compute()I");
        assert_eq!(graph["method_id"], "sample.Caller.compute()I");
        assert_eq!(graph["requested_method_id"], "Caller.compute");
        assert_eq!(graph["method_selection"]["status"], "unique_candidate");
        let batch = dispatch(
            &mut engine,
            &project,
            "get_call_graph_batch",
            &json!({"method_ids":["Target.doubleValue","Caller.compute"]}),
            &cancel,
        )
        .unwrap();
        assert_eq!(batch["failed_queries"], 1);
        let results = batch["results"].as_array().unwrap();
        assert_eq!(results[0]["method_id"], "Target.doubleValue");
        let error = results[0]["error"].as_str().unwrap();
        assert!(error.contains("AMBIGUOUS_METHOD"), "{error}");
        assert!(error.contains("sample.Target.doubleValue(I)I"), "{error}");
        assert_eq!(results[1]["data"]["method_id"], "sample.Caller.compute()I");
        assert_eq!(
            results[1]["data"]["method_selection"]["status"],
            "unique_candidate"
        );
        let scalar = dispatch(
            &mut engine,
            &project,
            "get_call_graph_batch",
            &json!({"method_id":"Caller.compute"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(scalar["failed_queries"], 0);
        assert_eq!(
            scalar["results"][0]["data"]["root_method_id"],
            "sample.Caller.compute()I"
        );
        let reference = dispatch(
            &mut engine,
            &project,
            "resolve_reference",
            &json!({"class_id":"Caller","method_id":"Caller.helper","source_hash":exact["source_hash"],"source_offset":0}),
            &cancel,
        );
        assert!(
            reference
                .as_ref()
                .err()
                .is_none_or(|e| !e.to_string().contains("another class")),
            "{reference:?}"
        );
        let typed = dispatch(
            &mut engine,
            &project,
            "find_type_references",
            &json!({"type_name":"sample.Target"}),
            &cancel,
        )
        .unwrap();
        let canonical = dispatch(
            &mut engine,
            &project,
            "find_type_references",
            &json!({"class_name":"sample.Target"}),
            &cancel,
        )
        .unwrap();
        assert_eq!(typed["items"], canonical["items"]);
        assert_eq!(typed["total"], canonical["total"]);
        assert!(canonical["total"].as_u64().unwrap() >= 1);
    }
    #[test]
    fn bounded_component_pages_keep_records_and_filters_exact() {
        let records: Vec<_> = (0..12).map(|i| json!({"name":format!("example.C{i}"),"exported_state":{"effective":match i%3 {0=>json!(true),1=>json!(false),_=>Value::Null}},"details":"x".repeat(430)})).collect();
        for (filter, count) in [("true", 4), ("false", 4), ("unresolved", 4)] {
            let mut args = json!({"exported_state":filter,"max_bytes":1024});
            let mut names = Vec::new();
            let mut hash = None;
            loop {
                let result = component_page(records.clone(), &args).unwrap();
                assert_eq!(result["total"], count);
                assert!(serde_json::to_vec(&result["items"]).unwrap().len() <= 1024);
                if let Some(hash) = &hash {
                    assert_eq!(&result["source_hash"], hash);
                }
                hash = Some(result["source_hash"].clone());
                names.extend(
                    result["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v["name"].clone()),
                );
                if result["next_offset"].is_null() {
                    break;
                }
                args["offset"] = result["next_offset"].clone();
            }
            assert_eq!(names.len(), count);
        }
        let large = vec![json!({"name":"Huge","details":"x".repeat(4096)})];
        assert!(
            component_page(large.clone(), &json!({"max_bytes":1024}))
                .unwrap_err()
                .to_string()
                .contains("MANIFEST_RECORD_TOO_LARGE")
        );
        assert_eq!(
            component_page(large, &json!({"max_bytes":1024,"fields":"name"})).unwrap()["items"],
            json!([{"name":"Huge"}])
        );
    }
    #[test]
    fn caller_status_distinguishes_unchecked_and_fallback() {
        let mut result = json!({"call_sites":[{}, {"source_mapping":{"reconstruction_status":"dex-fallback","fallback_reason":"unsupported flow"}}]});
        caller_status(&mut result);
        assert_eq!(
            result["call_sites"][0]["caller_reconstruction"],
            "not_attempted"
        );
        assert_eq!(
            result["call_sites"][1]["caller_reconstruction"],
            "dex-fallback"
        );
        assert_eq!(
            result["call_sites"][1]["source_mapping"]["fallback_reason"],
            "unsupported flow"
        );
    }
}
