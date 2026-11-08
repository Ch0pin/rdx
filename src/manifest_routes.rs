//! Literal declaration matching and bounded, non-authoritative near-match hints.
use super::{Node, parse, query_components_with_routes, query_with_routes};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const FIELDS: [&str; 3] = ["host", "scheme", "path"];
const MAX_HINT_XML: usize = 1024 * 1024;
const MAX_HINT_COMPONENTS: usize = 1000;

#[derive(Clone, Copy)]
pub(super) struct Relaxation {
    field: &'static str,
    fold_ascii: bool,
}

pub(super) fn validate(args: &Value) -> Result<(), String> {
    if let Some(mode) = args.get("route_match_mode")
        && !matches!(mode.as_str(), Some("exact" | "contains"))
    {
        return Err("route_match_mode must be exact or contains".into());
    }
    Ok(())
}
fn mode<'a>(args: &'a Value, field: &str) -> &'a str {
    args["route_match_mode"]
        .as_str()
        .unwrap_or(if field == "path" { "contains" } else { "exact" })
}
fn compare(value: &str, query: &str, matching: &str, fold_ascii: bool) -> bool {
    if fold_ascii {
        return value
            .to_ascii_lowercase()
            .contains(&query.to_ascii_lowercase());
    }
    if matching == "contains" {
        value.contains(query)
    } else {
        value == query
    }
}
fn field_matches(node: &Node, field: &str, query: &str, matching: &str, folded: bool) -> bool {
    if field == "path" {
        node.attrs
            .iter()
            .any(|(key, value)| key.starts_with("path") && compare(value, query, matching, folded))
    } else {
        node.get(field)
            .is_some_and(|value| compare(value, query, matching, folded))
    }
}
pub(super) fn matches(data: &[&Node], args: &Value, relaxed: Option<Relaxation>) -> bool {
    FIELDS.iter().all(|field| {
        args[*field].as_str().is_none_or(|query| {
            let override_field = relaxed.filter(|r| r.field == *field);
            data.iter().any(|node| {
                field_matches(
                    node,
                    field,
                    query,
                    if override_field.is_some() {
                        "contains"
                    } else {
                        mode(args, field)
                    },
                    override_field.is_some_and(|r| r.fold_ascii),
                )
            })
        })
    })
}

pub(crate) fn effective_matching(args: &Value) -> Value {
    let mut rules = serde_json::Map::new();
    for field in [
        "class_name",
        "component",
        "component_type",
        "permission",
        "query",
        "host",
        "scheme",
        "path",
        "exported_state",
    ] {
        if args.get(field).is_none() {
            continue;
        }
        let matching = if FIELDS.contains(&field) {
            mode(args, field)
        } else {
            match field {
                "class_name" => args["match_mode"].as_str().unwrap_or("contains"),
                "component_type" | "exported_state" => "exact",
                _ => "contains",
            }
        };
        rules.insert(field.into(),json!({"mode":matching,"case_sensitive":field!="query",
            "scope":if FIELDS.contains(&field) {"literal declared attribute; same intent-filter"} else {"component record"}}));
    }
    Value::Object(rules)
}
pub(crate) fn exported_matches(record: &Value, args: &Value) -> bool {
    match args["exported_state"].as_str() {
        None => true,
        Some("true") => record["exported_state"]["effective"] == true,
        Some("false") => record["exported_state"]["effective"] == false,
        Some("unresolved") => record["exported_state"]["effective"].is_null(),
        _ => false,
    }
}

fn examples(records: &[Value], field: &str, query: &str, folded: bool) -> Vec<Value> {
    records.iter().take(5).map(|record| {
        let mut values=Vec::<String>::new();
        // Only values in a matching intent-filter qualify for the hint.
        for i in record["matching_filter_indexes"].as_array().into_iter().flatten().filter_map(Value::as_u64) {
            for element in record["intent_filters"][i as usize]["elements"].as_array().into_iter().flatten() {
                let nodes=std::iter::once(element).chain(element["children"].as_array().into_iter().flatten());
                for node in nodes.filter(|node|node["tag"]=="data") {
                    for pair in node["attributes"].as_array().into_iter().flatten() {
                        if let (Some(key),Some(value))=(pair[0].as_str(),pair[1].as_str())
                            && (key==field || (field=="path" && key.starts_with("path")))
                            && compare(value,query,"contains",folded) && values.len()<3 {
                            values.push(value.chars().take(256).collect());
                        }
                    }
                }
            }
        }
        let name=record["name"].as_str().unwrap_or("");
        json!({"name":name.chars().take(512).collect::<String>(),"name_truncated":name.chars().count()>512,
            "component_type":record["component_type"],"declared_value_previews":values})
    }).collect()
}

/// Called only when the full filtered record count is zero, never for an empty page.
/// Input is already resource-resolved; class/type/permission/query filters stay fixed.
pub(crate) fn near_matches(
    xml: &str,
    args: &Value,
    component_mode: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Value, String> {
    validate(args)?;
    let check = || {
        if cancelled() {
            Err("CANCELLED".to_owned())
        } else {
            Ok(())
        }
    };
    check()?;
    if !FIELDS.iter().any(|field| args[*field].is_string()) {
        return Ok(json!({"status":"not_applicable","warnings":[]}));
    }
    if xml.len() > MAX_HINT_XML {
        return Ok(
            json!({"status":"skipped","reason":"manifest exceeds near-match byte budget","max_manifest_bytes":MAX_HINT_XML,"warnings":[]}),
        );
    }
    let components = parse(xml)?
        .iter()
        .filter(|n| {
            matches!(
                n.tag.as_str(),
                "activity" | "activity-alias" | "receiver" | "service" | "provider"
            )
        })
        .count();
    if components > MAX_HINT_COMPONENTS {
        return Ok(
            json!({"status":"skipped","reason":"manifest exceeds near-match component budget","max_components":MAX_HINT_COMPONENTS,"warnings":[]}),
        );
    }
    let mut warnings = Vec::new();
    for field in FIELDS {
        let Some(query) = args[field].as_str() else {
            continue;
        };
        for folded in [false, true] {
            if !folded && mode(args, field) == "contains" {
                continue;
            }
            check()?;
            let relaxation = Some(Relaxation {
                field,
                fold_ascii: folded,
            });
            let records = if component_mode {
                query_components_with_routes(xml, args, relaxation)?
            } else {
                query_with_routes(xml, args, relaxation)?
            };
            let unique: BTreeMap<_, _> = records
                .into_iter()
                .filter(|r| exported_matches(r, args))
                .map(|r| {
                    (
                        (
                            r["component_type"].as_str().unwrap_or("").to_owned(),
                            r["name"].as_str().unwrap_or("").to_owned(),
                        ),
                        r,
                    )
                })
                .collect();
            if unique.is_empty() {
                continue;
            }
            let records: Vec<_> = unique.into_values().collect();
            let count = records.len();
            warnings.push(json!({"code":"MANIFEST_ROUTE_NEAR_MATCH","field":field,
                "message":format!("0 requested matches; {count} component(s) match when only {field} uses {}substring matching",if folded {"ASCII-case-insensitive "}else{"case-sensitive "}),
                "requested_mode":mode(args,field),"relaxed_mode":"contains","relaxed_case_sensitive":!folded,"case_folding":if folded {"ascii"} else {"none"},
                "matching_components":count,"other_filters_preserved":true,"examples":examples(&records,field,query,folded),
                "examples_truncated":count>5,"hint":if folded {"Use the declared spelling shown in the examples. route_match_mode=contains enables case-sensitive substring searches for all route fields."} else {"For substring route searches, set route_match_mode=contains; it applies to host, scheme and path."},
                "scope":"Declaration-search hints only; requested results are unchanged and runtime route handling is not established"}));
            break;
        }
    }
    check()?;
    Ok(
        json!({"status":"complete","max_examples_per_field":5,"max_value_preview_chars":256,"max_name_preview_chars":512,"warnings":warnings}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest_summary::{query, query_components, query_resolved, resolved_xml};
    use crate::resource_table::{Resource, ResourceTable, Variant};
    const XML: &str = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="sample"><application>
      <activity a:name=".Mail" a:permission="sample.OPEN" a:exported="true"><intent-filter><data a:scheme="https" a:host="ashleymadison.com" a:pathPrefix="/inbox"/></intent-filter><intent-filter><data a:scheme="other" a:host="separate.test" a:path="/elsewhere"/></intent-filter></activity>
      <activity-alias a:name=".Alias" a:targetActivity=".Mail" a:exported="false"><intent-filter><data a:scheme="https" a:host="alias.test" a:path="/alias"/></intent-filter></activity-alias>
      <service a:name=".Service" a:exported="false"><intent-filter><data a:scheme="https" a:host="ashleymadison.com" a:pathPrefix="/inbox"/></intent-filter></service>
    </application></manifest>"#;
    #[test]
    fn route_modes_preserve_defaults_and_class_selection_independence() {
        let args = json!({"component_type":"activity","class_name":".Mail","match_mode":"exact"});
        for (field, value) in [("host", "madison"), ("scheme", "ttp"), ("path", "inbo")] {
            let mut q = args.clone();
            q[field] = json!(value);
            assert_eq!(
                query_components(XML, &q).unwrap().len(),
                usize::from(field == "path")
            );
            q["route_match_mode"] = json!("contains");
            assert_eq!(query_components(XML, &q).unwrap().len(), 1);
            q["route_match_mode"] = json!("exact");
            assert!(query_components(XML, &q).unwrap().is_empty());
        }
        assert_eq!(query_components(XML,&json!({"host":"ashleymadison.com","path":"/inbox","route_match_mode":"exact","component_type":"activity"})).unwrap().len(),1);
        let rules = effective_matching(
            &json!({"host":"madison","path":"inbo","class_name":".Mail","match_mode":"exact"}),
        );
        assert_eq!(rules["host"]["mode"], "exact");
        assert_eq!(rules["path"]["mode"], "contains");
        assert_eq!(rules["class_name"]["mode"], "exact");
        assert_eq!(rules["host"]["case_sensitive"], true);
        for q in [
            json!({"route_match_mode":"regex"}),
            json!({"route_match_mode":null}),
        ] {
            assert!(query_components(XML, &q).is_err());
            assert!(query(XML, &q).is_err());
        }
    }
    #[test]
    fn hints_relax_one_field_and_preserve_every_other_constraint() {
        let args = json!({"host":"madison","scheme":"https","path":"inbo","component_type":"activity","class_name":".Mail","match_mode":"exact","permission":"sample.OPEN","query":"inbox","exported_state":"true"});
        let result = near_matches(XML, &args, true, || false).unwrap();
        assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
        let hint = &result["warnings"][0];
        assert_eq!(hint["field"], "host");
        assert_eq!(hint["matching_components"], 1);
        assert_eq!(
            hint["examples"][0]["declared_value_previews"],
            json!(["ashleymadison.com"])
        );
        assert!(query_components(XML, &args).unwrap().is_empty());
        for (field, value) in [
            ("scheme", "other"),
            ("path", "elsewhere"),
            ("permission", "missing"),
            ("class_name", ".Absent"),
            ("query", "absent-text"),
            ("exported_state", "false"),
        ] {
            let mut q = args.clone();
            q[field] = json!(value);
            assert!(
                near_matches(XML, &q, true, || false).unwrap()["warnings"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "{q}"
            );
        }
        // A class match on an alias target must not borrow that target's routes.
        let q = json!({"class_name":".Mail","match_mode":"exact","component_type":"activity-alias","host":"madison"});
        assert!(
            near_matches(XML, &q, true, || false).unwrap()["warnings"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn casefold_hints_are_separate_from_case_sensitive_results() {
        let q = json!({"component_type":"activity","host":"MADISON","route_match_mode":"contains"});
        assert!(query_components(XML, &q).unwrap().is_empty());
        let hint = near_matches(XML, &q, true, || false).unwrap();
        assert_eq!(hint["warnings"][0]["matching_components"], 1);
        assert_eq!(hint["warnings"][0]["relaxed_case_sensitive"], false);
        assert_eq!(hint["warnings"][0]["case_folding"], "ascii");
        assert!(
            near_matches(XML, &q, true, || true)
                .unwrap_err()
                .contains("CANCELLED")
        );
    }
    #[test]
    fn summary_and_components_share_literal_route_semantics() {
        for component in [false, true] {
            let q = json!({"component_type":"activity","host":"madison","scheme":"https"});
            assert_eq!(
                near_matches(XML, &q, component, || false).unwrap()["warnings"][0]["matching_components"],
                1
            );
            let mut contains = q;
            contains["route_match_mode"] = json!("contains");
            let rows = if component {
                query_components(XML, &contains)
            } else {
                query(XML, &contains)
            }
            .unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["matching_filter_indexes"], json!([0]));
        }
    }
    #[test]
    fn hints_are_bounded_and_deduplicate_components() {
        let repeated=XML.replace("</intent-filter><intent-filter><data a:scheme=\"other\"", "<data a:host=\"ashleymadison.com\"/></intent-filter><intent-filter><data a:scheme=\"other\"");
        assert_eq!(
            near_matches(
                &repeated,
                &json!({"host":"madison","component_type":"activity"}),
                true,
                || false
            )
            .unwrap()["warnings"][0]["matching_components"],
            1
        );
        let xml=format!("<manifest xmlns:a=\"http://schemas.android.com/apk/res/android\" package=\"sample\"><application>{}</application></manifest>",(0..12).map(|i|format!("<activity a:name=\".C{i}\"><intent-filter><data a:host=\"example.test\"/></intent-filter></activity>")).collect::<String>());
        let result = near_matches(&xml, &json!({"host":"example"}), true, || false).unwrap();
        assert_eq!(result["warnings"][0]["matching_components"], 12);
        assert_eq!(
            result["warnings"][0]["examples"].as_array().unwrap().len(),
            5
        );
        assert_eq!(result["warnings"][0]["examples_truncated"], true);
        let many = format!(
            "<manifest><application>{}</application></manifest>",
            "<activity/>".repeat(MAX_HINT_COMPONENTS + 1)
        );
        assert_eq!(
            near_matches(&many, &json!({"host":"example"}), true, || false).unwrap()["max_components"],
            MAX_HINT_COMPONENTS
        );
        let oversized = " ".repeat(MAX_HINT_XML + 1);
        assert_eq!(
            near_matches(&oversized, &json!({"host":"example"}), true, || false).unwrap()["status"],
            "skipped"
        );
    }
    #[test]
    fn resource_resolution_precedes_route_matching_and_hints() {
        let mut table = ResourceTable::default();
        table.entries.insert(
            1,
            Resource {
                id: 1,
                package: "sample".into(),
                kind: "string".into(),
                name: "host".into(),
                variants: vec![Variant {
                    configuration: "default".into(),
                    value: "ashleymadison.com".into(),
                    file: None,
                    reference: None,
                }],
            },
        );
        let xml = XML.replace("ashleymadison.com", "@string/host");
        let q = json!({"component_type":"activity","host":"madison","route_match_mode":"contains"});
        assert_eq!(query_resolved(&xml, &q, &table, true).unwrap().len(), 1);
        let resolved = resolved_xml(&xml, &table).unwrap().0;
        let hints = near_matches(
            &resolved,
            &json!({"component_type":"activity","host":"madison"}),
            true,
            || false,
        )
        .unwrap();
        assert_eq!(hints["warnings"][0]["matching_components"], 1);
    }
}
