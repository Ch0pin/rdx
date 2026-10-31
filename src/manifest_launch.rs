//! Manifest launch configuration, with declaration/default provenance.
//! Intent flags, task state and device behavior are outside this inventory.
use super::Node;
use serde_json::{Value, json};

pub(super) const FIELDS: &[&str] = &[
    "launchMode",
    "taskAffinity",
    "allowTaskReparenting",
    "documentLaunchMode",
    "excludeFromRecents",
];
fn parsed(key: &str, raw: &str) -> Option<Value> {
    if raw.starts_with(['@', '?']) {
        return None;
    }
    match key {
        "allowTaskReparenting" | "excludeFromRecents" => match raw {
            "true" => Some(json!(true)),
            "false" => Some(json!(false)),
            _ => None,
        },
        "launchMode" | "documentLaunchMode" => {
            let choices: &[&str] = if key == "launchMode" {
                &[
                    "standard",
                    "singleTop",
                    "singleTask",
                    "singleInstance",
                    "singleInstancePerTask",
                ]
            } else {
                &["none", "intoExisting", "always", "never"]
            };
            if choices.contains(&raw) {
                return Some(json!(raw));
            }
            let number = raw
                .strip_prefix("0x")
                .and_then(|s| usize::from_str_radix(s, 16).ok())
                .or_else(|| raw.parse().ok())?;
            choices.get(number).map(|s| json!(s))
        }
        "taskAffinity" => Some(json!(raw)),
        _ => None,
    }
}
pub(super) fn attach(
    record: &mut Value,
    nodes: &[Node],
    index: usize,
    app_index: usize,
    target: Option<usize>,
) {
    let node = &nodes[index];
    if !matches!(node.tag.as_str(), "activity" | "activity-alias") {
        return;
    }
    let alias = node.tag == "activity-alias";
    record["targetActivity"] = if alias {
        record["target"].clone()
    } else {
        Value::Null
    };
    for key in FIELDS {
        let owner = if alias { target } else { Some(index) };
        let Some(owner) = owner else {
            record[*key] = json!({"declared":node.get(key),"value":null,"source":"unresolved_target_activity","resolved":false});
            continue;
        };
        let declared = nodes[owner].get(key);
        let application = matches!(*key, "taskAffinity" | "allowTaskReparenting")
            .then(|| nodes[app_index].get(key))
            .flatten();
        let (raw, source, source_index) = if let Some(value) = declared {
            (Some(value), "activity", Some(owner))
        } else if let Some(value) = application {
            (Some(value), "application", Some(app_index))
        } else {
            (None, "default", None)
        };
        let value = if let Some(raw) = raw {
            parsed(key, raw)
        } else {
            match *key {
                "launchMode" => Some(json!("standard")),
                "documentLaunchMode" => Some(json!("none")),
                "allowTaskReparenting" | "excludeFromRecents" => Some(json!(false)),
                "taskAffinity" => nodes[0]
                    .get("raw:package")
                    .filter(|s| !s.is_empty())
                    .map(|s| json!(s)),
                _ => None,
            }
        };
        let mut state = json!({"declared":if alias {None} else {declared},"raw":raw,"value":value,
            "source":source,"source_node_index":source_index,"resolved":value.is_some(),
            "scope":"manifest configuration with defaults; runtime launch behavior is not inferred"});
        if alias {
            state["source"] = json!("target_activity");
            state["inherited_source"] = json!(source);
            state["inherited_from"] = record["target"].clone();
        }
        record[*key] = state;
    }
}

#[cfg(test)]
mod tests {
    use super::super::{project_records, query_components, query_resolved};
    use crate::resource_table::{Resource, ResourceTable, Variant};
    use serde_json::{Value, json};
    const XML: &str = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="sample"><application a:taskAffinity="sample.shared" a:allowTaskReparenting="true"><activity a:name=".Main" a:launchMode="singleTop" a:documentLaunchMode="never" a:excludeFromRecents="true"/><activity-alias a:name=".Alias" a:targetActivity=".Main"/><activity a:name=".Empty" a:taskAffinity="" a:allowTaskReparenting="false"/><activity a:name=".Unknown" a:launchMode="@integer/missing"/><activity-alias a:name=".Missing" a:targetActivity=".Absent"/><service a:name=".Service"/></application></manifest>"#;
    #[test]
    fn launch_fields_keep_defaults_inheritance_empty_affinity_and_aliases_distinct() {
        let rows = query_components(XML, &json!({})).unwrap();
        let named = |name: &str| rows.iter().find(|r| r["name"] == name).unwrap();
        assert_eq!(named("sample.Main")["launchMode"]["value"], "singleTop");
        assert_eq!(
            named("sample.Main")["taskAffinity"]["source"],
            "application"
        );
        assert_eq!(named("sample.Main")["allowTaskReparenting"]["value"], true);
        assert_eq!(named("sample.Alias")["targetActivity"], "sample.Main");
        assert_eq!(named("sample.Alias")["launchMode"]["value"], "singleTop");
        assert_eq!(
            named("sample.Alias")["launchMode"]["source"],
            "target_activity"
        );
        assert_eq!(named("sample.Empty")["taskAffinity"]["value"], "");
        assert_eq!(
            named("sample.Empty")["allowTaskReparenting"]["value"],
            false
        );
        assert_eq!(named("sample.Empty")["launchMode"]["value"], "standard");
        assert_eq!(named("sample.Empty")["documentLaunchMode"]["value"], "none");
        assert_eq!(named("sample.Unknown")["launchMode"]["resolved"], false);
        assert_eq!(
            named("sample.Missing")["launchMode"]["source"],
            "unresolved_target_activity"
        );
        assert!(named("sample.Service").get("launchMode").is_none());
        let mut projected = rows;
        project_records(
            &mut projected,
            &json!({"fields":"name,launchMode,exported,taskAffinity,targetActivity"}),
        );
        assert!(projected[0]["launchMode"].is_object());
        assert!(projected[0].get("details").is_none());
        let defaults=query_components(r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="sample"><application><activity a:name=".Default"/></application></manifest>"#,&json!({})).unwrap();
        assert_eq!(defaults[0]["taskAffinity"]["value"], "sample");
    }
    #[test]
    fn resolved_launch_resources_keep_raw_references_and_source_provenance() {
        let mut table = ResourceTable::default();
        table.entries.insert(
            1,
            Resource {
                id: 1,
                package: "sample".into(),
                kind: "integer".into(),
                name: "mode".into(),
                variants: vec![Variant {
                    configuration: "default".into(),
                    value: "2".into(),
                    file: None,
                    reference: None,
                }],
            },
        );
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="sample"><application><activity a:name=".Main" a:launchMode="@integer/mode"/><activity-alias a:name=".Alias" a:targetActivity=".Main"/></application></manifest>"#;
        for component_mode in [true, false] {
            let mut rows = query_resolved(xml, &json!({}), &table, component_mode).unwrap();
            let main = rows.iter().find(|r| r["name"] == "sample.Main").unwrap();
            assert_eq!(main["launchMode"]["value"], "singleTask");
            assert_eq!(main["launchMode"]["declared"], "@integer/mode");
            let alias = rows.iter().find(|r| r["name"] == "sample.Alias").unwrap();
            assert_eq!(alias["launchMode"]["raw"], "@integer/mode");
            assert_eq!(alias["launchMode"]["declared"], Value::Null);
            project_records(&mut rows, &json!({"projection":"compact"}));
            assert_eq!(
                rows.iter().find(|r| r["name"] == "sample.Alias").unwrap()["launchMode"]["value"],
                "singleTask"
            );
        }
    }
    #[test]
    fn alias_requires_a_unique_preceding_target() {
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="sample"><application><activity-alias a:name=".Alias" a:targetActivity=".Late"/><activity a:name=".Late" a:launchMode="singleTask"/></application></manifest>"#;
        let rows = query_components(xml, &json!({})).unwrap();
        assert_eq!(rows[0]["launchMode"]["resolved"], false);
    }
}
