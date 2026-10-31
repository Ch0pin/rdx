use super::*;
#[test]
#[ignore = "requires RDX_HEY_APK for the build-248 obfuscated annotation regression"]
fn reviewer_hey_annotation_absence_diagnostic() {
    let path = PathBuf::from(std::env::var("RDX_HEY_APK").unwrap());
    let mut engine = NativeEngine::default();
    let project = engine.open(&path).unwrap();
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let absent = dispatch(
        &mut engine,
        &project,
        "find_annotation_usages",
        &json!({"annotation_type":"retrofit2.http.POST"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(absent["requested_type_present"], false);
    assert_eq!(absent["total"], 0);
    assert_eq!(absent["warnings"][0]["code"], "ANNOTATION_TYPE_ABSENT");
    let actual = dispatch(
        &mut engine,
        &project,
        "find_annotation_usages",
        &json!({"annotation_type":"qj.o", "limit":1}),
        &cancel,
    )
    .unwrap();
    assert_eq!(actual["requested_type_present"], true);
    assert!(actual["total"].as_u64().unwrap() > 0);
    assert_eq!(actual["warnings"], json!([]));
    println!(
        "annotation_presence_probe: {}",
        json!({
            "apk_sha256":digest(&path).unwrap(), "classes":project.classes.len(),
            "queries_ms": start.elapsed().as_millis(),
            "requested_original":absent, "actual_dex_annotation":actual
        })
    );
}

fn fixture() -> (NativeEngine, Project) {
    let mut engine = NativeEngine::default();
    let project = engine
        .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
        .unwrap();
    (engine, project)
}
#[test]
fn reviewer_selectors_exports_and_schema_contracts() {
    let (mut engine, project) = fixture();
    let cancel = AtomicBool::new(false);
    let args = json!({"instance_id":"i","project_id":"p","class_name":"Target","method_name":"doubleValue","descriptor":"(I)I"});
    validate("get_method_source", &args).unwrap();
    let resolved = review::member_selector(&engine, &project, &args, true).unwrap();
    assert_eq!(resolved, "sample.Target.doubleValue(I)I");
    let mut missing = args.clone();
    missing["method_name"] = json!("absent");
    assert!(
        review::member_selector(&engine, &project, &missing, true)
            .unwrap_err()
            .to_string()
            .contains("candidates=")
    );
    let source = dispatch(&mut engine, &project, "get_method_source", &args, &cancel).unwrap();
    assert_eq!(source["method_id"], resolved);
    let out = std::env::temp_dir().join(format!("rdx-review-export-{}", random_id().unwrap()));
    let mut export = args;
    export["out_path"] = json!(out);
    export["format"] = json!("dex");
    validate("export_source", &export).unwrap();
    let result = dispatch(&mut engine, &project, "export_source", &export, &cancel).unwrap();
    let text = fs::read_to_string(result["source_path"].as_str().unwrap()).unwrap();
    assert!(text.contains(".method sample.Target.doubleValue(I)I"));
    assert_eq!(
        digest(Path::new(result["source_path"].as_str().unwrap())).unwrap(),
        result["sha256"]
    );
    let coverage: Value =
        serde_json::from_slice(&fs::read(out.join("coverage.json")).unwrap()).unwrap();
    assert_eq!(coverage["coverage"]["reconstruction_attempted"], false);
    assert!(dispatch(&mut engine, &project, "export_source", &export, &cancel).is_err());
    assert_eq!(fs::read_to_string(out.join("source.txt")).unwrap(), text);
    fs::remove_dir_all(out).unwrap();
    for (tool, extra) in [
        (
            "find_annotation_usages",
            json!({"annotation_type":"retrofit2.http.GET","element":"parameter"}),
        ),
        (
            "get_resource_value",
            json!({"resource_name":"string/app_name"}),
        ),
        ("search_resources", json!({"query":"http"})),
        (
            "find_field_references",
            json!({"class_name":"Target","field_name":"value"}),
        ),
    ] {
        let mut scoped = extra;
        scoped["instance_id"] = json!("i");
        scoped["project_id"] = json!("p");
        validate(tool, &scoped).unwrap();
    }
    assert!(
        validate(
            "get_resource_value",
            &json!({"instance_id":"i","project_id":"p"})
        )
        .is_err()
    );
}
#[test]
fn reviewer_streaming_literals_match_complete_index_and_paginate() {
    let (engine, project) = fixture();
    let cancel = AtomicBool::new(false);
    let index = literals::Index::build(&engine, &project, &cancel, &AtomicUsize::new(0)).unwrap();
    let args = json!({"query":"","limit":500});
    let indexed = index.search(&args, &cancel).unwrap();
    let stream = scan::literals(&engine, &project, &args, &cancel).unwrap();
    assert_eq!(indexed["total"], stream["total"]);
    let filtered = scan::literals(
        &engine,
        &project,
        &json!({"query":"hello","package_prefix":"absent.package"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(filtered["scanned_project_classes"], 0);
    assert_eq!(filtered["project_classes"], project.classes.len());
    assert_eq!(filtered["scan_complete"], true);
    let key = |v: &Value| {
        format!(
            "{}|{}|{}|{}|{}",
            v["class_id"], v["method_id"], v["string_index"], v["code_unit_offset"], v["kind"]
        )
    };
    let mut a: Vec<_> = indexed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(key)
        .collect();
    let mut b: Vec<_> = stream["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(key)
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
    let mut offset = 0;
    let mut paged = vec![];
    loop {
        let page = scan::literals(
            &engine,
            &project,
            &json!({"query":"","limit":2,"offset":offset}),
            &cancel,
        )
        .unwrap();
        assert_eq!(page["source_hash"], stream["source_hash"]);
        paged.extend(page["items"].as_array().unwrap().clone());
        match page["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(paged, *stream["items"].as_array().unwrap());
    assert!(scan::literals(&engine, &project, &args, &AtomicBool::new(true)).is_err());
}
#[test]
#[ignore = "requires RDX_REVIEW_APK and RDX_REVIEW_OUTPUT; large real APK validation"]
fn reviewer_mercado_native_probe() {
    let apk = PathBuf::from(std::env::var("RDX_REVIEW_APK").unwrap());
    let output = PathBuf::from(std::env::var("RDX_REVIEW_OUTPUT").unwrap());
    fs::create_dir_all(&output).unwrap();
    let mut engine = NativeEngine::default();
    let start = Instant::now();
    let project = engine.open(&apk).unwrap();
    println!(
        "opened {} classes in {} seconds",
        project.classes.len(),
        start.elapsed().as_secs()
    );
    let engine = Arc::new(engine);
    let project = Arc::new(project);
    let cancel = AtomicBool::new(false);
    let controller = literals::Controller::new(
        engine.clone(),
        project.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    let mut results = json!({"apk_sha256":digest(&apk).unwrap(),"classes":project.classes.len()});
    for key in ["filename", "file_name"] {
        let t = Instant::now();
        let result = controller
            .search(
                &json!({"query":key,"match_mode":"exact","limit":5}),
                &cancel,
            )
            .unwrap();
        println!(
            "literal {key}: {} hits in {} ms, mode {}",
            result["total"],
            t.elapsed().as_millis(),
            result["search_mode"]
        );
        results[key] = json!({"result":result,"elapsed_ms":t.elapsed().as_millis()});
    }
    results["index_status"] = controller.status();
    results["annotations"] = review::annotations(
        &engine,
        &project,
        &json!({"annotation_type":"retrofit2.http.y","element":"parameter","limit":5}),
        &cancel,
    )
    .unwrap();
    results["resources"] =
        review::resources(&engine, &json!({"query":"http","limit":5}), true, &cancel).unwrap();
    fs::write(
        output.join("native-queries.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
    drop(controller);
    let mut engine = Arc::try_unwrap(engine).ok().unwrap();
    let builder_ids: Vec<_> = engine
        .dex_class("okhttp3.Request$Builder")
        .unwrap()
        .methods
        .iter()
        .filter(|m| {
            ["h", "removeHeader"].contains(&m.name.as_ref())
                && m.parameters.len() == 1
                && m.parameters[0].as_ref() == "Ljava/lang/String;"
        })
        .map(crate::native_engine::disassembly::method_id)
        .collect();
    for (n, id) in builder_ids.iter().enumerate() {
        let result = dispatch(&mut engine,&project,"get_call_graph",&json!({"method_id":id,"direction":"callers","projection":"call_sites","direct_only":true,"include_const_args":true,"limit":500}),&cancel).unwrap();
        fs::write(
            output.join(format!("call-arguments-{n}.json")),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("caller arguments {id}: {} sites", result["matching_total"]);
    }
    let exported=review::export(&mut engine,&project,
        &json!({"instance_id":"local-validation","project_id":"pinned-mercado","class_name":"com.mercadolibre.android.pdf_viewer.pdfviewer.presenter.c","out_path":output.join("presenter-export")}),
        &cancel,Some(json!({"apk_path":apk,"apk_sha256":digest(&apk).unwrap()}))).unwrap();
    println!(
        "full source export: {} bytes; response {} bytes",
        exported["size_bytes"],
        serde_json::to_vec(&exported).unwrap().len()
    );
    fs::write(
        output.join("export-result.json"),
        serde_json::to_vec_pretty(&exported).unwrap(),
    )
    .unwrap();
    let classes: Vec<_> = project
        .classes
        .iter()
        .filter(|n| n.contains("FeedRemoteDataSource") || n.ends_with("mediaplayer.network.a"))
        .cloned()
        .collect();
    let mut reports = vec![];
    for name in classes {
        let ids: Vec<_> = engine
            .dex_class(&name)
            .unwrap()
            .methods
            .iter()
            .filter(|m| {
                m.name.as_ref() == "resolveActionUrl" || ["a", "c"].contains(&m.name.as_ref())
            })
            .map(crate::native_engine::disassembly::method_id)
            .collect();
        for id in ids {
            let t = Instant::now();
            let report = engine
                .method_source(&id, crate::engine::MethodSourceFormat::Auto, &cancel)
                .unwrap();
            assert_eq!(report.format, "java", "{id}: {:?}", report.fallback_reason);
            println!(
                "method {id}: {} {:?} {}ms",
                report.format,
                report.fallback_reason,
                t.elapsed().as_millis()
            );
            let file = output.join(format!("method-{}.txt", reports.len()));
            fs::write(&file, &report.code.source).unwrap();
            reports.push(json!({"method_id":id,"format":report.format,"reason":report.fallback_reason,"source_path":file,"elapsed_ms":t.elapsed().as_millis()}));
        }
    }
    fs::write(
        output.join("method-recovery.json"),
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "requires RDX_REVIEW_APK and RDX_REVIEW_OUTPUT"]
fn reviewer_call_graph_navigation_and_forwarders_real_apk() {
    let apk = PathBuf::from(std::env::var_os("RDX_REVIEW_APK").unwrap());
    let output = PathBuf::from(std::env::var_os("RDX_REVIEW_OUTPUT").unwrap());
    fs::create_dir_all(&output).unwrap();
    let mut engine = NativeEngine::default();
    let project = engine.open(&apk).unwrap();
    let cancel = AtomicBool::new(false);
    let helper = "com.google.android.gms.internal.mlkit_vision_common.D3";
    let ids: Vec<_> = engine
        .dex_class(helper)
        .unwrap()
        .methods
        .iter()
        .filter(|m| m.name.as_ref() == "c")
        .map(crate::native_engine::disassembly::method_id)
        .collect();
    let proofs: Vec<_> = ids
        .iter()
        .filter_map(|id| navigation::cross_namespace_calls(&engine, id))
        .collect();
    fs::write(
        output.join("forwarders.json"),
        serde_json::to_vec_pretty(&proofs).unwrap(),
    )
    .unwrap();
    let proof = proofs
        .iter()
        .find(|p| {
            p["calls"].as_array().unwrap().iter().any(|c| {
                c["target_id"]
                    .as_str()
                    .unwrap()
                    .contains("native_reauth.shared.validation.b.a(")
            })
        })
        .expect("reviewed D3 cross-package helper");
    let id = proof["method_id"].as_str().unwrap();
    assert!(
        navigation::forwarder(&engine, id).is_none(),
        "D3 has guards and additional work, so it is not a pure forwarder"
    );
    let delegate = proof["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| {
            c["target_id"]
                .as_str()
                .unwrap()
                .contains("native_reauth.shared.validation.b.a(")
        })
        .unwrap()["target_id"]
        .clone();
    let args = json!({"method_id":id,"direction":"callers","projection":"call_sites","limit":500});
    let start = Instant::now();
    let cold = dispatch(&mut engine, &project, "get_call_graph", &args, &cancel).unwrap();
    let cold_ms = start.elapsed().as_millis();
    let start = Instant::now();
    let warm = dispatch(&mut engine, &project, "get_call_graph", &args, &cancel).unwrap();
    let warm_ms = start.elapsed().as_millis();
    assert_eq!(cold["call_sites"], warm["call_sites"]);
    assert_eq!(warm["invoke_index_cached"], true);
    assert!(warm["next_offset"].is_null());
    let mut expected = std::collections::BTreeSet::new();
    for name in &project.classes {
        for site in
            crate::native_xrefs::call_sites(engine.dex_class(name).unwrap(), None, &cancel).unwrap()
        {
            if site.callee == id {
                expected.insert((site.caller, site.pc as u64));
            }
        }
    }
    let actual: std::collections::BTreeSet<_> = warm["call_sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["caller_method_id"].as_str().unwrap().to_owned(),
                s["call_site_offset"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(actual, expected);
    assert!(!actual.is_empty());
    let mut enriched_args = args.clone();
    enriched_args["include_source_locations"] = json!(true);
    enriched_args["include_forwarders"] = json!(true);
    let enriched = dispatch(
        &mut engine,
        &project,
        "get_call_graph",
        &enriched_args,
        &cancel,
    )
    .unwrap();
    for site in enriched["call_sites"].as_array().unwrap() {
        assert!(site["forwarder"].is_null());
        assert_eq!(site["cross_namespace_calls"], *proof);
        assert!(site["source_mapping"]["status"].is_string());
    }
    let batch = dispatch(
        &mut engine,
        &project,
        "get_call_graph_batch",
        &json!({"method_ids":[id,delegate],"limit":500}),
        &cancel,
    )
    .unwrap();
    assert_eq!(batch["complete"], true);
    assert_eq!(batch["results"].as_array().unwrap().len(), 2);
    assert!(
        engine.invoke_index.status()["active_batches"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let owner = "com.mercadolibre.android.remedy.core.activities.KycWebViewActivity";
    let method =
        format!("{owner}.extendsPageConfig()Lcom/mercadolibre/android/mlwebkit/page/config/o;");
    let source = engine
        .method_source(&method, crate::engine::MethodSourceFormat::Auto, &cancel)
        .unwrap();
    let link = source
        .code
        .links
        .iter()
        .find(|l| l.label.contains("KycWebViewActivity$b"))
        .expect("nested class symbol");
    let resolved=dispatch(&mut engine,&project,"resolve_reference",&json!({"class_name":owner,"method_id":method,"source_hash":source.code.source_hash,"source_offset":link.start}),&cancel).unwrap();
    assert!(
        resolved["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["target_id"] == link.label)
    );
    let report = json!({"apk_sha256":digest(&apk).unwrap(),"classes":project.classes.len(),"cold_ms":cold_ms,"warm_ms":warm_ms,"independent_call_sites":expected.len(),"cold":cold,"warm":warm,"enriched":enriched,"batch":batch,"resolved_reference":resolved});
    fs::write(
        output.join("call-navigation.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!(
        "classes={}, cold={}ms warm={}ms, exact caller sites={}",
        project.classes.len(),
        cold_ms,
        warm_ms,
        expected.len()
    );
}

#[test]
fn discovery_tools_validate_and_dispatch_without_unknown_tools() {
    let (mut engine, project) = fixture();
    let cancel = AtomicBool::new(false);
    for (tool, extra) in [
        (
            "find_type_references",
            json!({"class_name":"java.lang.Object"}),
        ),
        ("search_kotlin_metadata", json!({"query":"Target"})),
        ("search_resource_xml", json!({"query":"manifest"})),
        ("list_nav_deeplinks", json!({})),
        (
            "find_layout_references",
            json!({"class_name":"android.webkit.WebView"}),
        ),
        ("find_webview_chooser_flows", json!({})),
    ] {
        let mut args = extra;
        args["instance_id"] = json!("i");
        args["project_id"] = json!("p");
        validate(tool, &args).unwrap();
        let result = dispatch(&mut engine, &project, tool, &args, &cancel).unwrap();
        assert!(result.is_object(), "{tool}");
    }
    let err = validate("get_method_source", &json!({"field":"x"})).unwrap_err();
    assert!(err.downcast_ref::<ArgumentError>().unwrap().details["accepted_parameters"]["properties"]["method_id"].is_object());
}
#[test]
fn exports_report_missing_parent_and_preserve_class_identity() {
    let (mut engine, project) = fixture();
    let cancel = AtomicBool::new(false);
    let root = std::env::temp_dir().join(format!("rdx-discovery-{}", random_id().unwrap()));
    let missing = root.join("missing").join("export");
    let error = dispatch(
        &mut engine,
        &project,
        "export_source",
        &json!({"class_name":"sample.Target","out_path":missing}),
        &cancel,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains(root.join("missing").to_str().unwrap())
    );
    let args =
        json!({"out_path":root,"package_prefix":"sample","instance_id":"i","project_id":"p"});
    validate("export_package_source", &args).unwrap();
    let result = dispatch(
        &mut engine,
        &project,
        "export_package_source",
        &args,
        &cancel,
    )
    .unwrap();
    assert_eq!(result["complete"], true);
    let text = fs::read_to_string(root.join("manifest.jsonl")).unwrap();
    let mut paths = std::collections::HashSet::new();
    for line in text.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let path = row["source_path"].as_str().unwrap();
        assert!(paths.insert(path.to_lowercase()));
        assert_eq!(digest(Path::new(path)).unwrap(), row["sha256"]);
        assert_eq!(
            row["descriptor"],
            format!("L{};", row["class_id"].as_str().unwrap().replace('.', "/"))
        );
    }
    assert!(
        dispatch(
            &mut engine,
            &project,
            "export_package_source",
            &args,
            &cancel
        )
        .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn case_insensitive_discovery_preserves_case_collisions() {
    let project = Project {
        classes: vec!["o.arp".into(), "o.arP".into(), "other.Z".into()],
        resources: vec![],
    };
    let exact = discovery::classes(&project, &json!({"query":"aRP"})).unwrap();
    assert_eq!(exact["total"], 0);
    assert_eq!(exact["suggestions"].as_array().unwrap().len(), 2);
    let folded =
        discovery::classes(&project, &json!({"query":"aRP","case_sensitive":false})).unwrap();
    assert_eq!(folded["total"], 2);
    assert_ne!(
        folded["items"][0]["class_id"],
        folded["items"][1]["class_id"]
    );
    assert!(resolve_class_id(&project, "o.aRP").is_err());
}
#[test]
#[ignore = "Local APK validation; set RDX_REVIEW_APK and RDX_REVIEW_OUTPUT"]
fn discovery_real_apk_probe() {
    let path = PathBuf::from(std::env::var("RDX_REVIEW_APK").unwrap());
    let out = PathBuf::from(std::env::var("RDX_REVIEW_OUTPUT").unwrap());
    let mut engine = NativeEngine::default();
    let start = Instant::now();
    let project = engine.open(&path).unwrap();
    let mut reports = json!({"apk":path,"sha256":digest(&path).unwrap(),"classes":project.classes.len(),"open_ms":start.elapsed().as_millis()});
    let cancel = AtomicBool::new(false);
    for (tool, args) in [
        (
            "find_type_references",
            json!({"class_name":"android.webkit.WebView","limit":10}),
        ),
        (
            "search_kotlin_metadata",
            json!({"query":"LinkParamHandler","limit":10}),
        ),
        (
            "search_resource_xml",
            json!({"query":"leads_bundle","limit":20}),
        ),
        ("list_nav_deeplinks", json!({"limit":500})),
        (
            "find_layout_references",
            json!({"class_name":"android.webkit.WebView","limit":20}),
        ),
        ("find_webview_chooser_flows", json!({"limit":100})),
    ] {
        let start = Instant::now();
        let result = dispatch(&mut engine, &project, tool, &args, &cancel).unwrap();
        reports[tool] = json!({"elapsed_ms":start.elapsed().as_millis(),"result":result});
        fs::write(&out, serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
    }
    assert!(
        reports["list_nav_deeplinks"]["result"]["total"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        reports["search_resource_xml"]["result"]["total"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        reports["search_kotlin_metadata"]["result"]["total"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        reports["find_webview_chooser_flows"]["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["sinks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["external_uri_may_flow"] == true))
    );
}

#[test]
fn xml_resource_paging_and_failed_file_coverage() {
    let dir = std::env::temp_dir().join(format!("rdx-xml-{}", random_id().unwrap()));
    fs::create_dir(&dir).unwrap();
    let apk = dir.join("xml.apk");
    let mut zip = zip::ZipWriter::new(File::create(&apk).unwrap());
    for (name, text) in [
        (
            "res/navigation/routes.xml",
            r#"<navigation xmlns:a="http://schemas.android.com/apk/res/android" xmlns:x="http://schemas.android.com/apk/res-auto"><fragment a:name="p.Screen"><deepLink x:uri="app://first"/><deepLink x:uri="app://second"/><argument a:name="leads_bundle" x:argType="string"/></fragment></navigation>"#,
        ),
        (
            "res/layout/views.xml",
            r#"<LinearLayout><view class="p.View"/><p.View/><WebView/></LinearLayout>"#,
        ),
        ("res/xml/broken.xml", "<broken>"),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.start_file("classes.dex", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(include_bytes!("../tests/fixtures/hello.dex"))
        .unwrap();
    zip.finish().unwrap();
    let mut engine = NativeEngine::default();
    let project = engine.open(&apk).unwrap();
    let cancel = AtomicBool::new(false);
    let first = xml_discovery::search(
        &mut engine,
        &project,
        "list_nav_deeplinks",
        &json!({"limit":1}),
        &cancel,
    )
    .unwrap();
    let second = xml_discovery::search(
        &mut engine,
        &project,
        "list_nav_deeplinks",
        &json!({"limit":1,"offset":1}),
        &cancel,
    )
    .unwrap();
    assert_eq!(first["total"], 2);
    assert_eq!(first["next_offset"], 1);
    assert_eq!(first["source_hash"], second["source_hash"]);
    assert_eq!(
        first["items"][0]["arguments"][0]["name"]["value"],
        "leads_bundle"
    );
    let xml = xml_discovery::search(
        &mut engine,
        &project,
        "search_resource_xml",
        &json!({"query":"leads_bundle"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(xml["total"], 1);
    assert_eq!(xml["complete"], false);
    assert_eq!(xml["failed_files"], 1);
    let layout = xml_discovery::search(
        &mut engine,
        &project,
        "find_layout_references",
        &json!({"class_name":"p.View"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(layout["total"], 2);
    assert_eq!(layout["items"][0]["runtime_instantiation_proven"], false);
    let platform = xml_discovery::search(
        &mut engine,
        &project,
        "find_layout_references",
        &json!({"class_name":"android.webkit.WebView"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(platform["total"], 1);
    assert_eq!(
        platform["items"][0]["resolution"],
        "possible_platform_inflater_prefix"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn assets_are_readable_searchable_and_binary_skips_are_explicit() {
    let dir = std::env::temp_dir().join(format!("rdx-assets-{}", random_id().unwrap()));
    fs::create_dir(&dir).unwrap();
    let apk = dir.join("assets.apk");
    let mut zip = zip::ZipWriter::new(File::create(&apk).unwrap());
    for (path, bytes) in [
        (
            "classes.dex",
            include_bytes!("../tests/fixtures/hello.dex").as_slice(),
        ),
        (
            "assets/json/android_v10.json",
            b"{\"path-configuration\":true}".as_slice(),
        ),
        ("assets/other.txt", b"PATH-CONFIGURATION".as_slice()),
        ("assets/binary.bin", b"\0binary".as_slice()),
    ] {
        zip.start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let mut engine = NativeEngine::default();
    let project = engine.open(&apk).unwrap();
    let cancel = AtomicBool::new(false);
    let args =
        json!({"query":"path-configuration","scope":"assets","case_sensitive":false,"limit":1});
    let first = dispatch(&mut engine, &project, "search_resources", &args, &cancel).unwrap();
    assert_eq!(first["total"], 2);
    assert_eq!(first["complete"], false);
    assert_eq!(first["coverage"]["files_skipped"], 1);
    let mut next = args.clone();
    next["offset"] = json!(1);
    let second = dispatch(&mut engine, &project, "search_resources", &next, &cancel).unwrap();
    assert_eq!(first["source_hash"], second["source_hash"]);
    assert_ne!(first["items"], second["items"]);
    let read = dispatch(
        &mut engine,
        &project,
        "get_resource_file",
        &json!({"resource_path":"assets/json/android_v10.json"}),
        &cancel,
    )
    .unwrap();
    assert!(
        read["text"]
            .as_str()
            .unwrap()
            .contains("path-configuration")
    );
    assert!(
        engine
            .read_resource_with_metadata("assets/binary.bin")
            .is_err()
    );
    let reverse = dispatch(
        &mut engine,
        &project,
        "find_instantiations",
        &json!({"allocating_class":"Hello"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(reverse["allocating_class"], "sample.Hello");
    assert!(reverse["definition_available"].is_null());
    for a in [
        json!({"allocating_class":"Hello"}),
        json!({"allocating_method_id":"Hello.answer()I"}),
    ] {
        let mut a = a;
        a["instance_id"] = json!("i");
        a["project_id"] = json!("p");
        validate("find_instantiations", &a).unwrap();
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn scan_and_reverse_case_schemas_validate_exact_selectors() {
    for case in [json!(23), json!(-1)] {
        validate(
            "resolve_switch_case_callers",
            &json!({"instance_id":"i","project_id":"p","class_name":"je.j","case":case}),
        )
        .unwrap();
    }
    for bad in [json!(2147483648i64), json!("23"), json!(null)] {
        assert!(
            validate(
                "resolve_switch_case_callers",
                &json!({"instance_id":"i","project_id":"p","class_name":"je.j","case":bad})
            )
            .is_err()
        );
    }
    validate("search_code",&json!({"instance_id":"i","project_id":"p","mode":"identifiers","query":"delete","projection":"summary","search_mode":"scan"})).unwrap();
    assert!(
        validate(
            "search_code",
            &json!({"instance_id":"i","project_id":"p","query":"delete","projection":"summary"})
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires RDX_BASECAMP_APK"]
fn basecamp_reverse_lambda_case_and_referenced_root() {
    let path = std::env::var_os("RDX_BASECAMP_APK").unwrap();
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(Path::new(&path)).unwrap();
    let cancel = false.into();
    let root = dispatch(
        &mut engine,
        &project,
        "find_implementations",
        &json!({"class_name":"javax.net.ssl.X509TrustManager"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(root["total"], 0);
    assert_eq!(root["referenced_type"], true);
    assert_eq!(root["declared_hierarchy_edge"], false);
    let mut args = json!({"class_name":"je.j","case":23,"limit":100});
    let mut sites = vec![];
    let mut unresolved = vec![];
    let start = Instant::now();
    loop {
        let result = dispatch(
            &mut engine,
            &project,
            "resolve_switch_case_callers",
            &args,
            &cancel,
        )
        .unwrap();
        sites.extend(result["items"].as_array().unwrap().iter().cloned());
        unresolved.extend(
            result["unresolved_sites"]
                .as_array()
                .unwrap()
                .iter()
                .cloned(),
        );
        if result["scan_complete"] == true {
            break;
        }
        args["resume_cursor"] = result["next_cursor"].clone();
        assert!(start.elapsed() < Duration::from_secs(90));
    }
    eprintln!(
        "case 23: {} ms; matches={sites:?}; unresolved={unresolved:?}",
        start.elapsed().as_millis()
    );
    assert!(
        sites
            .iter()
            .any(|s| s["caller_method_id"].as_str().unwrap().starts_with("js.b."))
    );
    assert!(unresolved.is_empty(), "{unresolved:?}");
}

#[test]
#[ignore = "requires RDX_BASECAMP_APK"]
fn basecamp_relocated_network_stacks() {
    let path = std::env::var_os("RDX_BASECAMP_APK").unwrap();
    let mut engine = NativeEngine::start().unwrap();
    let project = engine.open(Path::new(&path)).unwrap();
    let args = json!({"package_prefix":"hu", "limit":100});
    let start = Instant::now();
    let result = dispatch(
        &mut engine,
        &project,
        "find_network_stacks",
        &args,
        &false.into(),
    )
    .unwrap();
    eprintln!(
        "network scan: {} ms; {}",
        start.elapsed().as_millis(),
        result
    );
    assert_eq!(result["coverage"]["complete"], true);
    let found = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["hook_map"]["interceptor_method_id"] == "hu.b.a(Lku/g;)Leu/o0;")
        .expect("relocated bridge interceptor");
    assert_eq!(
        found["hook_map"]["request_from_chain_field_id"],
        "ku.g.e:Leu/i0;"
    );
    assert_eq!(found["hook_map"]["request_url_field_id"], "eu.i0.a:Leu/z;");
    assert_eq!(
        found["hook_map"]["header_pairs_field_id"],
        "eu.x.a:[Ljava/lang/String;"
    );
    // The real getters use a bounded helper and a throwing null path. Preserve
    // that unresolved method-level proof, alongside the available array field.
    assert_eq!(found["hook_map"]["header_name_indexing_proven"], false);
    assert_eq!(found["unresolved_roles"], json!(["header_name_accessor"]));
    assert_eq!(found["runtime_verified"], false);
    assert_eq!(result["runtime_values_included"], false);
    assert!(!result.to_string().contains("Authorization:"));
    let all = dispatch(
        &mut engine,
        &project,
        "find_network_stacks",
        &json!({"limit":100}),
        &false.into(),
    )
    .unwrap();
    assert_eq!(all["coverage"]["complete"], true);
    assert!(all["total"].as_u64().unwrap() >= result["total"].as_u64().unwrap());
    assert!(all["items"].as_array().unwrap().iter().any(|r| r == found));
    eprintln!("app-wide network candidates: {}", all["total"]);

    // The alternate constructor fixes case three independently of its int argument.
    for (id, pc) in [
        ("je.j.create(Ljava/lang/Object;Lrp/c;)Lrp/c;", 411),
        (
            "k6.b.invokeSuspend(Ljava/lang/Object;)Ljava/lang/Object;",
            217,
        ),
    ] {
        let proof = dispatch(
            &mut engine,
            &project,
            "get_lambda_branch",
            &json!({"caller_method_id":id,"call_site_offset":pc}),
            &false.into(),
        )
        .unwrap();
        assert_eq!(proof["status"], "resolved", "{proof}");
        for d in proof["dispatchers"].as_array().unwrap() {
            assert_eq!(d["discriminator"], 3);
            assert_eq!(d["discriminator_source"], "constructor_constant");
            assert!(d["constructor_argument_word"].is_null());
        }
    }
}

#[test]
fn network_detector_schema_dispatch_and_limits() {
    let (mut engine, project) = fixture();
    validate(
        "find_network_stacks",
        &json!({"instance_id":"i","project_id":"p","package_prefix":"sample","limit":100}),
    )
    .unwrap();
    assert!(
        validate(
            "find_network_stacks",
            &json!({"instance_id":"i","project_id":"p","limit":101})
        )
        .is_err()
    );
    let result = dispatch(
        &mut engine,
        &project,
        "find_network_stacks",
        &json!({}),
        &false.into(),
    )
    .unwrap();
    assert_eq!(result["coverage"]["complete"], true);
    assert_eq!(result["total"], 0);
}

#[test]
fn manifest_launch_projection_errors_list_valid_fields() {
    let args = json!({"instance_id":"i","project_id":"p","fields":"name,launchMode,exported,taskAffinity,allowTaskReparenting,documentLaunchMode,excludeFromRecents,permission,targetActivity"});
    validate("get_manifest_components", &args).unwrap();
    let mut invalid = args;
    invalid["fields"] = json!("name,notAField");
    let error = validate("get_manifest_components", &invalid).unwrap_err();
    let error = error.downcast_ref::<ArgumentError>().unwrap();
    assert_eq!(error.details["invalid_fields"], json!(["notAField"]));
    assert!(
        error.details["allowed_fields"]
            .as_array()
            .unwrap()
            .contains(&json!("launchMode"))
    );
}

#[test]
#[ignore = "requires RDX_COSTCO_APK; static detector/projection regression only"]
fn reviewer_costco_launch_projection_and_network_usage() {
    let path = PathBuf::from(std::env::var("RDX_COSTCO_APK").unwrap());
    let mut engine = NativeEngine::default();
    let project = engine.open(&path).unwrap();
    let xml = engine
        .read_resource_with_metadata("AndroidManifest.xml")
        .unwrap()
        .source;
    let args = json!({"instance_id":"i","project_id":"p","component_type":"activity","class_name":"com.costco.app.android.ui.main.MainActivity","match_mode":"exact","fields":"name,launchMode,exported,taskAffinity"});
    validate("get_manifest_components", &args).unwrap();
    let records =
        crate::manifest_summary::query_resolved(&xml, &args, engine.resource_table(), true)
            .unwrap();
    let components = feedback::component_page(records, &args).unwrap();
    assert_eq!(components["total"], 1);
    assert!(components["items"][0]["launchMode"].is_object());
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let mut query = json!({"package_prefix":"com.costco","limit":100});
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut roles = std::collections::BTreeMap::<String, usize>::new();
    let mut total = 0;
    let mut pages = 0;
    let coverage = loop {
        let page = crate::network_stacks::search(&engine, &project, &query, &cancel).unwrap();
        pages += 1;
        for row in page["items"].as_array().unwrap() {
            total += 1;
            *counts
                .entry(row["detector"].as_str().unwrap().to_owned())
                .or_default() += 1;
            if let Some(role) = row["role"].as_str() {
                *roles.entry(role.to_owned()).or_default() += 1;
            }
        }
        if page["next_offset"].is_null() {
            break page["coverage"].clone();
        }
        assert!(pages < 30, "unexpected detector page volume");
        query["offset"] = page["next_offset"].clone();
    };
    let factory_calls: Vec<_> = project
        .classes
        .iter()
        .filter(|n| n.starts_with("com.costco") && n.contains("RetrofitServiceImpl"))
        .flat_map(|n| {
            let class = engine.dex_class(n).unwrap();
            class
                .methods
                .iter()
                .flat_map(|m| {
                    crate::native_xrefs::method_call_sites(class, m, &cancel).unwrap_or_default()
                })
                .filter(|site| {
                    site.callee.starts_with("okhttp3.") || site.callee.starts_with("retrofit2.")
                })
                .map(|site| json!({"caller":site.caller,"callee":site.callee,"offset":site.pc}))
                .collect::<Vec<_>>()
        })
        .collect();
    let factory_helpers:Vec<_>=project.classes.iter().filter(|n|n.starts_with("com.costco") && n.ends_with(".RetrofitServiceImpl"))
        .flat_map(|n|engine.dex_class(n).unwrap().methods.iter().filter(|m|m.name.as_ref()=="allowAllCerts")
            .map(|m|json!({"method_id":crate::native_engine::disassembly::method_id(m),"instructions":m.code.as_ref().map(|c|c.instructions.clone())})).collect::<Vec<_>>()).collect();
    let report = json!({"apk_sha256":digest(&path).unwrap(),"loaded_classes":project.classes.len(),"manifest_projection":components,
        "factory_calls":factory_calls,"factory_helpers":factory_helpers,"network":{"records":total,"pages":pages,"detectors":counts,"roles":roles,"coverage":coverage,"elapsed_ms":started.elapsed().as_millis()}});
    if let Ok(out) = std::env::var("RDX_VALIDATION_OUTPUT") {
        fs::write(out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    println!("costco_manifest_network_probe: {report}");
    assert!(total > 0);
    assert!(counts.get("okhttp_contracts").copied().unwrap_or(0) > 0);
    assert!(counts.get("retrofit_annotations").copied().unwrap_or(0) > 0);
    assert!(
        roles
            .get("certificate_pinner_configuration")
            .copied()
            .unwrap_or(0)
            > 0
    );
}
