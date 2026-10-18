//! Human-readable inventory from the decoded manifest. This describes declarations,
//! not runtime reachability or domain verification.
use crate::resource_table::ResourceTable;
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

const ANDROID: &str = "http://schemas.android.com/apk/res/android";

#[derive(Default)]
struct Node {
    tag: String,
    attrs: Vec<(String, String)>,
    parent: Option<usize>,
}
impl Node {
    fn get(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn parse(xml: &str) -> Result<Vec<Node>, String> {
    let mut reader = NsReader::from_str(xml);
    let mut nodes = Vec::new();
    let mut stack = Vec::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                let empty = matches!(event, Event::Empty(_));
                let element = match event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                let (ns, local) = reader.resolver().resolve_element(element.name());
                let tag = if matches!(ns, ResolveResult::Unbound) {
                    local.as_ref().to_owned()
                } else {
                    String::new()
                };
                let mut node = Node {
                    tag,
                    parent: stack.last().copied(),
                    ..Default::default()
                };
                for attr in element.attributes() {
                    let attr = attr.map_err(|e| e.to_string())?;
                    let (ns, local) = reader.resolver().resolve_attribute(attr.key);
                    let key = if matches!(ns, ResolveResult::Bound(uri) if uri.as_ref() == ANDROID)
                    {
                        local.as_ref().to_owned()
                    } else if matches!(ns, ResolveResult::Unbound) {
                        format!("raw:{}", local.as_ref())
                    } else {
                        continue;
                    };
                    let value = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| e.to_string())?
                        .into_owned();
                    node.attrs.push((key, value));
                }
                let index = nodes.len();
                nodes.push(node);
                if !empty {
                    stack.push(index);
                }
            }
            Event::End(_) => {
                stack.pop().ok_or("Unexpected closing tag")?;
            }
            Event::DocType(_) | Event::GeneralRef(_) => {
                return Err("DTD/entity declarations are unsupported".into());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() || nodes.first().is_none_or(|n| n.tag != "manifest") {
        return Err("Invalid manifest structure".into());
    }
    Ok(nodes)
}

fn child<'a>(nodes: &'a [Node], parent: usize, tag: &str) -> Option<&'a Node> {
    nodes
        .iter()
        .find(|n| n.parent == Some(parent) && n.tag == tag)
}

/// Only application-level Android attributes identify the launcher artwork.
pub(crate) fn application_icons(xml: &str) -> Option<(String, Vec<String>)> {
    let nodes = parse(xml).ok()?;
    let application = child(&nodes, 0, "application")?;
    Some((
        nodes[0].get("raw:package").unwrap_or_default().to_owned(),
        ["icon", "roundIcon"]
            .into_iter()
            .filter_map(|key| application.get(key).map(str::to_owned))
            .collect(),
    ))
}
fn value(node: &Node, key: &str) -> String {
    node.get(key).unwrap_or("Not declared").to_owned()
}
fn raw(node: &Node, key: &str) -> String {
    node.get(&format!("raw:{key}"))
        .unwrap_or("Not declared")
        .to_owned()
}
fn bool_value(node: &Node, key: &str, default: Option<bool>) -> String {
    match node.get(key) {
        Some("true") => "true (explicit)".into(),
        Some("false") => "false (explicit)".into(),
        Some(v) => format!("{v} (unresolved)"),
        None => default
            .map(|v| format!("{v} (default)"))
            .unwrap_or_else(|| "Not declared".into()),
    }
}
fn class_name(package: &str, name: &str) -> String {
    if name.starts_with('.') {
        format!("{package}{name}")
    } else if !name.contains('.') && !package.is_empty() && !name.starts_with(['@', '?']) {
        format!("{package}.{name}")
    } else {
        name.into()
    }
}
fn attr_line(out: &mut String, name: &str, val: String) {
    out.push_str(&format!("| {name} | {} |\n", escape_inline(&val)));
}
fn escape_inline(value: &str) -> String {
    let mut result = String::new();
    for ch in value.chars() {
        match ch {
            '\n' | '\r' => result.push_str("<br>"),
            '\\' | '|' | '*' | '_' | '[' | ']' | '`' | '<' | '>' => {
                result.push('\\');
                result.push(ch);
            }
            c if c.is_control() => result.push(' '),
            c => result.push(c),
        }
    }
    result
}
fn code_block(out: &mut String, content: &str) {
    for line in content.replace('\r', "\n").lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
}

fn resolve_label(raw: &str, package: &str, table: &ResourceTable) -> String {
    let Some(reference) = raw.strip_prefix('@') else {
        return raw.into();
    };
    let resource = if let Some(hex) = reference.strip_prefix("0x") {
        u32::from_str_radix(hex, 16)
            .ok()
            .and_then(|id| table.entries.get(&id))
    } else if let Some((type_part, name)) = reference.rsplit_once('/') {
        let (owner, kind) = type_part.split_once(':').unwrap_or((package, type_part));
        table
            .entries
            .values()
            .find(|r| r.package == owner && r.kind == kind && r.name == name)
    } else {
        None
    };
    let Some(resource) = resource else {
        return format!("{raw} (unresolved)");
    };
    let mut seen = std::collections::HashSet::new();
    let mut current = resource.id;
    for _ in 0..8 {
        if !seen.insert(current) {
            break;
        }
        let Some(entry) = table.entries.get(&current) else {
            break;
        };
        let Some(variant) = entry.variants.iter().find(|v| v.configuration == "default") else {
            return format!("{raw} (no default resource value)");
        };
        if let Some(next) = variant.reference {
            current = next;
            continue;
        }
        return if entry.variants.len() > 1 {
            format!(
                "{} (default resource value; other configurations exist)",
                variant.value
            )
        } else {
            variant.value.clone()
        };
    }
    format!("{raw} (unresolved reference)")
}

fn protection_level(raw: &str) -> String {
    let parsed = raw
        .strip_prefix("0x")
        .and_then(|n| u32::from_str_radix(n, 16).ok())
        .or_else(|| raw.parse::<u32>().ok());
    let Some(bits) = parsed else {
        return raw.into();
    };
    let base = match bits & 0xf {
        0 => "normal",
        1 => "dangerous",
        2 => "signature",
        3 => "signatureOrSystem",
        _ => "unknown base",
    };
    let mut names = vec![base];
    for (mask, name) in [
        (0x10, "privileged"),
        (0x20, "development"),
        (0x40, "appop"),
        (0x80, "pre23"),
        (0x100, "installer"),
        (0x200, "verifier"),
        (0x400, "preinstalled"),
        (0x800, "setup"),
        (0x1000, "instant"),
        (0x2000, "runtimeOnly"),
        (0x4000, "oem"),
        (0x8000, "vendorPrivileged"),
    ] {
        if bits & mask != 0 {
            names.push(name);
        }
    }
    format!("{} ({raw})", names.join(" | "))
}

pub fn sha256(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut file = BufReader::new(file);
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn package_name(xml: &str) -> Result<String, String> {
    parse(xml)?[0]
        .get("raw:package")
        .map(str::to_owned)
        .ok_or_else(|| "Missing package name".into())
}

/// Export defaults for Android API 17+, shared by the report and component table.
fn exported_value(component: &Node, target: Option<u32>, has_filters: bool) -> String {
    match component.get("exported") {
        Some("true") => "true (explicit)".into(),
        Some("false") => "false (explicit)".into(),
        Some(v) => format!("{v} (unresolved)"),
        None if component.tag == "provider" => target
            .map(|t| format!("{} (inferred from target SDK {t})", t <= 16))
            .unwrap_or_else(|| "Unresolved (target SDK)".into()),
        None if has_filters && target.is_none() => "Unresolved (target SDK)".into(),
        None if has_filters && target.is_some_and(|t| t >= 31) => {
            "Unresolved (missing exported on target SDK 31+)".into()
        }
        None => format!("{} (inferred from intent filters)", has_filters),
    }
}

#[derive(Clone, Debug)]
pub struct ManifestComponent {
    pub kind: String,
    pub name: String,
    pub target: String,
    pub exported: String,
    pub enabled: String,
    pub launcher: bool,
    pub filters: Vec<String>,
    pub details: Vec<(String, String)>,
}
impl ManifestComponent {
    pub fn is_exported(&self) -> bool {
        self.exported.starts_with("true")
    }
}

/// Structured declarations; unresolved resource flags stay explicitly unknown.
pub fn components(xml: &str) -> Result<Vec<ManifestComponent>, String> {
    let nodes = parse(xml)?;
    let package = nodes[0].get("raw:package").unwrap_or_default();
    let sdk = child(&nodes, 0, "uses-sdk");
    let target = sdk
        .and_then(|n| n.get("targetSdkVersion"))
        .or_else(|| sdk.and_then(|n| n.get("minSdkVersion")))
        .unwrap_or("1")
        .parse::<u32>()
        .ok();
    let Some(app_index) = nodes
        .iter()
        .position(|n| n.parent == Some(0) && n.tag == "application")
    else {
        return Ok(Vec::new());
    };
    let app = &nodes[app_index];
    let mut result = Vec::new();
    for (index, node) in nodes.iter().enumerate().filter(|(_, n)| {
        n.parent == Some(app_index)
            && matches!(
                n.tag.as_str(),
                "activity" | "activity-alias" | "service" | "receiver" | "provider"
            )
    }) {
        let name = class_name(package, node.get("name").unwrap_or("Not declared"));
        let mut launcher = false;
        let mut filters = Vec::new();
        for (filter_index, filter) in nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.parent == Some(index) && n.tag == "intent-filter")
        {
            let children: Vec<_> = nodes
                .iter()
                .filter(|n| n.parent == Some(filter_index))
                .collect();
            if matches!(node.tag.as_str(), "activity" | "activity-alias") {
                launcher |= children.iter().any(|n| {
                    n.tag == "action" && n.get("name") == Some("android.intent.action.MAIN")
                }) && children.iter().any(|n| {
                    n.tag == "category"
                        && matches!(
                            n.get("name"),
                            Some(
                                "android.intent.category.LAUNCHER"
                                    | "android.intent.category.LEANBACK_LAUNCHER"
                            )
                        )
                });
            }
            let mut lines = Vec::new();
            for (key, val) in &filter.attrs {
                lines.push(format!("{key}: {val}"));
            }
            // Keep each data element intact rather than inventing URL combinations.
            for child in children {
                lines.push(format!(
                    "{}: {}",
                    child.tag,
                    child
                        .attrs
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            filters.push(lines.join("\n"));
        }
        let component_enabled = bool_value(node, "enabled", Some(true));
        let app_enabled = bool_value(app, "enabled", Some(true));
        let enabled = if component_enabled.starts_with("false") || app_enabled.starts_with("false")
        {
            "false".to_owned()
        } else if component_enabled.starts_with("true") && app_enabled.starts_with("true") {
            "true".to_owned()
        } else {
            "unresolved".to_owned()
        };
        let mut details = vec![
            ("Component enabled".into(), component_enabled),
            ("Application enabled".into(), app_enabled),
        ];
        for (key, val) in &node.attrs {
            details.push((key.clone(), val.clone()));
        }
        if node.get("permission").is_none()
            && node.tag != "activity-alias"
            && let Some(permission) = app.get("permission")
        {
            details.push(("Inherited application permission".into(), permission.into()));
        }
        let target_class = if node.tag == "activity-alias" {
            node.get("targetActivity")
                .map(|n| class_name(package, n))
                .unwrap_or_default()
        } else {
            name.clone()
        };
        result.push(ManifestComponent {
            kind: node.tag.clone(),
            name,
            target: target_class,
            exported: exported_value(node, target, !filters.is_empty()),
            enabled,
            launcher,
            filters,
            details,
        });
    }
    Ok(result)
}

/// Filter manifest declarations before pagination. Route predicates are evaluated
/// within one intent-filter, while each original data element stays separate.
pub fn query(xml: &str, args: &serde_json::Value) -> Result<Vec<serde_json::Value>, String> {
    use serde_json::json;
    let nodes = parse(xml)?;
    let components = components(xml)?;
    let mut children_of = vec![Vec::new(); nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        if let Some(parent) = node.parent {
            children_of[parent].push(i);
        }
    }
    let mut records = Vec::new();
    let component_only = ["component_type", "component", "host", "scheme", "path"]
        .iter()
        .any(|key| args.get(*key).is_some());
    let has_routes = ["host", "scheme", "path"]
        .iter()
        .any(|key| args.get(*key).is_some());
    let contains = |key: &str, value: &str| args[key].as_str().is_none_or(|q| value.contains(q));
    let matches_query = |record: &serde_json::Value| {
        args["query"].as_str().is_none_or(|q| {
            record
                .to_string()
                .to_lowercase()
                .contains(&q.to_lowercase())
        })
    };
    let app = nodes
        .iter()
        .position(|n| n.parent == Some(0) && n.tag == "application");
    let mut declarations = components.into_iter();
    for (index, node) in nodes.iter().enumerate() {
        if app.is_some()
            && node.parent == app
            && matches!(
                node.tag.as_str(),
                "activity" | "activity-alias" | "service" | "receiver" | "provider"
            )
        {
            let c = declarations.next().ok_or("Component ordering mismatch")?;
            if args["component_type"].as_str().is_some_and(|k| k != c.kind)
                || !contains("component", &c.name)
            {
                continue;
            }
            if let Some(q) = args["permission"].as_str()
                && !c
                    .details
                    .iter()
                    .any(|(k, v)| k.to_lowercase().contains("permission") && v.contains(q))
            {
                continue;
            }
            let mut filters = Vec::new();
            let mut matching_filter_indexes = Vec::new();
            for fi in children_of[index]
                .iter()
                .copied()
                .filter(|i| nodes[*i].tag == "intent-filter")
            {
                let filter = &nodes[fi];
                let groups: Vec<_> = children_of[fi]
                    .iter()
                    .copied()
                    .filter(|i| nodes[*i].tag == "uri-relative-filter-group")
                    .collect();
                let data: Vec<_> = children_of[fi]
                    .iter()
                    .copied()
                    .chain(groups.iter().flat_map(|g| children_of[*g].iter().copied()))
                    .map(|i| &nodes[i])
                    .filter(|n| n.tag == "data")
                    .collect();
                let matches = ["host", "scheme", "path"].iter().all(|key| {
                    args[*key].as_str().is_none_or(|q| {
                        data.iter().any(|d| {
                            if *key == "path" {
                                d.attrs
                                    .iter()
                                    .any(|(k, v)| k.starts_with("path") && v.contains(q))
                            } else {
                                d.get(key) == Some(q)
                            }
                        })
                    })
                });
                if matches {
                    matching_filter_indexes.push(filters.len());
                }
                let elements: Vec<_> = children_of[fi].iter().map(|ci| {
                    let n = &nodes[*ci];
                    json!({"tag":n.tag,"attributes":n.attrs,
                        "children":children_of[*ci].iter().map(|i| {
                            let child = &nodes[*i]; json!({"tag":child.tag,"attributes":child.attrs})
                        }).collect::<Vec<_>>()})
                }).collect();
                filters.push(json!({"attributes":filter.attrs,"elements":elements}));
            }
            if has_routes && matching_filter_indexes.is_empty() {
                continue;
            }
            let record = json!({"record_type":"component","component_type":c.kind,"name":c.name,
                "target":c.target,"exported":c.exported,"enabled":c.enabled,"launcher":c.launcher,
                "details":c.details,"intent_filters":filters,"matching_filter_indexes":matching_filter_indexes});
            if matches_query(&record) {
                records.push(record);
            }
        } else if !component_only
            && node.parent == Some(0)
            && matches!(
                node.tag.as_str(),
                "uses-permission"
                    | "uses-permission-sdk-23"
                    | "permission"
                    | "permission-group"
                    | "permission-tree"
            )
        {
            let name = node.get("name").unwrap_or_default();
            let record = json!({"record_type":"permission","declaration":node.tag,"name":name,"attributes":node.attrs});
            if contains("permission", name) && matches_query(&record) {
                records.push(record);
            }
        }
    }
    Ok(records)
}

pub fn render(
    xml: &str,
    path: &Path,
    digest: &str,
    table: &ResourceTable,
) -> Result<String, String> {
    let nodes = parse(xml)?;
    let manifest = &nodes[0];
    let package = raw(manifest, "package");
    let app_index = nodes
        .iter()
        .position(|n| n.parent == Some(0) && n.tag == "application");
    let app = app_index.map(|i| &nodes[i]);
    let sdk = child(&nodes, 0, "uses-sdk");
    let target = sdk
        .and_then(|n| n.get("targetSdkVersion"))
        .or_else(|| sdk.and_then(|n| n.get("minSdkVersion")))
        .unwrap_or("1")
        .parse::<u32>()
        .ok();
    let mut out = String::from(
        "# Manifest summary\n\n## Application information\n\n| Field | Value |\n| --- | --- |\n",
    );
    attr_line(&mut out, "Original Filename", path.display().to_string());
    attr_line(
        &mut out,
        "Application Name",
        app.map(|a| resolve_label(&value(a, "label"), &package, table))
            .unwrap_or_else(|| "Not declared".into()),
    );
    attr_line(&mut out, "Package Name", package.clone());
    for (label, node, key) in [
        ("Shared User ID", Some(manifest), "sharedUserId"),
        ("Version Code", Some(manifest), "versionCode"),
        ("Version Name", Some(manifest), "versionName"),
        ("Minimum SDK", sdk, "minSdkVersion"),
        ("Target SDK", sdk, "targetSdkVersion"),
        ("Maximum SDK", sdk, "maxSdkVersion"),
    ] {
        attr_line(
            &mut out,
            label,
            node.map(|n| value(n, key))
                .unwrap_or_else(|| "Not declared".into()),
        );
    }
    attr_line(&mut out, "SHA-256", digest.into());
    attr_line(
        &mut out,
        "Debuggable",
        app.map(|a| bool_value(a, "debuggable", Some(false)))
            .unwrap_or_else(|| "Not declared".into()),
    );
    attr_line(
        &mut out,
        "Allow Backup",
        app.map(|a| bool_value(a, "allowBackup", Some(true)))
            .unwrap_or_else(|| "Not declared".into()),
    );
    attr_line(
        &mut out,
        "Application Enabled",
        app.map(|a| bool_value(a, "enabled", Some(true)))
            .unwrap_or_else(|| "Not declared".into()),
    );
    let mut groups = [String::new(), String::new(), String::new(), String::new()];
    let mut deep_links = String::new();
    let mut unresolved = Vec::new();
    if let Some(app_index) = app_index {
        for (index, component) in nodes.iter().enumerate().filter(|(_, n)| {
            n.parent == Some(app_index)
                && matches!(
                    n.tag.as_str(),
                    "activity" | "activity-alias" | "service" | "receiver" | "provider"
                )
        }) {
            let name = class_name(&package, component.get("name").unwrap_or("Not declared"));
            let filters: Vec<usize> = nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| n.parent == Some(index) && n.tag == "intent-filter")
                .map(|(i, _)| i)
                .collect();
            let exported = exported_value(component, target, !filters.is_empty());
            let enabled = bool_value(component, "enabled", Some(true));
            if exported.contains("Unresolved")
                || exported.contains("unresolved")
                || enabled.contains("unresolved")
            {
                unresolved.push(format!("{name}: exported {exported}; enabled {enabled}"));
            }
            let group = match component.tag.as_str() {
                "activity" | "activity-alias" => 0,
                "service" => 1,
                "receiver" => 2,
                _ => 3,
            };
            let mut details = format!(
                "\n{} {}\n  exported: {exported}; enabled: {enabled}\n",
                component.tag, name
            );
            if component.tag == "activity-alias" {
                details.push_str(&format!(
                    "  targetActivity: {}\n",
                    component
                        .get("targetActivity")
                        .map(|v| class_name(&package, v))
                        .unwrap_or_else(|| "Not declared".into())
                ));
            }
            for key in [
                "permission",
                "readPermission",
                "writePermission",
                "authorities",
            ] {
                if (key == "readPermission" || key == "writePermission" || key == "authorities")
                    && component.tag != "provider"
                {
                    continue;
                }
                let inherited = if key == "permission"
                    && component.tag != "activity-alias"
                    && component.get("permission").is_none()
                {
                    app.and_then(|a| a.get("permission"))
                } else {
                    None
                };
                let provider_fallback = if component.tag == "provider"
                    && matches!(key, "readPermission" | "writePermission")
                    && component.get(key).is_none()
                {
                    component
                        .get("permission")
                        .or_else(|| app.and_then(|a| a.get("permission")))
                } else {
                    None
                };
                details.push_str(&format!(
                    "  {key}: {}{}\n",
                    provider_fallback
                        .or(inherited)
                        .unwrap_or_else(|| component.get(key).unwrap_or("Not declared")),
                    if provider_fallback.is_some() {
                        " (provider permission fallback)"
                    } else if inherited.is_some() {
                        " (application declaration)"
                    } else {
                        ""
                    }
                ));
            }
            if exported.starts_with("true") {
                groups[group].push_str(&details);
            }
            if matches!(component.tag.as_str(), "activity" | "activity-alias") {
                for (number, filter) in filters.iter().enumerate() {
                    let children: Vec<&Node> =
                        nodes.iter().filter(|n| n.parent == Some(*filter)).collect();
                    let actions: Vec<&str> = children
                        .iter()
                        .filter(|n| n.tag == "action")
                        .filter_map(|n| n.get("name"))
                        .collect();
                    let categories: Vec<&str> = children
                        .iter()
                        .filter(|n| n.tag == "category")
                        .filter_map(|n| n.get("name"))
                        .collect();
                    let data: Vec<&Node> =
                        children.into_iter().filter(|n| n.tag == "data").collect();
                    let browser_filter = actions.contains(&"android.intent.action.VIEW")
                        && categories.contains(&"android.intent.category.BROWSABLE")
                        && categories.contains(&"android.intent.category.DEFAULT");
                    let app_enabled = app
                        .map(|a| bool_value(a, "enabled", Some(true)))
                        .unwrap_or_else(|| "Not declared".into());
                    let launchable = if !browser_filter
                        || exported.starts_with("false")
                        || enabled.starts_with("false")
                        || app_enabled.starts_with("false")
                    {
                        "false"
                    } else if exported.starts_with("true")
                        && enabled.starts_with("true")
                        && app_enabled.starts_with("true")
                    {
                        "true"
                    } else {
                        "unresolved"
                    };
                    if data.is_empty() {
                        continue;
                    }
                    let single_data = data.len() == 1;
                    deep_links.push_str(&format!("\n{} — filter {}\n  VIEW/DEFAULT/BROWSABLE: {browser_filter}; link-launchable by declared flags: {launchable}; exported: {exported}; enabled: {enabled}; application enabled: {app_enabled}; autoVerify: {} (declaration only)\n", name,number+1,value(&nodes[*filter],"autoVerify")));
                    if component.tag == "activity-alias" {
                        deep_links.push_str(&format!(
                            "  targetActivity: {}\n",
                            component
                                .get("targetActivity")
                                .map(|v| class_name(&package, v))
                                .unwrap_or_else(|| "Not declared".into())
                        ));
                    }
                    deep_links.push_str(&format!(
                        "  actions: {}; categories: {}\n",
                        actions.join(", "),
                        categories.join(", ")
                    ));
                    for datum in data {
                        let properties: Vec<String> = datum
                            .attrs
                            .iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect();
                        deep_links.push_str(&format!("  data: {}\n", properties.join(", ")));
                        if single_data
                            && browser_filter
                            && let (Some(scheme), Some(host)) =
                                (datum.get("scheme"), datum.get("host"))
                            && !host.contains('*')
                            && !scheme.contains(['@', '?', '*'])
                            && datum
                                .get("port")
                                .is_none_or(|port| port.bytes().all(|c| c.is_ascii_digit()))
                            && datum
                                .get("path")
                                .is_none_or(|path| !path.contains(['*', '@', '?']))
                            && ![
                                "pathPrefix",
                                "pathPattern",
                                "pathSuffix",
                                "pathAdvancedPattern",
                            ]
                            .iter()
                            .any(|key| datum.get(key).is_some())
                        {
                            deep_links.push_str(&format!(
                                "  literal example: {scheme}://{host}{}{}\n",
                                datum
                                    .get("port")
                                    .map(|p| format!(":{p}"))
                                    .unwrap_or_default(),
                                datum.get("path").unwrap_or("/")
                            ));
                        }
                    }
                }
            }
        }
    }
    for (heading, group) in [
        "Exported activities and aliases",
        "Exported services",
        "Exported receivers",
        "Exported providers",
    ]
    .into_iter()
    .zip(groups)
    {
        out.push_str(&format!("\n## {heading}\n\n"));
        if group.is_empty() {
            out.push_str("None identified\n");
        } else {
            code_block(&mut out, &group);
        }
    }
    out.push_str("\n## Deep links\n\n");
    if deep_links.is_empty() {
        out.push_str("None declared\n");
    } else {
        code_block(&mut out, &deep_links);
    }
    out.push_str("\nData attributes within an intent filter combine under Android matching rules. Literal examples show only unambiguous single-element declarations; App Link verification is not asserted.\n");
    out.push_str("\n## Custom permissions\n\n");
    for permission in nodes
        .iter()
        .filter(|n| n.parent == Some(0) && n.tag == "permission")
    {
        out.push_str(&format!(
            "- {}: protectionLevel {}\n",
            escape_inline(&value(permission, "name")),
            escape_inline(
                &permission
                    .get("protectionLevel")
                    .map(protection_level)
                    .unwrap_or_else(|| "Not declared".into())
            )
        ));
    }
    if !nodes
        .iter()
        .any(|n| n.parent == Some(0) && n.tag == "permission")
    {
        out.push_str("Not declared\n");
    }
    out.push_str("\n## Unresolved component flags\n\n");
    if unresolved.is_empty() {
        out.push_str("None identified\n");
    } else {
        for name in unresolved {
            out.push_str(&format!("- {}\n", escape_inline(&name)));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_filter_boundaries_and_export_defaults() {
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="sample.app" a:versionCode="7"><uses-sdk a:targetSdkVersion="30"/><application a:label="Demo" a:permission="sample.ACCESS"><activity a:name=".Main"><intent-filter><action a:name="android.intent.action.VIEW"/><category a:name="android.intent.category.DEFAULT"/><category a:name="android.intent.category.BROWSABLE"/><data a:scheme="demo" a:host="example.test" a:path="/open"/></intent-filter><intent-filter><data a:scheme="other" a:host="example.test"/></intent-filter></activity><service a:name=".Hidden" a:exported="false"/><provider a:name=".Provider" a:authorities="sample.app.provider"/></application><permission a:name="sample.ACCESS" a:protectionLevel="signature"/></manifest>"#;
        let text = render(
            xml,
            Path::new("/tmp/demo.apk"),
            "abc",
            &ResourceTable::default(),
        )
        .unwrap();
        assert!(text.contains("sample.app.Main"));
        assert!(text.contains("demo://example.test/open"));
        assert!(!text.contains("other://example.test/open"));
        assert!(text.contains("permission: sample.ACCESS (application declaration)"));
        assert!(!text.contains("service sample.app.Hidden"));
        assert!(!text.contains("provider sample.app.Provider"));
        assert!(text.contains("signature"));
    }

    #[test]
    fn sdk31_missing_exported_is_unresolved() {
        let xml = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="a"><uses-sdk android:targetSdkVersion="31"/><application><activity android:name=".A"><intent-filter><action android:name="android.intent.action.VIEW"/></intent-filter></activity></application></manifest>"#;
        let text = render(
            xml,
            Path::new("/tmp/demo.apks"),
            "abc",
            &ResourceTable::default(),
        )
        .unwrap();
        assert!(text.contains("Unresolved (missing exported on target SDK 31+)"));
        assert!(text.contains("## Exported activities and aliases\n\nNone identified"));
    }

    #[test]
    fn provider_permission_fallback_and_separate_data_do_not_invent_uri() {
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="sample.app"><uses-sdk a:targetSdkVersion="30"/><application a:permission="sample.ACCESS"><provider a:name=".P" a:exported="true"/><activity a:name=".A" a:exported="true"><intent-filter><action a:name="android.intent.action.VIEW"/><category a:name="android.intent.category.DEFAULT"/><category a:name="android.intent.category.BROWSABLE"/><data a:scheme="demo" a:host="example.test"/><data a:path="/other"/></intent-filter></activity></application></manifest>"#;
        let text = render(
            xml,
            Path::new("/tmp/demo.apk"),
            "abc",
            &ResourceTable::default(),
        )
        .unwrap();
        assert!(text.contains("readPermission: sample.ACCESS (provider permission fallback)"));
        assert!(text.contains("writePermission: sample.ACCESS (provider permission fallback)"));
        assert!(!text.contains("literal example:"));
    }

    #[test]
    fn unknown_sdk_keeps_exported_unresolved() {
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="sample.app"><uses-sdk a:targetSdkVersion="@integer/sdk"/><application><activity a:name=".A"><intent-filter><action a:name="android.intent.action.VIEW"/></intent-filter></activity></application></manifest>"#;
        let text = render(
            xml,
            Path::new("/tmp/demo.apk"),
            "abc",
            &ResourceTable::default(),
        )
        .unwrap();
        assert!(text.contains("Unresolved (target SDK)"));
        assert!(text.contains("## Exported activities and aliases\n\nNone identified"));
    }
}

#[cfg(test)]
mod component_table_tests {
    use super::*;
    #[test]
    fn component_table_preserves_filters_aliases_and_application_flags() {
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" package="demo"><uses-sdk a:targetSdkVersion="30"/><application a:enabled="false" a:permission="demo.ACCESS"><activity a:name=".Main"><intent-filter><action a:name="android.intent.action.MAIN"/><category a:name="android.intent.category.LAUNCHER"/><data a:scheme="demo" a:host="one.test"/><data a:pathPrefix="/open"/></intent-filter></activity><activity-alias a:name="Alias" a:targetActivity=".Main" a:exported="true"/><service a:name=".Worker"/><receiver a:name=".Receiver" a:exported="@bool/exported"/><provider a:name=".Provider" a:authorities="demo.provider"/></application><activity a:name=".Outside"/></manifest>"#;
        let rows = components(xml).unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].name, "demo.Main");
        assert!(rows[0].is_exported());
        assert!(rows[0].launcher);
        assert_eq!(rows[0].enabled, "false");
        assert_eq!(rows[0].filters[0].matches("data:").count(), 2);
        assert!(
            rows[0]
                .details
                .iter()
                .any(|(k, v)| k == "Inherited application permission" && v == "demo.ACCESS")
        );
        assert_eq!(rows[1].target, "demo.Main");
        assert!(!rows[1].launcher);
        assert!(!rows[2].is_exported());
        assert!(rows[3].exported.contains("unresolved"));
        assert!(!rows[4].is_exported());
    }
    #[test]
    fn component_table_sdk_defaults_and_unknowns() {
        for (sdk, expected) in [("16", "true"), ("17", "false"), ("Preview", "Unresolved")] {
            let xml = format!(
                r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android"><uses-sdk a:targetSdkVersion="{sdk}"/><application><provider a:name="demo.P"/></application></manifest>"#
            );
            assert!(components(&xml).unwrap()[0].exported.starts_with(expected));
        }
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android"><uses-sdk a:targetSdkVersion="31"/><application><activity a:name="demo.A" a:enabled="@bool/enabled"><intent-filter><action a:name="android.intent.action.MAIN"/></intent-filter><intent-filter><category a:name="android.intent.category.LAUNCHER"/></intent-filter></activity></application></manifest>"#;
        let row = components(xml).unwrap().remove(0);
        assert!(!row.launcher);
        assert_eq!(row.enabled, "unresolved");
        assert!(row.exported.contains("missing exported"));
        assert!(components("<manifest><application>").is_err());
    }
}
