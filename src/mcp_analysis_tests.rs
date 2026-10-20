use super::*;

#[test]
fn class_aliases_validate_and_reject_conflicting_resolutions() {
    let mut engine = NativeEngine::default();
    let project = engine
        .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
        .unwrap();
    let scope = json!({"instance_id":"instance","project_id":"project","class_name":"Target"});
    validate("get_class_source", &scope).unwrap();
    let cancel = AtomicBool::new(false);
    let source = dispatch(&mut engine, &project, "get_class_source", &scope, &cancel).unwrap();
    assert_eq!(source["class_id"], "sample.Target");
    assert_eq!(source["coverage"]["omitted_methods"], json!([]));
    assert_eq!(source["full_text_in_response"], true);
    let mut both = scope.clone();
    both["class_id"] = json!("sample.Target");
    assert!(dispatch(&mut engine, &project, "get_class_source", &both, &cancel).is_ok());
    both["class_name"] = json!(
        project
            .classes
            .iter()
            .find(|c| c.as_str() != "sample.Target")
            .unwrap()
    );
    assert!(
        dispatch(&mut engine, &project, "get_class_source", &both, &cancel)
            .unwrap_err()
            .to_string()
            .contains("CONFLICTING_CLASS_SELECTORS")
    );
    for bad in [json!(null), json!(""), json!(4)] {
        both["class_name"] = bad;
        assert!(validate("get_class_source", &both).is_err());
    }
    assert!(
        validate(
            "get_class_source",
            &json!({"instance_id":"i","project_id":"p"})
        )
        .unwrap_err()
        .to_string()
        .contains("class_id or class_name")
    );
}

#[test]
fn pages_expose_full_document_boundary_and_preserve_coverage() {
    let full = "é🙂a\nb\n";
    let first = text_page(full.into(), &json!({"limit":2}), "java").unwrap();
    assert_eq!(first["text"], "é🙂");
    assert_eq!(first["has_more"], true);
    assert_eq!(first["full_text_in_response"], false);
    assert_eq!(first["next_offset"], 2);
    let tail = text_page(full.into(), &json!({"offset":2,"limit":20}), "java").unwrap();
    assert_eq!(tail["has_more"], false);
    assert_eq!(tail["full_text_in_response"], false);
    assert_eq!(first["source_hash"], tail["source_hash"]);
    let complete = text_page(full.into(), &json!({}), "java").unwrap();
    assert_eq!(complete["full_text_in_response"], true);
}

#[test]
fn legacy_selection_errors_are_diagnostic_not_project_failures() {
    let legacy = json!({"state":"Running","project_id":"same","error":"Unknown class: WebSocketSession$WebSocketConfig"});
    let fixed = normalize_instance_info(legacy);
    assert_eq!(fixed["error"], "");
    assert_eq!(fixed["project_id"], "same");
    assert_eq!(
        fixed["selection_diagnostic"]["requested_class_id"],
        "WebSocketSession$WebSocketConfig"
    );
    let failed = json!({"state":"Failed","error":"Unknown class: Broken"});
    assert_eq!(normalize_instance_info(failed.clone()), failed);
    let other = json!({"state":"Running","error":"Resource table unavailable"});
    assert_eq!(normalize_instance_info(other.clone()), other);
}

#[test]
fn analysis_endpoint_schemas_reject_invalid_offsets_and_filters() {
    let mut args = json!({"instance_id":"i","project_id":"p","caller_method_id":"sample.C.m()V","call_site_offset":12});
    validate("get_lambda_branch", &args).unwrap();
    for bad in [json!(-1), json!("12"), json!(null)] {
        args["call_site_offset"] = bad;
        assert!(validate("get_lambda_branch", &args).is_err());
    }
    let args = json!({"instance_id":"i","project_id":"p","field_id":"sample.C.x:I","access":"read","limit":500});
    validate("find_field_references", &args).unwrap();
    let mut bad = args.clone();
    bad["access"] = json!("execute");
    assert!(validate("find_field_references", &bad).is_err());
}

#[test]
#[ignore = "requires explicit RDX_ANALYSIS_APK and RDX_ANALYSIS_SHA256; serial local APK validation"]
fn pinned_airbnb_analysis_notes_regression() {
    let path = PathBuf::from(std::env::var("RDX_ANALYSIS_APK").expect("set RDX_ANALYSIS_APK"));
    let expected_hash = std::env::var("RDX_ANALYSIS_SHA256").expect("set RDX_ANALYSIS_SHA256");
    assert_eq!(digest(&path).unwrap(), expected_hash);
    let mut engine = NativeEngine::default();
    let project = engine.open(&path).unwrap();
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let full = dispatch(
        &mut engine,
        &project,
        "get_class_source",
        &json!({"class_name":"ec","limit":128000}),
        &cancel,
    )
    .unwrap();
    assert!(full["total_chars"].as_u64().unwrap() > 128000);
    assert_eq!(full["full_text_in_response"], false);
    assert_eq!(full["coverage"]["omitted_methods"], json!([]));
    let (code, coverage) = engine.decompile_class_source_report("ec", &cancel).unwrap();
    assert!(
        code.source.contains("ncq0"),
        "full class must retain media helper"
    );
    let field = engine
        .dex_class("ncq0")
        .unwrap()
        .fields
        .iter()
        .find(|f| f.name.as_ref() == "ı")
        .unwrap();
    let field_id = format!("ncq0.{}:{}", field.name, field.field_type);
    let refs = dispatch(
        &mut engine,
        &project,
        "find_field_references",
        &json!({"field_id":field_id,"access":"read","limit":500}),
        &cancel,
    )
    .unwrap();
    assert!(
        refs["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["method_id"].as_str().unwrap().starts_with("ec.")),
        "{refs}"
    );
    let xml = engine
        .read_resource_with_metadata("AndroidManifest.xml")
        .unwrap()
        .source;
    let receipt = crate::manifest_summary::query_components(
        &xml,
        &json!({"class_name":"ViewReceiptPdfActivity"}),
    )
    .unwrap();
    assert!(!receipt.is_empty());
    assert!(
        receipt
            .iter()
            .any(|r| r["exported_state"]["effective"] == false)
    );
    let classes: Vec<_> = project
        .classes
        .iter()
        .filter_map(|name| engine.dex_class(name))
        .collect();
    let ec = engine.dex_class("ec").unwrap();
    let create = ec
        .methods
        .iter()
        .find(|m| m.name.as_ref() == "create")
        .unwrap();
    let resolved = crate::lambda_branch::analyze(
        &classes,
        &crate::native_engine::disassembly::method_id(create),
        35,
        &|| false,
    );
    assert!(
        resolved["dispatchers"]
            .as_array()
            .is_some_and(|ds| ds.iter().any(|d| d["discriminator"] == 28)),
        "ec.create must map discriminator 28 to its dispatcher branch: {resolved}"
    );
    println!(
        "{}",
        json!({"apk_sha256":expected_hash,"ec_total_chars":full["total_chars"],"ec_coverage":coverage,
        "ncq0_reads":refs["total"],"ncq0_complete":refs["complete"],"receipt":receipt,
        "lambda_call_site_offset":resolved["call_site_offset"],"seconds":start.elapsed().as_secs_f64()})
    );
}
