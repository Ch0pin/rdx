//! Selected-method representation modes preserve lookup, paging, and fallback meaning.
use super::*;
use crate::engine::MethodSourceFormat;

struct FormatFixtureDirectory(PathBuf);
impl Drop for FormatFixtureDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn opened() -> (NativeEngine, Project) {
    let mut engine = NativeEngine::default();
    let project = engine
        .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
        .unwrap();
    (engine, project)
}

#[test]
fn method_source_format_schema_accepts_only_auto_and_dex() {
    let list = tools_list();
    let schema = &list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "get_method_source")
        .unwrap()["inputSchema"];
    assert_eq!(
        schema["properties"]["format"]["enum"],
        json!(["auto", "dex"])
    );
    assert_eq!(schema["properties"]["format"]["default"], "auto");
    let args =
        json!({"instance_id":"i","project_id":"p","method_id":"sample.Target.doubleValue(I)I"});
    assert!(validate("get_method_source", &args).is_ok());
    for value in [
        json!("auto"),
        json!("dex"),
        json!("java"),
        json!("rdx-dex"),
        json!(""),
        json!(true),
        Value::Null,
    ] {
        let mut args = args.clone();
        args["format"] = value.clone();
        let valid = matches!(value.as_str(), Some("auto" | "dex"));
        assert_eq!(
            validate("get_method_source", &args).is_ok(),
            valid,
            "{value}"
        );
    }
}

#[test]
fn method_source_forced_dex_is_one_raw_method_with_stable_paging_and_lookup() {
    let (mut engine, project) = opened();
    let cancel = AtomicBool::new(false);
    let id = "sample.Target.doubleValue(I)I";
    let auto = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id}),
        &cancel,
    )
    .unwrap();
    let explicit = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id,"format":"auto"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(auto, explicit);
    assert_eq!(auto["format"], "java");
    assert_eq!(auto["requested_format"], "auto");
    assert_eq!(auto["reconstruction_attempted"], true);
    assert!(auto["fallback_reason"].is_null());
    let forced = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id,"format":"dex"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(forced["format"], "rdx-dex");
    assert_eq!(forced["requested_format"], "dex");
    assert_eq!(forced["reconstruction_attempted"], false);
    assert!(forced["fallback_reason"].is_null());
    assert!(forced.get("decompile_failed").is_none());
    let text = forced["text"].as_str().unwrap();
    assert!(
        text.trim_start_matches('\n')
            .starts_with(&format!(".method {id}")),
        "{text}"
    );
    assert_eq!(text.matches(".method ").count(), 1);
    assert_eq!(text.matches(".end method").count(), 1);
    assert!(!text.contains("reconstruction failed"));
    assert_eq!(forced["source_hash"], crate::engine::source_identity(text));
    let class = engine.dex_class("sample.Target").unwrap();
    let method = crate::method_lookup::resolve(class, id).unwrap();
    let expected = crate::native_engine::disassembly::render_method(class, method);
    assert_eq!(text, expected.source);
    let mut assembled = String::new();
    let mut offset = 0;
    loop {
        let page = dispatch(
            &mut engine,
            &project,
            "get_method_source",
            &json!({"method_id":id,"format":"dex","offset":offset,"limit":17}),
            &cancel,
        )
        .unwrap();
        for key in [
            "format",
            "requested_format",
            "reconstruction_attempted",
            "fallback_reason",
            "source_hash",
            "method_id",
            "total_chars",
        ] {
            assert_eq!(page[key], forced[key], "{key}");
        }
        assembled.push_str(page["text"].as_str().unwrap());
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        offset = next;
    }
    assert_eq!(assembled, text);
    let alias = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":"Lsample/Target;->doubleValue(I)I","format":"dex"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(alias["text"], forced["text"]);
    assert_eq!(alias["method_id"], id);
    assert_eq!(
        alias["requested_method_id"],
        "Lsample/Target;->doubleValue(I)I"
    );
    for format in [MethodSourceFormat::Auto, MethodSourceFormat::Dex] {
        let report = engine.method_source(id, format, &cancel).unwrap();
        let length = report.code.source.chars().count();
        assert_eq!(
            report.code.source_hash,
            crate::engine::source_identity(&report.code.source)
        );
        assert!(
            report
                .code
                .links
                .iter()
                .all(|link| link.start < link.end && link.end <= length)
        );
        assert!(
            report
                .code
                .definitions
                .iter()
                .all(|definition| definition.start < definition.end && definition.end <= length)
        );
        assert_eq!(
            engine
                .method_source(id, format, &AtomicBool::new(true))
                .unwrap_err()
                .to_string(),
            "CANCELLED"
        );
        let error = engine
            .method_source("sample.Target.doubleValue(I)J", format, &cancel)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(id) && error.contains("candidates"),
            "{error}"
        );
    }
    for value in [json!("java"), json!(true), Value::Null] {
        assert!(
            dispatch(
                &mut engine,
                &project,
                "get_method_source",
                &json!({"method_id":id,"format":value}),
                &cancel
            )
            .is_err()
        );
    }
    let legacy = engine.decompile_method_source(id, &cancel).unwrap();
    assert_eq!(legacy.0.source, auto["text"]);
    assert!(legacy.1.is_none());
}

#[test]
fn method_source_requested_disassembly_is_distinct_from_failed_reconstruction() {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(
        include_bytes!("../tests/fixtures/navigation.apk").as_slice(),
    ))
    .unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_name("classes.dex").unwrap(), &mut bytes).unwrap();
    let dex = crate::native_dex::parse(&bytes).unwrap();
    let (_class, method) =
        dex.classes
            .iter()
            .find_map(|class| {
                class
                    .methods
                    .iter()
                    .find(|method| {
                        method.return_type.as_ref() == "V"
                            && method.code.as_ref().is_some_and(|code| {
                                code.registers > 0 && code.instructions.len() >= 2
                            })
                    })
                    .map(|method| (class, method))
            })
            .unwrap();
    let id = crate::native_engine::disassembly::method_id(method);
    let code = method.code.as_ref().unwrap();
    let words = code.instructions.len();
    let start = code.offset as usize + 16;
    bytes[start..start + words * 2].fill(0);
    bytes[start..start + 2].copy_from_slice(&0x001du16.to_le_bytes()); // unmatched monitor-enter
    bytes[start + (words - 1) * 2..start + words * 2].copy_from_slice(&0x000eu16.to_le_bytes());
    let directory = FormatFixtureDirectory(
        std::env::temp_dir().join(format!("rdx-method-format-{}", random_id().unwrap())),
    );
    fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("fallback.dex");
    fs::write(&path, &bytes).unwrap();
    let mut engine = NativeEngine::default();
    let project = engine.open(&path).unwrap();
    let cancel = AtomicBool::new(false);
    let auto = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id}),
        &cancel,
    )
    .unwrap();
    assert_eq!(auto["format"], "rdx-dex");
    assert_eq!(auto["requested_format"], "auto");
    assert_eq!(auto["reconstruction_attempted"], true);
    assert!(
        auto["fallback_reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty())
    );
    assert!(
        auto["text"]
            .as_str()
            .unwrap()
            .starts_with("// RDX Java reconstruction failed:")
    );
    let requested = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id,"format":"dex"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(requested["format"], "rdx-dex");
    assert_eq!(requested["reconstruction_attempted"], false);
    assert!(requested["fallback_reason"].is_null());
    let raw = requested["text"].as_str().unwrap();
    assert!(raw.trim_start_matches('\n').starts_with(".method "));
    let owner = crate::method_lookup::owner(&id).unwrap();
    let class = engine.dex_class(owner).unwrap();
    let method = crate::method_lookup::resolve(class, &id).unwrap();
    let expected = crate::native_engine::disassembly::render_method(class, method);
    assert_eq!(raw, expected.source);
    assert!(raw.contains("monitor-enter"));
    assert!(!raw.contains("reconstruction failed"));
    assert_eq!(
        requested["source_hash"],
        crate::engine::source_identity(raw)
    );
    assert_ne!(requested["source_hash"], auto["source_hash"]);
    assert!(auto["text"].as_str().unwrap().ends_with(raw));
    let legacy = engine.decompile_method_source(&id, &cancel).unwrap();
    assert_eq!(legacy.0.source, auto["text"]);
    assert!(legacy.1.is_some());
    drop(engine);
    drop(directory);
    assert!(!path.exists(), "temporary DEX fixture must be removed");
}

#[test]
fn method_source_call_sites_carry_dex_offsets_registers_and_text_positions() {
    let (mut engine, project) = opened();
    let cancel = AtomicBool::new(false);
    let id = "sample.Caller.compute()I";
    let plain = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id}),
        &cancel,
    )
    .unwrap();
    assert!(plain.get("call_sites").is_none());
    let java = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id,"include_call_sites":true}),
        &cancel,
    )
    .unwrap();
    assert_eq!(java["text"], plain["text"]);
    assert_eq!(java["source_hash"], plain["source_hash"]);
    let table = &java["call_sites"];
    assert_eq!(table["total"], 2);
    assert_eq!(table["truncated"], false);
    assert!(
        table["offset_unit"]
            .as_str()
            .unwrap()
            .contains("16-bit DEX code units")
    );
    let items = table["items"].as_array().unwrap();
    assert_eq!(items[0]["callee_method_id"], "sample.Target.<init>()V");
    assert_eq!(items[0]["dispatch"], "direct");
    assert_eq!(items[0]["constant_arguments"]["status"], "analyzed");
    assert_eq!(items[0]["constant_arguments"]["arguments"], json!([]));
    let call = &items[1];
    assert_eq!(call["callee_method_id"], "sample.Target.doubleValue(I)I");
    assert_eq!(call["call_site_offset"], 7);
    assert_eq!(call["call_site_offset_hex"], "0x0007");
    assert_eq!(call["dispatch"], "virtual (declared target)");
    let argument = &call["constant_arguments"]["arguments"][0];
    assert_eq!(argument["argument_index"], 0);
    assert_eq!(argument["register"], 1);
    assert_eq!(argument["descriptor"], "I");
    assert_eq!(argument["status"], "constant");
    assert_eq!(argument["kind"], "bits32");
    assert_eq!(argument["value"], 21);
    let mapping = &call["source_mapping"];
    assert_eq!(mapping["status"], "unique_target_reference");
    assert_eq!(mapping["reconstruction_status"], "decompiled");
    assert_eq!(mapping["source_hash"], java["source_hash"]);
    assert_eq!(mapping["location"]["source_hash"], java["source_hash"]);
    let line = mapping["location"]["line"].as_u64().unwrap() as usize;
    let column = mapping["location"]["column"].as_u64().unwrap() as usize;
    let text_line = java["text"]
        .as_str()
        .unwrap()
        .lines()
        .nth(line - 1)
        .unwrap();
    let at_column: String = text_line.chars().skip(column - 1).collect();
    assert!(at_column.starts_with("doubleValue(21)"), "{text_line}");
    // The same offsets identify the sites in the call-graph projection.
    let graph = dispatch(
        &mut engine,
        &project,
        "get_call_graph",
        &json!({"method_id":id,"projection":"call_sites"}),
        &cancel,
    )
    .unwrap();
    let graph_offsets: Vec<_> = graph["call_sites"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|site| site["callee_method_id"] == "sample.Target.doubleValue(I)I")
        .map(|site| site["call_site_offset"].clone())
        .collect();
    assert_eq!(graph_offsets, vec![json!(7)]);
    // Requested DEX text maps every invoke to its offset line.
    let dex = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id,"format":"dex","include_call_sites":true}),
        &cancel,
    )
    .unwrap();
    let text = dex["text"].as_str().unwrap();
    let dex_items = dex["call_sites"]["items"].as_array().unwrap();
    assert_eq!(dex_items.len(), 2);
    for item in dex_items {
        let mapping = &item["source_mapping"];
        assert_eq!(mapping["status"], "exact_dex_line");
        assert_eq!(mapping["reconstruction_status"], "dex-requested");
        let line = mapping["location"]["line"].as_u64().unwrap() as usize;
        let marker = format!("{:04x}:", item["call_site_offset"].as_u64().unwrap());
        let text_line = text.lines().nth(line - 1).unwrap();
        assert!(text_line.trim_start().starts_with(&marker), "{text_line}");
        assert!(
            text_line.contains(item["callee_method_id"].as_str().unwrap()),
            "{text_line}"
        );
    }
    assert_eq!(
        dex_items[1]["constant_arguments"]["arguments"][0]["value"],
        21
    );
    // Paging the text keeps the complete table.
    let paged = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id,"include_call_sites":true,"limit":10}),
        &cancel,
    )
    .unwrap();
    assert_eq!(paged["text"].as_str().unwrap().chars().count(), 10);
    assert_eq!(paged["call_sites"]["total"], 2);
    assert_eq!(paged["call_sites"]["items"], java["call_sites"]["items"]);
    let scoped = json!({"instance_id":"i","project_id":"p","method_id":id});
    let mut flag = scoped.clone();
    flag["include_call_sites"] = json!(true);
    validate("get_method_source", &flag).unwrap();
    flag["include_call_sites"] = json!("yes");
    assert!(validate("get_method_source", &flag).is_err());
}
