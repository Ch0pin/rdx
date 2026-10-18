use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("rdx-mcp-regression-{}", random_id().unwrap()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn discovery_path_identity_resolves_aliases_and_separates_same_basenames() {
    let dir = Scratch::new();
    fs::create_dir(dir.0.join("one")).unwrap();
    fs::create_dir(dir.0.join("two")).unwrap();
    let one = dir.0.join("one/sample.apk");
    let two = dir.0.join("two/sample.apk");
    fs::write(&one, b"same bytes").unwrap();
    fs::write(&two, b"same bytes").unwrap();
    let info = json!({"apk_path":one.canonicalize().unwrap(),"apk_sha256":"same"});
    assert!(instance_matches(&info, &json!({"apk_path":one})));
    assert!(!instance_matches(
        &info,
        &json!({"apk_path":two,"apk_sha256":"same"})
    ));
    assert!(instance_matches(&info, &json!({"apk_path":"sample"})));
    #[cfg(unix)]
    {
        let alias = dir.0.join("alias.apk");
        std::os::unix::fs::symlink(&one, &alias).unwrap();
        assert!(instance_matches(&info, &json!({"apk_path":alias})));
    }
}

#[test]
fn legacy_basename_requires_absolute_request_and_explicit_matching_hash() {
    let info = json!({"apk_path":"sample.apk","apk_sha256":"abc"});
    assert!(instance_matches(
        &info,
        &json!({"apk_path":"/missing/sample.apk","apk_sha256":"abc"})
    ));
    for args in [
        json!({"apk_path":"/missing/sample.apk"}),
        json!({"apk_path":"/missing/sample.apk","apk_sha256":"wrong"}),
        json!({"apk_path":"/missing/other.apk","apk_sha256":"abc"}),
        json!({"apk_path":"missing/sample.apk","apk_sha256":"abc"}),
    ] {
        assert!(!instance_matches(&info, &args), "unexpected match: {args}");
    }
}

#[test]
fn discovery_preserves_live_results_and_reports_gui_disconnect_separately() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let live = Registration {
        instance_id: "live".into(),
        address,
        token: "token".into(),
    };
    let closed = Registration {
        instance_id: "closed".into(),
        address,
        token: "token".into(),
    };
    let peer = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        for response in [
            Some(
                json!({"result":{"instance_id":"live","state":"Running","apk_path":"/sample.apk"}}),
            ),
            None,
        ] {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "client never connected"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let request = read_frame(&mut BufReader::new(&mut stream))
                .unwrap()
                .unwrap();
            assert_eq!(request["name"], "get_instance_info");
            if let Some(response) = response {
                write_frame(&mut stream, &response).unwrap();
            }
        }
    });
    let result = discover(&[live, closed], &json!({})).unwrap();
    peer.join().unwrap();
    assert_eq!(result["items"][0]["instance_id"], "live");
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["diagnostics"][0]["instance_id"], "closed");
    assert_eq!(result["diagnostics"][0]["status"], "instance_unreachable");
    assert!(
        result["diagnostics"][0]["error"]
            .as_str()
            .unwrap()
            .contains("INSTANCE_DISCONNECTED")
    );
    assert_eq!(result["discovery_complete"], true);
    assert_eq!(result["registered_instances"], 2);
    assert!(
        discover(&[], &json!({})).unwrap()["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn manifest_route_filters_must_match_within_one_intent_filter() {
    let xml = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="example"><application><activity android:name=".Split"><intent-filter><data android:scheme="https" android:host="first.example"/></intent-filter><intent-filter><data android:scheme="custom" android:host="second.example"/></intent-filter></activity><activity android:name=".Combined"><intent-filter><data android:scheme="https"/><data android:host="second.example" android:pathPrefix="/safe"/></intent-filter></activity></application></manifest>"#;
    let records = crate::manifest_summary::query(
        xml,
        &json!({"scheme":"https","host":"second.example","path":"/safe"}),
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "example.Combined");
    assert_eq!(records[0]["matching_filter_indexes"], json!([0]));
    assert!(
        crate::manifest_summary::query(
            xml,
            &json!({"scheme":"https","host":"second.example","component":"Split"})
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        crate::manifest_summary::query(xml, &json!({"host":"example"}))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn discovery_and_manifest_schemas_reject_invalid_values() {
    for tool in ["get_health", "list_instances"] {
        validate(tool, &json!({"apk_path":"/sample.apk","apk_sha256":"abc","package_name":"example","registration_offset":0,"offset":0,"limit":1})).unwrap();
        for args in [
            json!({"apk_path":""}),
            json!({"registration_offset":-1}),
            json!({"limit":0}),
            json!({"unknown":true}),
        ] {
            assert!(validate(tool, &args).is_err(), "{tool}: {args}");
        }
    }
    let base = json!({"instance_id":"instance","project_id":"project","format":"records","component_type":"activity","host":"example.com","scheme":"https","path":"/route","permission":"example.READ","query":"route","limit":1});
    validate("get_manifest_summary", &base).unwrap();
    for (key, value) in [
        ("format", json!("xml")),
        ("component_type", json!("application")),
        ("host", json!("")),
        ("scheme", json!(7)),
        ("offset", json!(1.5)),
    ] {
        let mut args = base.clone();
        args[key] = value;
        assert!(validate("get_manifest_summary", &args).is_err(), "{args}");
    }
}

#[test]
fn manifest_record_pages_roundtrip_with_canonical_server_identity() {
    let dir = Scratch::new();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
    let alias = dir.0.join("navigation.apk");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&fixture, &alias).unwrap();
    #[cfg(not(unix))]
    fs::copy(&fixture, &alias).unwrap();
    let server = Server::start_in(alias.clone(), dir.0.clone(), random_id().unwrap()).unwrap();
    let record: Registration =
        serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        let status = server.status();
        assert_ne!(status.state, "Failed", "{}", status.last_error);
        if status.state == "Running" {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "project did not load");
        thread::sleep(Duration::from_millis(10));
    };
    let info = remote(&record, "get_instance_info", &json!({})).unwrap();
    assert_eq!(info["apk_path"], json!(alias.canonicalize().unwrap()));
    let mut args = json!({"instance_id":server.instance_id,"project_id":status.project_id,"format":"records","limit":500});
    let all = remote(&record, "get_manifest_summary", &args).unwrap();
    let expected = all["data"]["items"].as_array().unwrap();
    assert!(expected.len() > 1, "fixture must exercise multiple pages");
    let mut found = Vec::new();
    args["limit"] = json!(1);
    for offset in 0..expected.len() {
        args["offset"] = json!(offset);
        let response = remote(&record, "get_manifest_summary", &args).unwrap();
        assert_eq!(response["identity"]["project_id"], args["project_id"]);
        assert_eq!(response["data"]["source_hash"], all["data"]["source_hash"]);
        assert_eq!(response["data"]["offset_unit"], "matching records");
        assert_eq!(
            response["data"]["next_offset"],
            json!((offset + 1 < expected.len()).then_some(offset + 1))
        );
        found.extend(
            response["data"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .cloned(),
        );
    }
    assert_eq!(&found, expected);
    args["offset"] = json!(expected.len() + 1);
    assert!(remote(&record, "get_manifest_summary", &args).is_err());
}

#[test]
fn discovery_registration_batches_are_stable_across_record_pagination() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let entries: Vec<_> = (0..18)
        .map(|index| Registration {
            instance_id: format!("instance-{index}"),
            address,
            token: "batch-token".into(),
        })
        .collect();
    let peer = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        for _ in 0..18 {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "missing batch request"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let request = read_frame(&mut BufReader::new(&mut stream))
                .unwrap()
                .unwrap();
            write_frame(&mut stream, &json!({"result":{"instance_id":request["instance_id"],"state":"Running","package_name":"example"}})).unwrap();
        }
    });
    let first = discover(
        &entries,
        &json!({"registration_offset":0,"offset":15,"limit":1,"package_name":"example"}),
    )
    .unwrap();
    assert_eq!(first["total"], 16);
    assert_eq!(
        first["items"],
        json!([{"instance_id":"instance-15","state":"Running","package_name":"example"}])
    );
    assert_eq!(first["next_registration_offset"], 16);
    assert_eq!(first["discovery_complete"], false);
    assert_eq!(first["next_offset"], Value::Null);
    let second = discover(
        &entries,
        &json!({"registration_offset":16,"offset":1,"limit":1}),
    )
    .unwrap();
    peer.join().unwrap();
    assert_eq!(second["total"], 2);
    assert_eq!(second["items"][0]["instance_id"], "instance-17");
    assert_eq!(second["next_registration_offset"], Value::Null);
    assert_eq!(second["discovery_complete"], true);
    assert_eq!(second["registered_instances"], 18);
}

#[test]
fn manifest_grouped_relative_paths_are_searchable_and_preserve_nested_declarations() {
    let xml = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="example"><application><activity android:name=".Grouped"><intent-filter><data android:scheme="https" android:host="example.com"/><uri-relative-filter-group android:allow="false"><data android:pathPrefix="/private"/></uri-relative-filter-group></intent-filter></activity></application></manifest>"#;
    let records = crate::manifest_summary::query(
        xml,
        &json!({"scheme":"https","host":"example.com","path":"/private"}),
    )
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["matching_filter_indexes"], json!([0]));
    let elements = records[0]["intent_filters"][0]["elements"]
        .as_array()
        .unwrap();
    let group = elements
        .iter()
        .find(|element| element["tag"] == "uri-relative-filter-group")
        .unwrap();
    assert!(
        group["attributes"]
            .as_array()
            .unwrap()
            .contains(&json!(["allow", "false"]))
    );
    assert_eq!(group["children"][0]["tag"], "data");
    assert!(
        group["children"][0]["attributes"]
            .as_array()
            .unwrap()
            .contains(&json!(["pathPrefix", "/private"]))
    );
}

#[test]
fn manifest_record_limits_and_markdown_character_limits_are_validated_separately() {
    let base =
        json!({"instance_id":"instance","project_id":"project","format":"records","limit":501});
    assert!(validate("get_manifest_summary", &base).is_err());
    let mut args = base.clone();
    args["limit"] = json!(500);
    validate("get_manifest_summary", &args).unwrap();
    args["format"] = json!("markdown");
    args["limit"] = json!(128000);
    validate("get_manifest_summary", &args).unwrap();
    args["host"] = json!("example.com");
    assert!(validate("get_manifest_summary", &args).is_err());
}

#[test]
fn oversized_gui_response_becomes_small_error_and_next_frame_remains_readable() {
    let oversized = json!({"result":{"text":"x".repeat(MAX_FRAME as usize)}});
    let following = json!({"result":{"state":"Running"}});
    let mut wire = Vec::new();
    write_response(&mut wire, &oversized).unwrap();
    write_response(&mut wire, &following).unwrap();
    assert!(wire.len() < 1024, "oversized payload reached the transport");
    let mut reader = BufReader::new(wire.as_slice());
    let error = read_frame(&mut reader).unwrap().unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .starts_with("RESPONSE_TOO_LARGE:")
    );
    assert_eq!(read_frame(&mut reader).unwrap(), Some(following));
    assert_eq!(read_frame(&mut reader).unwrap(), None);
}

#[test]
fn final_mcp_envelope_size_accounts_for_both_content_representations() {
    let data = json!({"text":"x".repeat(MAX_FRAME as usize / 2)});
    assert!(serde_json::to_vec(&data).unwrap().len() < MAX_FRAME as usize);
    let envelope = json!({"jsonrpc":"2.0","id":42,"result":{
        "content":[{"type":"text","text":data.to_string()}],
        "structuredContent":data,"isError":false
    }});
    assert!(serde_json::to_vec(&envelope).unwrap().len() > MAX_FRAME as usize);
    let following = json!({"jsonrpc":"2.0","id":43,"result":{}});
    let mut wire = Vec::new();
    write_response(&mut wire, &envelope).unwrap();
    write_response(&mut wire, &following).unwrap();
    assert!(
        wire.len() < 1024,
        "oversized MCP envelope reached the transport"
    );
    let mut reader = BufReader::new(wire.as_slice());
    let error = read_frame(&mut reader).unwrap().unwrap();
    assert_eq!(error["jsonrpc"], "2.0");
    assert_eq!(error["id"], 42);
    assert_eq!(error["result"]["isError"], true);
    assert!(
        error["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("RESPONSE_TOO_LARGE:")
    );
    assert_eq!(read_frame(&mut reader).unwrap(), Some(following));
    assert_eq!(read_frame(&mut reader).unwrap(), None);
}

#[test]
fn manifest_permissions_cover_own_inherited_and_top_level_declarations() {
    let xml = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="example"><permission android:name="example.ACCESS" android:protectionLevel="signature"/><uses-permission android:name="example.READ"/><application android:permission="example.ACCESS"><activity android:name=".Inherited"/><activity android:name=".Own" android:permission="example.OWN"/><service android:name=".Worker"/></application></manifest>"#;
    let inherited =
        crate::manifest_summary::query(xml, &json!({"permission":"example.ACCESS"})).unwrap();
    assert_eq!(inherited.len(), 3);
    assert!(
        inherited
            .iter()
            .any(|record| record["record_type"] == "permission"
                && record["name"] == "example.ACCESS")
    );
    let activity = inherited
        .iter()
        .find(|record| record["name"] == "example.Inherited")
        .unwrap();
    assert!(activity["details"].as_array().unwrap().contains(&json!([
        "Inherited application permission",
        "example.ACCESS"
    ])));
    let own = crate::manifest_summary::query(xml, &json!({"permission":"example.OWN"})).unwrap();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0]["name"], "example.Own");
    assert!(
        own[0]["details"]
            .as_array()
            .unwrap()
            .contains(&json!(["permission", "example.OWN"]))
    );
    let declaration =
        crate::manifest_summary::query(xml, &json!({"permission":"example.READ"})).unwrap();
    assert_eq!(declaration.len(), 1);
    assert_eq!(declaration[0]["record_type"], "permission");
    assert_eq!(declaration[0]["declaration"], "uses-permission");
}

#[test]
fn manifest_combines_record_filters_and_case_insensitive_query() {
    let xml = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="example"><application android:permission="example.ACCESS"><activity android:name=".RouteActivity"><intent-filter><data android:scheme="https" android:host="routes.example" android:pathPrefix="/Open"/></intent-filter></activity><activity android:name=".OtherActivity"><intent-filter><data android:scheme="https" android:host="other.example" android:pathPrefix="/Open"/></intent-filter></activity><service android:name=".RouteService"/></application></manifest>"#;
    let filters = json!({"format":"records","component_type":"activity","component":"Route","host":"routes.example","scheme":"https","path":"/Open","permission":"example.ACCESS","query":"rOuTeAcTiViTy"});
    let records = crate::manifest_summary::query(xml, &filters).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["record_type"], "component");
    assert_eq!(records[0]["component_type"], "activity");
    assert_eq!(records[0]["name"], "example.RouteActivity");
    assert_eq!(records[0]["matching_filter_indexes"], json!([0]));
    let mut mismatch = filters.clone();
    mismatch["permission"] = json!("example.OTHER");
    assert!(
        crate::manifest_summary::query(xml, &mismatch)
            .unwrap()
            .is_empty()
    );
    mismatch = filters;
    mismatch["query"] = json!("absent");
    assert!(
        crate::manifest_summary::query(xml, &mismatch)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn accepted_nonblocking_socket_is_configured_for_complete_large_responses() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let accepted = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "connection was not accepted"
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept: {error}"),
        }
    };
    accepted.set_nonblocking(true).unwrap();
    configure_stream(&accepted).unwrap();
    assert_eq!(
        accepted.read_timeout().unwrap(),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        accepted.write_timeout().unwrap(),
        Some(Duration::from_secs(5))
    );
    let expected = json!({"result":{"text":"x".repeat(2 * 1024 * 1024)}});
    let outgoing = expected.clone();
    let writer = thread::spawn(move || {
        let mut stream = accepted;
        write_response(&mut stream, &outgoing)
    });
    let actual = read_frame(&mut BufReader::new(&mut client))
        .unwrap()
        .unwrap();
    writer.join().unwrap().unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn short_class_selection_requires_unique_exact_simple_name() {
    let p = Project {
        classes: vec![
            "one.ReceiptPdfViewerArgs".into(),
            "one.Shared".into(),
            "two.Shared".into(),
            "Root".into(),
        ],
        resources: vec![],
    };
    assert_eq!(
        resolve_class_id(&p, "ReceiptPdfViewerArgs").unwrap(),
        "one.ReceiptPdfViewerArgs"
    );
    assert_eq!(resolve_class_id(&p, "one.Shared").unwrap(), "one.Shared");
    assert_eq!(resolve_class_id(&p, "Root").unwrap(), "Root");
    assert!(
        resolve_class_id(&p, "Shared")
            .unwrap_err()
            .to_string()
            .contains("AMBIGUOUS_CLASS")
    );
    assert!(
        resolve_class_id(&p, "stale.ReceiptPdfViewerArgs")
            .unwrap_err()
            .to_string()
            .contains("requested_class_id=stale.ReceiptPdfViewerArgs")
    );
}

#[test]
fn method_source_is_complete_by_default_and_unicode_pages_reassemble() {
    let source = "𐐀switch λ\n".repeat(20000);
    let all = method_source_page(source.clone(), &json!({}), "java").unwrap();
    assert_eq!(all["text"], source);
    assert!(all["next_offset"].is_null());
    let mut got = String::new();
    let mut offset = 0;
    loop {
        let page = method_source_page(
            source.clone(),
            &json!({"offset":offset,"limit":12345}),
            "java",
        )
        .unwrap();
        assert_eq!(page["source_hash"], all["source_hash"]);
        got.push_str(page["text"].as_str().unwrap());
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        offset = next;
    }
    assert_eq!(got, source);
    assert!(
        validate(
            "get_method_source",
            &json!({"method_id":"one.C.m()V","instance_id":"i","project_id":"p"})
        )
        .is_ok()
    );
    assert!(
        validate(
            "get_method_source",
            &json!({"method_id":"one.C.m()V","instance_id":"i"})
        )
        .is_err()
    );
}

#[test]
fn method_source_resolves_raw_signature_and_short_class_recovers_after_error() {
    let mut engine = NativeEngine::start().unwrap();
    let project = engine
        .open(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
        .unwrap();
    let cancel = AtomicBool::new(false);
    let methods = dispatch(
        &mut engine,
        &project,
        "get_methods_of_class",
        &json!({"class_id":"Target"}),
        &cancel,
    )
    .unwrap();
    assert_eq!(methods["class_id"], "sample.Target");
    let id = methods["items"][0]["method_id"].as_str().unwrap();
    let result = dispatch(
        &mut engine,
        &project,
        "get_method_source",
        &json!({"method_id":id}),
        &cancel,
    )
    .unwrap();
    assert_eq!(result["method_id"], id);
    assert!(result["next_offset"].is_null());
    assert_eq!(
        result["source_hash"],
        crate::engine::source_identity(result["text"].as_str().unwrap())
    );
    assert!(
        dispatch(
            &mut engine,
            &project,
            "get_class_source",
            &json!({"class_id":"Missing"}),
            &cancel
        )
        .unwrap_err()
        .to_string()
        .contains("requested_class_id=Missing")
    );
    assert!(
        dispatch(
            &mut engine,
            &project,
            "get_class_source",
            &json!({"class_id":"Target"}),
            &cancel
        )
        .is_ok()
    );
    assert!(
        dispatch(
            &mut engine,
            &project,
            "get_method_source",
            &json!({"method_id":"sample.Target.missing()V"}),
            &cancel
        )
        .is_err()
    );
    assert_eq!(
        engine
            .decompile_method_source(id, &AtomicBool::new(true))
            .unwrap_err()
            .to_string(),
        "CANCELLED"
    );
}

#[test]
fn stale_class_request_keeps_discovery_running_and_method_pages_pin_identity() {
    let dir = Scratch::new();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
    let server = Server::start_in(path, dir.0.clone(), random_id().unwrap()).unwrap();
    let record: Registration =
        serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        let status = server.status();
        assert_ne!(status.state, "Failed", "{}", status.last_error);
        if status.state == "Running" {
            break status;
        }
        assert!(std::time::Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    };
    let scope = json!({"instance_id":server.instance_id,"project_id":status.project_id});
    let mut args = scope.clone();
    args["class_id"] = json!("StaleSelection");
    assert!(
        remote(&record, "get_class_source", &args)
            .unwrap_err()
            .to_string()
            .contains("requested_class_id=StaleSelection")
    );
    let discovered = discover(std::slice::from_ref(&record), &json!({})).unwrap();
    let info = &discovered["items"][0];
    assert_eq!(info["state"], "Running");
    assert_eq!(info["error"], "");
    assert_eq!(
        info["last_request_error"]["requested_class_id"],
        "StaleSelection"
    );
    assert_eq!(info["last_request_error"]["tool"], "get_class_source");
    assert_eq!(info["project_id"], scope["project_id"]);
    args["class_id"] = json!("Target");
    let methods = remote(&record, "get_methods_of_class", &args).unwrap();
    assert_eq!(methods["data"]["class_id"], "sample.Target");
    assert_eq!(methods["data"]["requested_class_id"], "Target");
    let info = remote(&record, "get_instance_info", &json!({})).unwrap();
    assert!(info["last_request_error"].is_null());
    let mut args = scope;
    args["method_id"] = json!("sample.Target.doubleValue(I)I");
    let full = remote(&record, "get_method_source", &args).unwrap();
    assert_eq!(full["identity"], methods["identity"]);
    let source = full["data"]["text"].as_str().unwrap();
    assert!(source.contains("doubleValue"));
    assert!(
        !source.contains("String"),
        "selected overload must be isolated: {source}"
    );
    assert!(full["data"]["next_offset"].is_null());
    args["limit"] = json!(7);
    let mut rebuilt = String::new();
    loop {
        let page = remote(&record, "get_method_source", &args).unwrap();
        assert_eq!(page["identity"], full["identity"]);
        assert_eq!(page["data"]["source_hash"], full["data"]["source_hash"]);
        rebuilt.push_str(page["data"]["text"].as_str().unwrap());
        let Some(next) = page["data"]["next_offset"].as_u64() else {
            break;
        };
        args["offset"] = json!(next);
    }
    assert_eq!(rebuilt, source);
    args["project_id"] = json!("stale-project");
    assert!(
        remote(&record, "get_method_source", &args)
            .unwrap_err()
            .to_string()
            .contains("PROJECT_CHANGED")
    );
}

#[test]
#[ignore = "root runs pinned Airbnb MCP validation serially with RDX_REVIEW_APK"]
fn airbnb_short_selection_and_large_method_source_roundtrip() {
    let dir = Scratch::new();
    let path = PathBuf::from(std::env::var_os("RDX_REVIEW_APK").unwrap());
    let server = Server::start_in(path, dir.0.clone(), random_id().unwrap()).unwrap();
    let record: Registration =
        serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let status = loop {
        let status = server.status();
        assert_ne!(status.state, "Failed", "{}", status.last_error);
        if status.state == "Running" {
            break status;
        }
        assert!(std::time::Instant::now() < deadline);
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        status.apk_sha256,
        "cc74899171c8e84a2c4b6d5e0f307ccbbfbb5d61911fb3113250ca17217d07ad"
    );
    let scope = json!({"instance_id":server.instance_id,"project_id":status.project_id});
    let mut args = scope.clone();
    args["class_id"] = json!("ReceiptPdfViewerArgs");
    let resolved = remote(&record, "get_methods_of_class", &args).unwrap();
    assert!(
        resolved["data"]["class_id"]
            .as_str()
            .unwrap()
            .ends_with(".ReceiptPdfViewerArgs")
    );
    assert_eq!(
        resolved["data"]["requested_class_id"],
        "ReceiptPdfViewerArgs"
    );
    let mut largest = 0;
    for name in ["f9o", "peu0", "t8v0", "ylr0"] {
        let mut args = scope.clone();
        args["class_id"] = json!(name);
        args["limit"] = json!(500);
        let methods = remote(&record, "get_methods_of_class", &args).unwrap();
        assert!(methods["data"]["next_offset"].is_null());
        for method in methods["data"]["items"].as_array().unwrap() {
            let mut args = scope.clone();
            args["method_id"] = method["method_id"].clone();
            let all = remote(&record, "get_method_source", &args).unwrap();
            assert_eq!(all["identity"], resolved["identity"]);
            assert_eq!(all["data"]["method_id"], method["method_id"]);
            assert!(all["data"]["next_offset"].is_null());
            let source = all["data"]["text"].as_str().unwrap();
            let length = source.chars().count();
            largest = largest.max(length);
            assert_eq!(all["data"]["total_chars"], length);
            assert_eq!(
                all["data"]["source_hash"],
                crate::engine::source_identity(source)
            );
            if length > 50000 {
                args["offset"] = json!(50000);
                let tail = remote(&record, "get_method_source", &args).unwrap();
                assert_eq!(tail["data"]["source_hash"], all["data"]["source_hash"]);
                assert_eq!(
                    tail["data"]["text"],
                    source.chars().skip(50000).collect::<String>()
                );
                println!(
                    "{}",
                    json!({"method_id":method["method_id"],"total_chars":length,"format":all["data"]["format"],"source_hash":all["data"]["source_hash"]})
                );
            }
        }
    }
    assert!(
        largest > 50000,
        "corpus must exercise reported truncation threshold"
    );
    assert_eq!(server.status().state, "Running");
}
