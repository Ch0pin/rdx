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
    assert_eq!(tools.len(), 22);
    let schema = |name: &str| {
        &tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} must be advertised over stdio"))["inputSchema"]
    };
    let health = schema("get_health");
    let method_source = schema("get_method_source");
    for key in ["method_id", "instance_id", "project_id"] {
        assert!(
            method_source["required"]
                .as_array()
                .unwrap()
                .contains(&json!(key))
        );
    }
    assert_eq!(method_source["properties"]["limit"]["maximum"], 128000);
    assert_eq!(method_source["properties"]["offset"]["minimum"], 0);
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
        json!(["markdown", "records"])
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
    let implementations = tools
        .iter()
        .find(|tool| tool["name"] == "find_implementations")
        .expect("implementation lookup must be advertised over stdio");
    let required = implementations["inputSchema"]["required"]
        .as_array()
        .unwrap();
    for key in ["class_id", "instance_id", "project_id"] {
        assert!(required.contains(&json!(key)));
    }
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
