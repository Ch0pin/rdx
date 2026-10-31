//! Cheap query feedback: no source rendering or additional indexes.
use super::*;

pub(super) fn method_aliases(name: &str, args: &Value) -> Result<Value> {
    let mut args = args.clone();
    if !matches!(
        name,
        "get_method_source"
            | "get_call_graph"
            | "export_source"
            | "resolve_reference"
            | "resolve_switch_case_callers"
    ) {
        return Ok(args);
    }
    let object = args
        .as_object_mut()
        .context("Arguments must be an object")?;
    for alias in ["method_signature", "signature"] {
        if let Some(value) = object.remove(alias) {
            ensure!(
                value.as_str().is_some_and(|s| !s.is_empty()),
                "{alias} must be a nonempty method signature"
            );
            if let Some(id) = object.get("method_id") {
                ensure!(
                    id == &value,
                    "CONFLICTING_METHOD_SELECTORS: method_id, method_signature and signature must agree"
                );
            } else {
                object.insert("method_id".into(), value);
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
    if let Some(filter) = args["exported_state"].as_str() {
        records.retain(|r| match filter {
            "true" => r["exported_state"]["effective"] == true,
            "false" => r["exported_state"]["effective"] == false,
            "unresolved" => r["exported_state"]["effective"].is_null(),
            _ => false,
        });
    }
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
                method_aliases(tool, &json!({"signature":"example.m()V"})).unwrap()["method_id"],
                "example.m()V"
            );
        }
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
