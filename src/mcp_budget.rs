//! Serialized output limits applied after call-site enrichment and filtering.
use super::*;

pub(super) fn requested(args: &Value) -> Result<usize> {
    let bytes = match args.get("max_output_bytes") {
        None => 65536,
        Some(value) => usize::try_from(
            value
                .as_u64()
                .context("max_output_bytes must be a nonnegative integer")?,
        )
        .context("max_output_bytes is too large")?,
    };
    ensure!(
        (16384..=1048576).contains(&bytes),
        "max_output_bytes must be 16384..1048576"
    );
    Ok(bytes)
}
pub(super) fn call_graph(result: &mut Value, args: &Value) -> Result<()> {
    bound(result, args, requested(args)?)
}
fn bound(result: &mut Value, args: &Value, budget: usize) -> Result<()> {
    result["response_byte_budget"] = json!(budget);
    result["response_budget_limited"] = json!(false);
    if result["projection"] == "call_sites" {
        if serde_json::to_vec(result)?.len() <= budget {
            return Ok(());
        }
        let keys = ["call_sites", "excluded_call_sites", "unresolved_call_sites"];
        let mut arrays = Vec::new();
        let mut positions = Vec::new();
        for key in keys {
            if let Some(rows) = result.get_mut(key).and_then(Value::as_array_mut) {
                let rows = std::mem::take(rows);
                for row in &rows {
                    positions.push((
                        row["result_offset"]
                            .as_u64()
                            .context("Missing call-site result_offset")?,
                        serde_json::to_vec(row)?.len() + 1,
                    ));
                }
                arrays.push((key, rows));
            }
        }
        positions.sort_unstable();
        let mut bytes = serde_json::to_vec(result)?.len() + 1024;
        let mut resume = None;
        for (index, (position, size)) in positions.iter().enumerate() {
            if bytes.saturating_add(*size) > budget {
                ensure!(
                    index > 0,
                    "RESULT_TOO_LARGE: a call-site record exceeds max_output_bytes; increase the budget or disable optional enrichment"
                );
                resume = Some(*position);
                break;
            }
            bytes += size;
        }
        for (key, mut rows) in arrays {
            if let Some(resume) = resume {
                rows.retain(|r| r["result_offset"].as_u64().unwrap() < resume);
            }
            result[key] = json!(rows);
        }
        if let Some(resume) = resume {
            result["next_offset"] = json!(resume);
            result["has_more"] = json!(true);
            result["response_budget_limited"] = json!(true);
            let count = keys
                .iter()
                .map(|k| result[*k].as_array().map_or(0, Vec::len))
                .sum::<usize>();
            result["returned_count"] = json!(count);
            if result["argument_filter"].is_object() {
                let matched = result["call_sites"].as_array().map_or(0, Vec::len);
                let excluded = result["excluded_call_sites"].as_array().map_or(0, Vec::len);
                let unresolved = result["unresolved_call_sites"]
                    .as_array()
                    .map_or(0, Vec::len);
                result["argument_filter"]["page_total"] = json!(count);
                result["argument_filter"]["matched"] = json!(matched);
                result["argument_filter"]["excluded"] = json!(excluded);
                result["argument_filter"]["unresolved"] = json!(unresolved);
            }
            result["constant_arguments_hash_scope"] = json!(
                "Analyzed page before filtering and output trimming; source_hash identifies all matching raw records"
            );
        }
    } else if result["projection"] == "graph" {
        let mut continuation = args.clone();
        continuation["projection"] = json!("call_sites");
        // Graph defaults to depth 20; call_sites defaults to depth 1. Preserve
        // the actual traversal depth when switching projection.
        continuation["depth"] = result["depth"].clone();
        continuation["offset"] = json!(0);
        continuation["limit"] = json!(100);
        if continuation["method_id"].is_null() {
            continuation["method_id"] = result["method_id"].clone();
        }
        result["continuation"] = json!({"tool":"get_call_graph","arguments":continuation,"scope":"Enumerate complete relationships as call-site pages from offset zero; graph nodes and edges are a bounded overview"});
        if serde_json::to_vec(result)?.len() <= budget {
            if result["truncated"] != true {
                result.as_object_mut().unwrap().remove("continuation");
            }
            return Ok(());
        }
        let nodes = std::mem::take(
            result["nodes"]
                .as_array_mut()
                .context("Missing graph nodes")?,
        );
        let edges = std::mem::take(
            result["edges"]
                .as_array_mut()
                .context("Missing graph edges")?,
        );
        let mut bytes = serde_json::to_vec(result)?.len() + 1024;
        let mut kept_nodes = Vec::new();
        let mut ids = std::collections::HashSet::new();
        let mut limited = false;
        for node in &nodes {
            let size = serde_json::to_vec(node)?.len() + 1;
            if !kept_nodes.is_empty() && bytes + size > budget / 2 {
                limited = true;
                break;
            }
            ensure!(
                bytes + size <= budget,
                "RESULT_TOO_LARGE: graph metadata/root exceed max_output_bytes; use projection=call_sites or increase the budget"
            );
            bytes += size;
            ids.insert(node["id"].as_u64().context("Missing node ID")?);
            kept_nodes.push(node.clone());
        }
        let mut kept_edges = Vec::new();
        for edge in &edges {
            if !ids.contains(&edge["from"].as_u64().context("Missing source node")?)
                || !ids.contains(&edge["to"].as_u64().context("Missing target node")?)
            {
                limited = true;
                continue;
            }
            let size = serde_json::to_vec(edge)?.len() + 1;
            if bytes + size > budget {
                limited = true;
                break;
            }
            bytes += size;
            kept_edges.push(edge.clone());
        }
        result["nodes"] = json!(kept_nodes);
        result["edges"] = json!(kept_edges);
        result["response_budget_limited"] = json!(limited);
        result["truncated"] = json!(limited || result["truncated"] == true);
        if result["truncated"] != true {
            result.as_object_mut().unwrap().remove("continuation");
        }
    }
    ensure!(
        serde_json::to_vec(result)?.len() <= budget,
        "RESULT_TOO_LARGE: response metadata exceeds max_output_bytes; use a smaller page or larger explicit budget"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_pages_reassemble_without_losing_filtered_positions() {
        let row = |i| json!({"result_offset":i,"data":"x".repeat(400)});
        let mut page = json!({"projection":"call_sites","offset":0,"call_sites":[row(0),row(2),row(4)],"excluded_call_sites":[row(1)],"unresolved_call_sites":[row(3)],"argument_filter":{"page_total":5},"next_offset":null});
        bound(&mut page, &json!({}), 2200).unwrap();
        let next = page["next_offset"].as_u64().unwrap();
        assert!(next > 0 && next < 5);
        let mut found = Vec::new();
        for key in ["call_sites", "excluded_call_sites", "unresolved_call_sites"] {
            for row in page[key].as_array().unwrap() {
                found.push(row["result_offset"].as_u64().unwrap());
            }
        }
        found.sort();
        assert_eq!(found, (0..next).collect::<Vec<_>>());
        assert_eq!(page["argument_filter"]["page_total"], next);
        assert!(serde_json::to_vec(&page).unwrap().len() <= 2200);
    }
    #[test]
    fn clipped_graph_keeps_valid_edges_and_supplies_complete_callsite_query() {
        let mut result = json!({"projection":"graph","depth":20,"method_id":"A.m()V","nodes":(0..100).map(|i|json!({"id":i,"method_id":format!("{}{}.m()V","Long".repeat(10),i)})).collect::<Vec<_>>(),"edges":(1..100).map(|i|json!({"from":0,"to":i})).collect::<Vec<_>>(),"truncated":false});
        bound(
            &mut result,
            &json!({"method_id":"A.m()V","direction":"callers"}),
            4096,
        )
        .unwrap();
        assert_eq!(result["truncated"], true);
        assert_eq!(
            result["continuation"]["arguments"]["projection"],
            "call_sites"
        );
        assert_eq!(result["continuation"]["arguments"]["depth"], 20);
        let count = result["nodes"].as_array().unwrap().len() as u64;
        assert!(
            result["edges"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["to"].as_u64().unwrap() < count)
        );
        assert!(serde_json::to_vec(&result).unwrap().len() <= 4096);
    }
    #[test]
    fn oversized_first_record_fails_instead_of_empty_continuation_loop() {
        let mut result = json!({"projection":"call_sites","call_sites":[{"result_offset":0,"data":"x".repeat(20000)}]});
        assert!(bound(&mut result, &json!({}), 16384).is_err());
    }
}
