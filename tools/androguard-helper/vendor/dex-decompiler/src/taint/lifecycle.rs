//! Android component lifecycle seeds (MT-compatible subset).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{DexDecompilerError, Result};

/// One lifecycle callback that should seed user-input taint on a parameter.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LifecycleSeed {
    /// Substring matched against `Class.method(proto)` or `Class.method`.
    pub patterns: Vec<String>,
    /// Parameter index (0 = this for instance methods).
    pub argument: u32,
    /// Taint kind to introduce.
    pub kind: String,
}

/// Embedded default lifecycle table.
pub fn default_lifecycle_seeds() -> Vec<LifecycleSeed> {
    serde_json::from_str(DEFAULT_LIFECYCLES_JSON).expect("embedded lifecycles must parse")
}

/// Load lifecycle seeds from a JSON file (same schema as `configuration/lifecycles.json`).
pub fn load_lifecycle_seeds(path: &Path) -> Result<Vec<LifecycleSeed>> {
    let data = std::fs::read_to_string(path).map_err(|e| {
        DexDecompilerError::Parse(format!("read lifecycle config {}: {e}", path.display()))
    })?;
    serde_json::from_str(&data).map_err(|e| {
        DexDecompilerError::Parse(format!("parse lifecycle config {}: {e}", path.display()))
    })
}

const DEFAULT_LIFECYCLES_JSON: &str = r#"
[
  {"patterns": [".onCreate(", "Activity.onCreate"], "argument": 1, "kind": "ActivityUserInput"},
  {"patterns": [".onStart(", "Activity.onStart", "Fragment.onStart"], "argument": 0, "kind": "ActivityUserInput"},
  {"patterns": [".onResume(", "Activity.onResume"], "argument": 0, "kind": "ActivityUserInput"},
  {"patterns": [".onNewIntent("], "argument": 1, "kind": "ActivityUserInput"},
  {"patterns": [".onActivityResult("], "argument": 3, "kind": "ActivityUserInput"},
  {"patterns": [".onStartCommand("], "argument": 1, "kind": "ActivityUserInput"},
  {"patterns": [".onBind("], "argument": 1, "kind": "ActivityUserInput"},
  {"patterns": [".onHandleIntent("], "argument": 1, "kind": "ActivityUserInput"},
  {"patterns": [".onReceive("], "argument": 2, "kind": "ReceiverUserInput"},
  {"patterns": ["ContentProvider.query", ".query("], "argument": 2, "kind": "ProviderUserInput"},
  {"patterns": ["ContentProvider.insert", ".insert("], "argument": 2, "kind": "ProviderUserInput"},
  {"patterns": ["ContentProvider.update", ".update("], "argument": 2, "kind": "ProviderUserInput"},
  {"patterns": ["ContentProvider.call", ".call("], "argument": 2, "kind": "ProviderUserInput"},
  {"patterns": ["Fragment.onCreate(", ".onCreate(Landroid/os/Bundle;)"], "argument": 1, "kind": "ActivityUserInput"},
  {"patterns": [".onViewCreated("], "argument": 2, "kind": "ActivityUserInput"},
  {"patterns": [".onClick("], "argument": 1, "kind": "UserInput"}
]
"#;

/// Match lifecycle seeds against a callable `Class.method` or `Class.method(proto)`.
pub fn lifecycle_seeds_for<'a>(
    callable: &str,
    table: &'a [LifecycleSeed],
) -> Vec<&'a LifecycleSeed> {
    table
        .iter()
        .filter(|s| s.patterns.iter().any(|p| callable.contains(p.as_str())))
        .collect()
}

/// Best-effort extraction of exported component class names from a **decoded**
/// (text) AndroidManifest.xml. Binary AXML is not decoded here — callers should
/// pass CLI `--taint-priority-entry` when only binary manifests are available.
pub fn exported_classes_from_manifest_xml(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = xml.to_ascii_lowercase();
    // Walk component-ish tags; accept android:exported="true" on the same open tag.
    for tag in [
        "activity",
        "activity-alias",
        "service",
        "receiver",
        "provider",
    ] {
        let open = format!("<{tag}");
        let mut search = 0;
        while let Some(rel) = lower[search..].find(&open) {
            let start = search + rel;
            let Some(end_rel) = lower[start..].find('>') else {
                break;
            };
            let end = start + end_rel;
            let chunk = &xml[start..=end];
            let chunk_l = chunk.to_ascii_lowercase();
            let exported = chunk_l.contains("android:exported=\"true\"")
                || chunk_l.contains("android:exported='true'");
            if exported {
                if let Some(name) = attr_value(chunk, "android:name")
                    .or_else(|| attr_value(chunk, "name"))
                {
                    let class = name.trim_start_matches('.');
                    if !class.is_empty() {
                        out.push(class.to_string());
                    }
                }
            }
            search = end + 1;
        }
    }
    out.sort();
    out.dedup();
    out
}

fn attr_value<'a>(chunk: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=\"");
    if let Some(i) = chunk.find(&needle) {
        let rest = &chunk[i + needle.len()..];
        return rest.split('"').next();
    }
    let needle = format!("{key}='");
    if let Some(i) = chunk.find(&needle) {
        let rest = &chunk[i + needle.len()..];
        return rest.split('\'').next();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_from_text_manifest() {
        let xml = r#"<?xml version="1.0"?>
<manifest package="com.ex">
  <application>
    <activity android:name=".MainActivity" android:exported="true"/>
    <receiver android:name="com.ex.MyReceiver" android:exported="false"/>
    <service android:name=".SyncService" android:exported="true"/>
  </application>
</manifest>"#;
        let classes = exported_classes_from_manifest_xml(xml);
        assert!(classes.iter().any(|c| c.contains("MainActivity")));
        assert!(classes.iter().any(|c| c.contains("SyncService")));
        assert!(!classes.iter().any(|c| c.contains("MyReceiver")));
    }
}
