//! Decoded XML discovery with explicit per-file coverage and resource identity.
use super::*;
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use std::collections::BTreeMap;
#[derive(Default, Debug)]
struct Node {
    tag: String,
    attrs: BTreeMap<String, String>,
    parent: Option<usize>,
}
fn parse(xml: &str) -> Result<Vec<Node>> {
    let mut reader = NsReader::from_str(xml);
    let mut nodes = Vec::new();
    let mut stack = Vec::new();
    loop {
        match reader.read_event()? {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                ensure!(
                    !stack.is_empty() || nodes.is_empty(),
                    "Multiple XML root elements"
                );
                let empty = matches!(event, Event::Empty(_));
                let e = match event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                let mut n = Node {
                    tag: e.local_name().as_ref().to_owned(),
                    parent: stack.last().copied(),
                    ..Default::default()
                };
                for a in e.attributes() {
                    let a = a?;
                    let (ns, local) = reader.resolver().resolve_attribute(a.key);
                    let prefix = match ns {
                        ResolveResult::Unbound => "",
                        ResolveResult::Bound(n)
                            if n.as_ref() == "http://schemas.android.com/apk/res/android" =>
                        {
                            "android:"
                        }
                        ResolveResult::Bound(n)
                            if n.as_ref() == "http://schemas.android.com/apk/res-auto" =>
                        {
                            "app:"
                        }
                        _ => continue,
                    };
                    n.attrs.insert(
                        format!("{prefix}{}", local.as_ref()),
                        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                            .into_owned(),
                    );
                }
                ensure!(
                    nodes.len() < 200000 && stack.len() < 512,
                    "XML structure budget exceeded"
                );
                let id = nodes.len();
                nodes.push(n);
                if !empty {
                    stack.push(id);
                }
            }
            Event::End(_) => {
                stack.pop().context("Unexpected XML closing tag")?;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(stack.is_empty(), "Unclosed XML element");
    Ok(nodes)
}
fn scalar(engine: &NativeEngine, raw: Option<&String>, package: &str) -> Value {
    raw.map(|r| engine.resource_table().resolve_scalar(r, package))
        .unwrap_or(Value::Null)
}
fn unresolved_attributes(engine: &NativeEngine, n: &Node, package: &str) -> Vec<Value> {
    n.attrs
        .iter()
        .filter_map(|(name, value)| {
            name.strip_prefix("app:resource_0x").map(|id| {
                json!({
                    "resource_id":format!("0x{id}"), "value":scalar(engine,Some(value),package),
                    "semantic_name_resolved":false
                })
            })
        })
        .collect()
}
fn graph_files(engine: &NativeEngine, raw: &str, package: &str) -> Vec<Value> {
    let reference = raw.strip_prefix('@').unwrap_or(raw);
    let numeric = reference
        .strip_prefix("0x")
        .and_then(|s| u32::from_str_radix(s, 16).ok());
    engine
        .resource_table()
        .entries
        .values()
        .filter(|r| {
            if let Some(id) = numeric {
                r.id == id
            } else {
                let (kind, name) = reference.rsplit_once('/').unwrap_or(("", reference));
                let (pkg, kind) = kind.split_once(':').unwrap_or((package, kind));
                r.kind == kind && r.name == name && r.package == pkg
            }
        })
        .flat_map(|r| {
            r.variants.iter().filter_map(|v| {
                v.file
                    .as_ref()
                    .map(|f| json!({"path":f,"configuration":v.configuration}))
            })
        })
        .collect()
}
fn nav_rows(
    engine: &NativeEngine,
    nodes: &[Node],
    path: &str,
    package: &str,
    configs: &Value,
) -> Vec<Value> {
    let mut rows = Vec::new();
    for n in nodes {
        if !["deepLink", "include"].contains(&n.tag.as_str()) {
            continue;
        }
        let mut ancestry = Vec::new();
        let mut parent = n.parent;
        while let Some(i) = parent {
            ancestry.push(i);
            parent = nodes[i].parent;
        }
        if !ancestry.iter().any(|i| nodes[*i].tag == "navigation") {
            continue;
        }
        let graphs: Vec<_> = ancestry
            .iter()
            .rev()
            .filter(|i| nodes[**i].tag == "navigation")
            .map(|i| nodes[*i].attrs.get("android:id"))
            .collect();
        if n.tag == "include" {
            let raw = n.attrs.get("app:graph").map(String::as_str).unwrap_or("");
            rows.push(json!({"kind":"include","graph":path,"graph_path":graphs,"graph_reference":raw,"resolved_graph_files":graph_files(engine,raw,package),"unresolved_attributes":unresolved_attributes(engine,n,package),"configuration":configs,"semantics":"Graph include edge; each resource graph is inventoried once, preserving cycles and configurations"}));
            continue;
        }
        let dest = n.parent.map(|i| &nodes[i]);
        let arguments:Vec<_>=nodes.iter().filter(|a|a.tag=="argument" && a.parent==n.parent).map(|a|json!({"name":scalar(engine,a.attrs.get("android:name"),package),"type":a.attrs.get("app:argType"),"nullable":scalar(engine,a.attrs.get("app:nullable"),package),"default":scalar(engine,a.attrs.get("android:defaultValue"),package),"unresolved_attributes":unresolved_attributes(engine,a,package)})).collect();
        rows.push(json!({"kind":"deep_link","graph":path,"graph_path":graphs,"configuration":configs,"destination_tag":dest.map(|d|&d.tag),"destination_id":dest.and_then(|d|d.attrs.get("android:id")),"destination_class":scalar(engine,dest.and_then(|d|d.attrs.get("android:name")),package),"uri_pattern":scalar(engine,n.attrs.get("app:uri"),package),"action":scalar(engine,n.attrs.get("app:action"),package),"mime_type":scalar(engine,n.attrs.get("app:mimeType"),package),"unresolved_attributes":unresolved_attributes(engine,n,package),"arguments":arguments}));
    }
    rows
}
pub(super) fn search(
    engine: &mut NativeEngine,
    project: &Project,
    tool: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut page = scan::Page::new(args)?;
    let mut errors = Vec::new();
    let mut failed = 0;
    let mut scanned = 0;
    let mut unresolved_attribute_count = 0;
    let query = args["query"].as_str().unwrap_or("");
    let folded = query.to_lowercase();
    let sensitive = args["case_sensitive"].as_bool().unwrap_or(true);
    let requested = args["class_name"].as_str().unwrap_or("");
    let requested = requested
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
        .map(|s| s.replace('/', "."))
        .unwrap_or_else(|| requested.to_owned());
    let mut files: Vec<_> = project
        .resources
        .iter()
        .filter(|p| {
            p.ends_with(".xml") && p.starts_with(args["resource_prefix"].as_str().unwrap_or(""))
        })
        .collect();
    files.sort();
    files.dedup();
    // File identity comes from ARSC, including obfuscated archive filenames.
    let mut identities: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
    for r in engine.resource_table().entries.values() {
        for v in &r.variants {
            if let Some(f) = &v.file {
                identities.entry(f.clone()).or_default().push((
                    r.kind.clone(),
                    r.package.clone(),
                    v.configuration.clone(),
                ));
            }
        }
    }
    let mut reader = engine.resource_reader()?;
    for path in files {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let identity = identities.get(path);
        let kind = if tool == "list_nav_deeplinks" {
            "navigation"
        } else {
            "layout"
        };
        if tool != "search_resource_xml"
            && !identity.is_some_and(|ids| ids.iter().any(|i| i.0 == kind))
            && !path
                .split('/')
                .any(|s| s == kind || s.starts_with(&format!("{kind}-")))
        {
            continue;
        }
        let package = identity
            .and_then(|ids| ids.first())
            .map(|i| i.1.as_str())
            .unwrap_or("");
        let configs = json!(
            identity
                .map(|ids| ids.iter().map(|i| &i.2).collect::<Vec<_>>())
                .unwrap_or_default()
        );
        let outcome = (|| -> Result<()> {
            let bytes = reader.read_path(path, crate::native_resources::MAX_XML_BYTES as u64)?;
            let xml = crate::native_resources::decode_with_attribute_resolver(&bytes, &|id| {
                engine
                    .resource_table()
                    .entries
                    .get(&id)
                    .filter(|r| r.kind == "attr")
                    .map(|r| r.name.as_str())
            })?
            .0;
            let nodes = parse(&xml)?;
            unresolved_attribute_count += nodes
                .iter()
                .map(|n| {
                    n.attrs
                        .keys()
                        .filter(|k| k.starts_with("app:resource_0x"))
                        .count()
                })
                .sum::<usize>();
            if tool == "search_resource_xml" {
                let hash = crate::engine::source_identity(&xml);
                for (line, text) in xml.lines().enumerate() {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    if if sensitive {
                        text.contains(query)
                    } else {
                        text.to_lowercase().contains(&folded)
                    } {
                        page.add(json!({"resource_path":path,"line":line+1,"text":text.chars().take(4096).collect::<String>(),"text_truncated":text.chars().count()>4096,"configuration":configs,"source_hash":hash}))?;
                    }
                }
            } else {
                if tool == "list_nav_deeplinks" {
                    for row in nav_rows(engine, &nodes, path, package, &configs) {
                        let destination = row["destination_class"]["value"]
                            .as_str()
                            .or_else(|| row["destination_class"].as_str())
                            .unwrap_or("");
                        let destination = if destination.starts_with('.') {
                            format!("{package}{destination}")
                        } else if !destination.contains('.') && !destination.is_empty() {
                            format!("{package}.{destination}")
                        } else {
                            destination.to_owned()
                        };
                        if scan::package_matches(&destination, args) {
                            page.add(row)?;
                        }
                    }
                } else {
                    for (i, n) in nodes.iter().enumerate() {
                        let raw = if n.tag.contains('.') {
                            Some(&n.tag)
                        } else if n.tag == "view" {
                            n.attrs.get("class")
                        } else if ["fragment", "activity"].contains(&n.tag.as_str()) {
                            n.attrs.get("android:name").or_else(|| n.attrs.get("class"))
                        } else if n.tag.chars().next().is_some_and(char::is_uppercase) {
                            Some(&n.tag)
                        } else {
                            None
                        };
                        if let Some(raw) = raw {
                            let resolved = scalar(engine, Some(raw), package);
                            let class = resolved["value"].as_str().unwrap_or(raw);
                            let class = if class.starts_with('.') {
                                format!("{package}{class}")
                            } else {
                                class.to_owned()
                            };
                            let prefix_candidate = !class.contains('.')
                                && ["android.widget.", "android.view.", "android.webkit."]
                                    .iter()
                                    .any(|p| requested == format!("{p}{class}"));
                            if class == requested || prefix_candidate {
                                page.add(json!({"kind":"layout_declaration","class_id":requested,"raw_class":raw,"resolution":if prefix_candidate {"possible_platform_inflater_prefix"} else {"exact_xml_class"},"resource_path":path,"element_index":i,"element":n.tag,"configuration":configs,"possible_inflation":true,"runtime_instantiation_proven":false}))?;
                            }
                        }
                    }
                }
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => scanned += 1,
            Err(e) => {
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                failed += 1;
                if errors.len() < 50 {
                    errors.push(json!({"resource_path":path,"error":format!("{e:#}")}));
                }
            }
        }
    }
    let hotwire = if tool == "list_nav_deeplinks"
        && args["resource_prefix"].as_str().is_none_or(str::is_empty)
    {
        Some(super::hotwire::append(
            engine, project, args, cancel, &mut page,
        )?)
    } else {
        None
    };
    let mut result = page.finish()?;
    if let Some(coverage) = hotwire {
        result["hotwire_coverage"] = coverage;
    }
    result["complete"] = json!(
        failed == 0
            && result["hotwire_coverage"]["failed_array_methods"]
                .as_u64()
                .unwrap_or(0)
                == 0
            && result["hotwire_coverage"]["truncated_array_evidence"]
                .as_u64()
                .unwrap_or(0)
                == 0
    );
    result["scanned_files"] = json!(scanned);
    result["failed_files"] = json!(failed);
    result["unresolved_attribute_count"] = json!(unresolved_attribute_count);
    result["attribute_semantics_complete"] = json!(failed == 0 && unresolved_attribute_count == 0);
    result["errors"] = json!(errors);
    result["errors_truncated"] = json!(failed > 50);
    result["configuration_semantics"] = json!(
        "All stored XML variants; scalar attributes expose default and per-config values. No device configuration selection or runtime route expansion."
    );
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_graph_arguments_and_includes() {
        let xml = r#"<navigation xmlns:a="http://schemas.android.com/apk/res/android" xmlns:x="http://schemas.android.com/apk/res-auto" a:id="@id/root"><navigation a:id="@id/sub"><fragment a:name="p.Screen"><deepLink x:uri="app://x/{id}"/><argument a:name="id" x:argType="string"/></fragment><include x:graph="@navigation/root"/></navigation></navigation>"#;
        let nodes = parse(xml).unwrap();
        let rows = nav_rows(
            &NativeEngine::default(),
            &nodes,
            "res/navigation/a.xml",
            "p",
            &json!(["default"]),
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["arguments"][0]["name"]["value"], "id");
        assert_eq!(rows[0]["graph_path"].as_array().unwrap().len(), 2);
        assert_eq!(rows[1]["kind"], "include");
        let stripped = parse(r#"<navigation xmlns:app="http://schemas.android.com/apk/res-auto"><fragment><deepLink app:resource_0x7f040c19="app://retained"/></fragment></navigation>"#).unwrap();
        let rows = nav_rows(
            &NativeEngine::default(),
            &stripped,
            "res/navigation/a.xml",
            "p",
            &json!(["default"]),
        );
        assert!(rows[0]["uri_pattern"].is_null());
        assert_eq!(
            rows[0]["unresolved_attributes"][0]["resource_id"],
            "0x7f040c19"
        );
        assert_eq!(
            rows[0]["unresolved_attributes"][0]["value"]["value"],
            "app://retained"
        );
    }
}
