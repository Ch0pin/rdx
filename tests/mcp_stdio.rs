use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn stdio_handshake_tools_and_errors_are_machine_readable() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rdx"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let messages = [
        json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":4,"method":"not/a/method"}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"rename_class","arguments":{}}}),
    ];
    let mut input = child.stdin.take().unwrap();
    for message in messages {
        writeln!(input, "{message}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 6);
    assert_eq!(replies[0]["error"]["code"], -32600);
    assert_eq!(replies[1]["result"]["protocolVersion"], "2025-06-18");
    let tools = replies[2]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 45);
    let schema = |name: &str| {
        &tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} must be advertised over stdio"))["inputSchema"]
    };
    let health = schema("get_health");
    let batch = schema("get_call_graph_batch");
    assert_eq!(batch["properties"]["method_ids"]["maxItems"], 16);
    assert!(
        batch["required"]
            .as_array()
            .unwrap()
            .contains(&json!("method_ids"))
    );
    for name in ["get_call_graph", "get_call_graph_batch"] {
        assert_eq!(
            schema(name)["properties"]["include_source_locations"]["type"],
            "boolean"
        );
        assert_eq!(
            schema(name)["properties"]["include_forwarders"]["type"],
            "boolean"
        );
    }
    let reference = schema("resolve_reference");
    assert!(
        reference["required"]
            .as_array()
            .unwrap()
            .contains(&json!("source_hash"))
    );
    assert_eq!(reference["properties"]["source_offset"]["minimum"], 0);
    assert_eq!(reference["properties"]["line"]["minimum"], 1);
    let search = schema("search_code");
    assert_eq!(
        search["properties"]["mode"]["enum"],
        json!(["source", "identifiers"])
    );
    assert_eq!(
        search["properties"]["symbol_kind"]["enum"],
        json!(["all", "class", "method", "field"])
    );
    assert_eq!(search["properties"]["case_sensitive"]["type"], "boolean");

    let integers = schema("find_integer_references");
    for key in ["values", "instance_id", "project_id"] {
        assert!(
            integers["required"]
                .as_array()
                .unwrap()
                .contains(&json!(key))
        );
    }
    assert_eq!(integers["properties"]["values"]["maxItems"], 64);
    assert_eq!(integers["properties"]["bit_width"]["enum"], json!([32, 64]));
    assert_eq!(integers["properties"]["limit"]["maximum"], 500);

    assert_eq!(
        schema("find_instantiations")["anyOf"],
        json!([{"required":["class_id"]},{"required":["class_name"]},{"required":["allocating_class"]},{"required":["allocating_method_id"]}])
    );
    assert_eq!(
        schema("find_instantiations")["properties"]["limit"]["maximum"],
        500
    );
    let config = schema("find_config_key_consumers");
    for key in ["key", "instance_id", "project_id"] {
        assert!(config["required"].as_array().unwrap().contains(&json!(key)));
    }
    assert_eq!(config["properties"]["limit"]["maximum"], 100);
    assert_eq!(
        schema("get_class_hierarchy")["anyOf"],
        json!([{"required":["class_id"]},{"required":["class_name"]}])
    );
    assert!(schema("forget_instance")["properties"]["instance_id"].is_object());
    let class_source = schema("get_class_source");
    assert!(class_source["properties"]["class_name"].is_object());
    assert_eq!(
        class_source["anyOf"],
        json!([{"required":["class_id"]},{"required":["class_name"]}])
    );
    assert_eq!(
        schema("get_lambda_branch")["properties"]["call_site_offset"]["type"],
        "integer"
    );
    assert_eq!(
        schema("find_field_references")["properties"]["access"]["enum"],
        json!(["all", "read", "write"])
    );
    let method_source = schema("get_method_source");
    for key in ["instance_id", "project_id"] {
        assert!(
            method_source["required"]
                .as_array()
                .unwrap()
                .contains(&json!(key))
        );
    }
    assert_eq!(
        method_source["anyOf"],
        json!([{"required":["method_id"]},{"required":["class_name","method_name"]}])
    );
    assert_eq!(
        schema("find_field_references")["anyOf"],
        json!([{"required":["field_id"]},{"required":["class_name","field_name"]}])
    );
    assert_eq!(
        schema("find_annotation_usages")["properties"]["element"]["enum"],
        json!(["all", "class", "field", "method", "parameter"])
    );
    assert_eq!(
        schema("get_resource_value")["anyOf"],
        json!([{"required":["resource_id"]},{"required":["resource_name"]}])
    );
    assert_eq!(
        schema("search_resources")["properties"]["query"]["type"],
        "string"
    );
    assert_eq!(
        schema("export_source")["properties"]["out_path"]["type"],
        "string"
    );
    assert_eq!(
        schema("find_string_references")["properties"]["search_mode"]["enum"],
        json!(["auto", "streaming"])
    );
    assert_eq!(
        schema("get_call_graph")["properties"]["include_const_args"]["type"],
        "boolean"
    );
    assert_eq!(method_source["properties"]["limit"]["maximum"], 128000);
    assert_eq!(method_source["properties"]["offset"]["minimum"], 0);
    assert_eq!(
        method_source["properties"]["format"]["enum"],
        json!(["auto", "dex"])
    );
    assert_eq!(method_source["properties"]["format"]["default"], "auto");
    assert_eq!(method_source["additionalProperties"], false);
    assert_eq!(health["required"], json!([]));
    assert_eq!(health["additionalProperties"], false);
    for key in ["apk_path", "apk_sha256", "package_name"] {
        assert_eq!(health["properties"][key]["type"], "string");
        assert_eq!(health["properties"][key]["minLength"], 1);
    }
    for key in ["offset", "registration_offset"] {
        assert_eq!(health["properties"][key]["type"], "integer");
        assert_eq!(health["properties"][key]["minimum"], 0);
    }
    assert_eq!(health["properties"]["limit"]["maximum"], 500);
    let literals = schema("find_string_references");
    let required = literals["required"].as_array().unwrap();
    for key in ["query", "instance_id", "project_id"] {
        assert!(required.contains(&json!(key)));
    }
    assert_eq!(literals["properties"]["query"]["minLength"], 1);
    assert_eq!(
        literals["properties"]["match_mode"]["enum"],
        json!(["exact", "contains"])
    );
    assert_eq!(literals["properties"]["match_mode"]["default"], "contains");
    assert_eq!(literals["properties"]["package_prefix"]["type"], "string");
    assert_eq!(literals["properties"]["limit"]["maximum"], 500);
    let manifest = schema("get_manifest_summary");
    assert_eq!(
        manifest["properties"]["format"]["enum"],
        json!(["markdown", "records", "exposure"])
    );
    assert_eq!(
        manifest["properties"]["component_type"]["enum"],
        json!([
            "activity",
            "activity-alias",
            "service",
            "receiver",
            "provider"
        ])
    );
    for key in ["component", "host", "scheme", "path", "permission", "query"] {
        assert_eq!(manifest["properties"][key]["type"], "string");
        assert_eq!(manifest["properties"][key]["minLength"], 1);
    }
    assert_eq!(manifest["additionalProperties"], false);
    let components = schema("get_manifest_components");
    assert_eq!(
        components["properties"]["match_mode"]["enum"],
        json!(["exact", "contains"])
    );
    assert_eq!(components["properties"]["limit"]["maximum"], 500);
    for key in [
        "class_name",
        "component_type",
        "permission",
        "host",
        "scheme",
        "path",
        "query",
    ] {
        assert_eq!(components["properties"][key]["type"], "string");
    }
    let graph = schema("get_call_graph");
    assert_eq!(graph["properties"]["direct_only"]["type"], "boolean");
    assert_eq!(
        graph["properties"]["projection"]["enum"],
        json!(["graph", "call_sites"])
    );
    assert_eq!(graph["properties"]["limit"]["maximum"], 500);
    for key in [
        "package_prefix",
        "class_name",
        "annotation",
        "component_type",
    ] {
        assert_eq!(graph["properties"][key]["type"], "string");
    }
    let implementations = tools
        .iter()
        .find(|tool| tool["name"] == "find_implementations")
        .expect("implementation lookup must be advertised over stdio");
    let required = implementations["inputSchema"]["required"]
        .as_array()
        .unwrap();
    for key in ["instance_id", "project_id"] {
        assert!(required.contains(&json!(key)));
    }
    assert_eq!(
        implementations["inputSchema"]["anyOf"],
        json!([{"required":["class_id"]},{"required":["class_name"]}])
    );
    assert!(tools.iter().any(|t| t["name"] == "get_call_graph"));
    assert!(tools.iter().any(|t| t["name"] == "open_apk"));
    assert!(
        !tools
            .iter()
            .any(|t| t["name"].as_str().unwrap().starts_with("debug_"))
    );
    assert_eq!(replies[3]["result"], json!({}));
    assert_eq!(replies[4]["error"]["code"], -32601);
    assert_eq!(replies[5]["result"]["isError"], true);
}

#[test]
fn invalid_limits_return_structured_bounds_and_gateway_stays_responsive() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rdx"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"bounds-test","version":"1"}}})).unwrap();
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    let invalid = [json!(501), json!(0), json!(-1), json!(1.5), json!("500")];
    for (i, limit) in invalid.iter().enumerate() {
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":i+2,"method":"tools/call","params":{
            "name":"get_call_graph","arguments":{"instance_id":"unused","project_id":"unused",
            "method_id":"sample.Target.run()V","projection":"call_sites","limit":limit}}})
        )
        .unwrap();
    }
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","id":99,"method":"ping"})
    )
    .unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 7);
    for (i, received) in invalid.iter().enumerate() {
        let result = &replies[i + 1]["result"];
        assert_eq!(result["isError"], true);
        let data = &result["structuredContent"];
        assert_eq!(data["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(data["error"]["details"]["parameter"], "limit");
        assert_eq!(data["error"]["details"]["received"], *received);
        assert_eq!(data["error"]["details"]["minimum"], 1);
        assert_eq!(data["error"]["details"]["maximum"], 500);
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            *data
        );
    }
    assert_eq!(replies.last().unwrap()["id"], 99);
    assert_eq!(replies.last().unwrap()["result"], json!({}));
}
