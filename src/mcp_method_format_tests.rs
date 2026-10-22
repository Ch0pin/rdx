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
    let expected = crate::native_engine::disassembly::render_method(method, &class.symbols);
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
    let expected = crate::native_engine::disassembly::render_method(method, &class.symbols);
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
