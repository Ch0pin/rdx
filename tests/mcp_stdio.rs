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
    let invalid_tools = [
        (
            "resolve_instance",
            json!({"package_name":"sample"}),
            "Missing apk_sha256",
        ),
        (
            "resolve_instance",
            json!({"package_name":"sample","apk_sha256":"invalid"}),
            "64-character hexadecimal",
        ),
        (
            "resolve_instance",
            json!({"package_name":"sample","apk_sha256":"a".repeat(64),"diagnostics":"invalid"}),
            "Invalid diagnostics",
        ),
        (
            "get_guard_summary",
            json!({"instance_id":"unused","project_id":"unused",
            "signature":"sample.Target.run()Z","caller_method_id":"sample.Caller.run()V","call_site_offset":0}),
            "Supply either exact method_id or caller_method_id and call_site_offset",
        ),
        (
            "get_guard_summary",
            json!({"instance_id":"unused","project_id":"unused",
            "method_id":"sample.Target.run()Z","method_signature":"sample.Target.other()Z"}),
            "CONFLICTING_METHOD_SELECTORS",
        ),
        (
            "find_network_stacks",
            json!({"instance_id":"unused","project_id":"unused",
            "projection":"services","include_observer":true}),
            "include_observer requires projection=hook_maps",
        ),
    ];
    let mut input = child.stdin.take().unwrap();
    for message in messages {
        writeln!(input, "{message}").unwrap();
    }
    for (index, (tool, arguments, _)) in invalid_tools.iter().enumerate() {
        let message = json!({"jsonrpc":"2.0","id":index+6,"method":"tools/call",
            "params":{"name":tool,"arguments":arguments}});
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
    assert_eq!(replies.len(), 6 + invalid_tools.len());
    assert_eq!(replies[0]["error"]["code"], -32600);
    assert_eq!(replies[1]["result"]["protocolVersion"], "2025-06-18");
    let instructions = replies[1]["result"]["instructions"].as_str().unwrap();
    for clause in [
        "EVERY next_cursor as resume_cursor",
        "pinned instance_id/project_id",
        "exhausted=true and no continuation remains",
        "accumulate errors",
        "this response only",
        "client-driven",
        "limit may change between pages",
    ] {
        assert!(
            instructions.contains(clause),
            "initialize instructions: {clause}"
        );
    }
    let tools = replies[2]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 60);
    let schema = |name: &str| {
        &tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} must be advertised over stdio"))["inputSchema"]
    };
    let source_description = tools
        .iter()
        .find(|tool| tool["name"] == "search_code")
        .unwrap()["description"]
        .as_str()
        .unwrap();
    assert!(source_description.contains("follow EVERY next_cursor as resume_cursor"));
    assert!(
        source_description
            .contains("a clean final page cannot erase an earlier page's errors or truncation")
    );
    let source_properties = &schema("search_code")["properties"];
    assert_eq!(source_properties["regex"]["default"], false);
    assert_eq!(source_properties["case_sensitive"]["default"], true);
    assert_eq!(
        source_properties["scope"]["enum"],
        json!(["code", "resources", "all"])
    );
    assert!(source_properties.get("resource_prefix").is_some());
    let resume_description = source_properties["resume_cursor"]["description"]
        .as_str()
        .unwrap();
    assert!(resume_description.contains("pinned instance_id/project_id"));
    assert!(resume_description.contains("across every page"));

    for name in [
        "list_analysis_plugins",
        "run_plugin_analysis",
        "get_plugin_analysis",
        "cancel_plugin_analysis",
    ] {
        for key in ["instance_id", "project_id"] {
            assert!(
                schema(name)["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(key))
            );
        }
    }
    assert_eq!(
        schema("run_plugin_analysis")["properties"]["arguments"]["type"],
        "object"
    );
    assert_eq!(
        schema("get_plugin_analysis")["properties"]["limit"]["maximum"],
        100
    );
    assert_eq!(
        schema("get_call_graph")["properties"]["max_output_bytes"]["default"],
        65536
    );
    assert_eq!(
        schema("get_call_graph_batch")["properties"]["max_output_bytes"]["minimum"],
        32768
    );
    assert_eq!(
        schema("search_resources")["properties"]["include_skipped_details"]["default"],
        false
    );
    assert_eq!(
        schema("get_method_source")["properties"]["include_dex_summary"]["default"],
        false
    );
    for name in ["get_export_status", "cancel_export"] {
        assert!(
            schema(name)["required"]
                .as_array()
                .unwrap()
                .contains(&json!("job_id"))
        );
    }
    assert_eq!(
        schema("export_package_source")["properties"]["background"]["default"],
        true
    );
    assert_eq!(
        schema("export_package_source")["properties"]["resume"]["type"],
        "boolean"
    );
    let export_prefix = &schema("export_package_source")["properties"]["package_prefix"];
    assert_eq!(export_prefix["type"], json!(["string", "array"]));
    assert_eq!(export_prefix["minLength"], 0);
    assert_eq!(export_prefix["minItems"], 1);
    assert_eq!(export_prefix["maxItems"], 64);
    assert_eq!(
        export_prefix["items"],
        json!({"type":"string","minLength":1})
    );
    assert_eq!(
        schema("get_manifest_components")["properties"]["exported_state"]["enum"],
        json!(["true", "false", "unresolved"])
    );
    assert_eq!(
        schema("get_manifest_components")["properties"]["max_bytes"]["default"],
        32768
    );
    let health = schema("get_health");
    let resolver = schema("resolve_instance");
    assert_eq!(resolver["required"], json!(["package_name", "apk_sha256"]));
    assert_eq!(resolver["properties"]["apk_sha256"]["type"], "string");
    assert_eq!(
        resolver["properties"]["diagnostics"]["enum"],
        json!(["related", "all"])
    );
    assert_eq!(resolver["properties"]["diagnostics"]["default"], "related");
    let hooks = schema("get_hook_targets");
    assert_eq!(
        hooks["properties"]["projection"]["enum"],
        json!(["targets", "receivers"])
    );
    assert_eq!(hooks["properties"]["projection"]["default"], "targets");
    let guards = schema("get_guard_summary");
    assert_eq!(guards["properties"]["call_site_offset"]["maximum"], 32767);
    assert!(
        guards["anyOf"]
            .as_array()
            .unwrap()
            .contains(&json!({"required":["caller_method_id","call_site_offset"]}))
    );
    for definition in [hooks, guards] {
        for alias in ["method_signature", "signature"] {
            assert_eq!(definition["properties"][alias]["type"], "string");
            assert!(
                definition["anyOf"]
                    .as_array()
                    .unwrap()
                    .contains(&json!({"required":[alias]}))
            );
        }
    }
    let network = schema("find_network_stacks");
    assert_eq!(
        network["properties"]["projection"]["enum"],
        json!(["all", "services", "hook_maps"])
    );
    assert_eq!(network["properties"]["include_observer"]["default"], false);
    assert_eq!(network["properties"]["include_observer"]["type"], "boolean");
    let navigation = schema("list_nav_deeplinks");
    assert_eq!(
        navigation["properties"]["include_compose"]["type"],
        "boolean"
    );
    assert_eq!(
        navigation["properties"]["include_compose"]["default"],
        false
    );
    let batch = schema("get_call_graph_batch");
    assert_eq!(batch["properties"]["method_ids"]["maxItems"], 16);
    assert!(
        !batch["required"]
            .as_array()
            .unwrap()
            .contains(&json!("method_ids"))
    );
    assert_eq!(
        batch["anyOf"],
        json!([{"required":["method_ids"]},{"required":["method_id"]},{"required":["method_signature"]},{"required":["signature"]}])
    );
    assert_eq!(
        batch["properties"]["method_id"]["type"],
        json!(["string", "array"])
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
    assert_eq!(search["properties"]["pagination"]["default"], "hits");
    for name in [
        "search_code",
        "get_all_classes",
        "search_classes_by_keyword",
    ] {
        assert_eq!(
            schema(name)["properties"]["package_prefix"]["type"],
            json!(["string", "array"])
        );
    }
    for name in [
        "get_class_source",
        "get_method_source",
        "get_class_disassembly",
    ] {
        assert_eq!(schema(name)["properties"]["source_hash"]["type"], "string");
    }

    let integers = schema("find_integer_references");
    for key in ["instance_id", "project_id"] {
        assert!(
            integers["required"]
                .as_array()
                .unwrap()
                .contains(&json!(key))
        );
    }
    assert_eq!(
        integers["anyOf"],
        json!([{"required":["values"]},{"required":["value"]}])
    );
    assert!(
        integers["properties"]["value"]["description"]
            .as_str()
            .unwrap()
            .starts_with("Alias for values")
    );
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
    for key in ["instance_id", "project_id"] {
        assert!(config["required"].as_array().unwrap().contains(&json!(key)));
    }
    assert_eq!(
        config["anyOf"],
        json!([{"required":["key"]},{"required":["query"]},{"required":["value"]},{"required":["config_key"]}])
    );
    assert_eq!(config["properties"]["limit"]["maximum"], 100);
    assert_eq!(
        schema("get_class_hierarchy")["anyOf"],
        json!([{"required":["class_id"]},{"required":["class_name"]}])
    );
    assert!(schema("forget_instance")["properties"]["instance_id"].is_object());
    let class_source = schema("get_class_source");
    assert!(class_source["properties"]["class_name"].is_object());
    assert_eq!(class_source["properties"]["max_chars"]["type"], "integer");
    assert_eq!(class_source["properties"]["max_chars"]["minimum"], 1);
    assert_eq!(class_source["properties"]["max_chars"]["maximum"], 128000);
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
    let method_search = schema("search_method_source");
    assert_eq!(method_search["anyOf"], method_source["anyOf"]);
    assert_eq!(method_search["properties"]["context_lines"]["maximum"], 8);
    assert_eq!(method_search["properties"]["limit"]["maximum"], 100);
    assert_eq!(method_search["properties"]["start_line"]["minimum"], 1);
    assert_eq!(
        method_search["properties"]["resume_cursor"]["type"],
        "string"
    );
    assert_eq!(method_search["properties"]["query"]["type"], "string");
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
        json!([{"required":["method_id"]},{"required":["class_name","method_name"]},{"required":["method_signature"]},{"required":["signature"]}])
    );
    assert_eq!(
        schema("find_field_references")["anyOf"],
        json!([{"required":["field_id"]},{"required":["class_name","field_name"]},{"required":["field"]}])
    );
    assert_eq!(
        schema("find_annotation_usages")["properties"]["element"]["enum"],
        json!(["all", "class", "field", "method", "parameter"])
    );
    assert_eq!(
        schema("find_annotation_usages")["properties"]["match_mode"]["enum"],
        json!(["exact_descriptor", "simple_name", "prefix"])
    );
    assert_eq!(
        schema("get_resource_value")["anyOf"],
        json!([{"required":["resource_id"]},{"required":["resource_name"]}])
    );
    assert_eq!(
        schema("search_resources")["properties"]["query"]["type"],
        "string"
    );
    let source_export = schema("export_source");
    assert_eq!(source_export["properties"]["out_path"]["type"], "string");
    assert_eq!(source_export["properties"]["class_id"]["type"], "string");
    assert!(
        source_export["anyOf"]
            .as_array()
            .unwrap()
            .contains(&json!({"required":["class_id"]}))
    );
    assert_eq!(
        schema("find_string_references")["properties"]["search_mode"]["enum"],
        json!(["auto", "streaming"])
    );
    assert_eq!(
        schema("get_call_graph")["properties"]["include_const_args"]["type"],
        "boolean"
    );
    assert_eq!(
        schema("get_call_graph")["properties"]["argument_index"]["maximum"],
        255
    );
    assert_eq!(
        schema("get_call_graph")["properties"]["argument_literal"]["type"],
        json!(["string", "integer"])
    );
    assert!(schema("get_call_graph_batch")["properties"]["argument_literal"].is_null());
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
    for key in ["instance_id", "project_id"] {
        assert!(required.contains(&json!(key)));
    }
    assert_eq!(
        literals["anyOf"],
        json!([{"required":["query"]},{"required":["value"]},{"required":["string"]},{"required":["literal"]},{"required":["text"]}])
    );
    assert_eq!(literals["properties"]["query"]["minLength"], 1);
    assert_eq!(literals["properties"]["value"]["minLength"], 1);
    assert_eq!(
        literals["properties"]["value"]["description"],
        "Alias for query; conflicting values are rejected"
    );
    let types = schema("find_type_references");
    assert_eq!(
        types["anyOf"],
        json!([{"required":["class_name"]},{"required":["type_name"]},{"required":["class_id"]},{"required":["type"]}])
    );
    assert_eq!(
        types["properties"]["type_name"]["description"],
        "Alias for class_name; conflicting values are rejected"
    );
    assert_eq!(
        types["properties"]["projection"]["enum"],
        json!(["full", "compact"])
    );
    assert_eq!(types["properties"]["projection"]["default"], "full");
    assert_eq!(
        types["properties"]["group_by"]["enum"],
        json!(["none", "owner_and_kind"])
    );
    assert_eq!(types["properties"]["group_by"]["default"], "none");
    assert_eq!(
        schema("search_code")["anyOf"],
        json!([{"required":["query"]},{"required":["keyword"]},{"required":["text"]},{"required":["value"]}])
    );
    assert_eq!(
        schema("get_dex_strings")["properties"]["keyword"]["description"],
        "Alias for query; conflicting values are rejected"
    );
    assert_eq!(
        method_source["properties"]["include_call_sites"]["type"],
        "boolean"
    );
    assert_eq!(
        method_source["properties"]["include_call_sites"]["default"],
        false
    );
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
        json!(["markdown", "records", "exposure", "counts"])
    );
    assert_eq!(
        manifest["properties"]["exported_state"]["enum"],
        json!(["true", "false", "unresolved"])
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
    for tool in ["get_manifest_components", "get_manifest_summary"] {
        assert_eq!(
            schema(tool)["properties"]["exported_only"]["type"],
            "boolean"
        );
        assert_eq!(
            schema(tool)["properties"]["exported_only"]["default"],
            false
        );
        assert_eq!(
            schema(tool)["properties"]["route_match_mode"]["enum"],
            json!(["exact", "contains"])
        );
        assert!(
            schema(tool)["properties"]["route_match_mode"]
                .get("default")
                .is_none()
        );
    }
    assert!(
        components["properties"]["match_mode"]["description"]
            .as_str()
            .unwrap()
            .contains("class_name only")
    );
    for field in [
        "launchMode",
        "taskAffinity",
        "allowTaskReparenting",
        "documentLaunchMode",
        "excludeFromRecents",
        "targetActivity",
    ] {
        assert!(
            components["properties"]["fields"]["x-allowed-fields"]
                .as_array()
                .unwrap()
                .contains(&json!(field))
        );
    }
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
    assert_eq!(graph["anyOf"], method_source["anyOf"]);
    assert_eq!(graph["properties"]["direct_only"]["type"], "boolean");
    assert_eq!(
        graph["properties"]["projection"]["enum"],
        json!(["graph", "call_sites"])
    );
    assert_eq!(graph["properties"]["limit"]["maximum"], 500);
    for key in [
        "package_prefix",
        "class_name",
        "method_name",
        "descriptor",
        "neighbor_class_name",
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
    for (index, (tool, _, expected)) in invalid_tools.iter().enumerate() {
        let reply = replies
            .iter()
            .find(|reply| reply["id"] == index + 6)
            .unwrap();
        let error = &reply["result"]["structuredContent"]["error"];
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(error["code"], "INVALID_ARGUMENT");
        assert!(
            error["message"].as_str().unwrap().contains(*expected),
            "{tool}: {error}"
        );
    }
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
    writeln!(input,"{}",json!({"jsonrpc":"2.0","id":98,"method":"tools/call","params":{"name":"get_manifest_components","arguments":{"instance_id":"unused","project_id":"unused","fields":"name,unknownLaunchField"}}})).unwrap();
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
    assert_eq!(replies.len(), 8);
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
    let field_error = &replies.iter().find(|reply| reply["id"] == 98).unwrap()["result"]["structuredContent"]
        ["error"];
    assert_eq!(field_error["code"], "INVALID_ARGUMENT");
    assert_eq!(
        field_error["details"]["invalid_fields"],
        json!(["unknownLaunchField"])
    );
    assert!(
        field_error["details"]["allowed_fields"]
            .as_array()
            .unwrap()
            .contains(&json!("launchMode"))
    );
    assert_eq!(replies.last().unwrap()["id"], 99);
    assert_eq!(replies.last().unwrap()["result"], json!({}));
}
