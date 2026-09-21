//! Links only Android component attributes, retaining their original XML spans.
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use rdx::engine::CodeLink;
use std::collections::HashSet;

const ANDROID: &str = "http://schemas.android.com/apk/res/android";

pub fn links(text: &str, classes: &[String]) -> Vec<CodeLink> {
    let mut reader = NsReader::from_str(text);
    let mut stack: Vec<String> = Vec::new();
    let mut package = String::new();
    let mut candidates = Vec::new();
    let mut root_seen = false;
    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(_) => return Vec::new(),
        };
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let (namespace, local) = reader.resolver().resolve_element(element.name());
                let tag = local.as_ref();
                let unqualified = matches!(namespace, ResolveResult::Unbound);
                if stack.is_empty() {
                    if root_seen || !unqualified || tag != "manifest" {
                        return Vec::new();
                    }
                    root_seen = true;
                }
                let in_application = stack == ["manifest".to_owned(), "application".to_owned()];
                let in_manifest = stack == ["manifest".to_owned()];
                for attribute in element.attributes() {
                    let attribute = match attribute {
                        Ok(attribute) => attribute,
                        Err(_) => return Vec::new(),
                    };
                    let (namespace, name) = reader.resolver().resolve_attribute(attribute.key);
                    if stack.is_empty() && attribute.key.as_ref() == "package" {
                        let Ok(value) =
                            attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        else {
                            return Vec::new();
                        };
                        package = value.into_owned();
                    }
                    if !unqualified
                        || !matches!(namespace, ResolveResult::Bound(ns) if ns.as_ref() == ANDROID)
                        || !is_component_attribute(tag, name.as_ref(), in_manifest, in_application)
                    {
                        continue;
                    }
                    let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    else {
                        return Vec::new();
                    };
                    // Slice-reader attributes borrow the original input, including entity syntax.
                    let Some(start) =
                        (attribute.value.as_ptr() as usize).checked_sub(text.as_ptr() as usize)
                    else {
                        continue;
                    };
                    let end = start + attribute.value.len();
                    if text.get(start..end).is_some() {
                        candidates.push((start, end, value.into_owned()));
                    }
                }
                if matches!(event, Event::Start(_)) {
                    stack.push(if unqualified {
                        tag.to_owned()
                    } else {
                        String::new()
                    });
                }
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Vec::new();
    }
    let classes: HashSet<&str> = classes.iter().map(String::as_str).collect();
    let mut last_byte = 0;
    let mut last_char = 0;
    candidates
        .into_iter()
        .filter_map(|(start, end, value)| {
            let full = if value.starts_with('.') {
                format!("{package}{value}")
            } else if !value.contains('.') && !package.is_empty() {
                format!("{package}.{value}")
            } else {
                value
            };
            if !classes.contains(full.as_str())
                && !full
                    .split_once('$')
                    .is_some_and(|(outer, _)| classes.contains(outer))
            {
                return None;
            }
            let char_start = last_char + text[last_byte..start].chars().count();
            let char_end = char_start + text[start..end].chars().count();
            last_byte = end;
            last_char = char_end;
            Some(CodeLink {
                start: char_start,
                end: char_end,
                label: full,
            })
        })
        .collect()
}

fn is_component_attribute(tag: &str, name: &str, in_manifest: bool, in_application: bool) -> bool {
    match tag {
        "application" if in_manifest => matches!(
            name,
            "name" | "backupAgent" | "manageSpaceActivity" | "appComponentFactory"
        ),
        "instrumentation" if in_manifest => name == "name",
        "activity" if in_application => matches!(name, "name" | "parentActivityName"),
        "activity-alias" if in_application => name == "targetActivity",
        "service" | "receiver" | "provider" if in_application => name == "name",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_namespaces_relative_names_and_entity_spans() {
        let xml = r#"<manifest package="p" xmlns:a="http://schemas.android.com/apk/res/android"><!-- 😀 --><application a:name="App"><activity a:name=".M&#97;in" a:parentActivityName="q.Parent"/><service a:name="p.Service"/></application></manifest>"#;
        let classes = ["p.App", "p.Main", "q.Parent", "p.Service"].map(str::to_owned);
        let links = links(xml, &classes);
        assert_eq!(
            links
                .iter()
                .map(|link| link.label.as_str())
                .collect::<Vec<_>>(),
            classes.iter().map(String::as_str).collect::<Vec<_>>()
        );
        let raw: String = xml
            .chars()
            .skip(links[1].start)
            .take(links[1].end - links[1].start)
            .collect();
        assert_eq!(raw, ".M&#97;in");
    }

    #[test]
    fn ignores_alias_names_nonclasses_wrong_namespace_and_comments() {
        let xml = r#"<manifest package="p" xmlns:a="http://schemas.android.com/apk/res/android" xmlns:x="wrong"><application><activity-alias a:name=".Alias" a:targetActivity=".Main"/><!-- <activity a:name=".Main"/> --><meta-data a:name="p.Main"/><activity x:name="p.Main"/><activity name="p.Main"/><activity a:name="missing.Type"/><provider a:name="p.Outer$Inner"/></application></manifest>"#;
        let classes = ["p.Alias", "p.Main", "p.Outer"].map(str::to_owned);
        let links = links(xml, &classes);
        assert_eq!(
            links
                .iter()
                .map(|link| link.label.as_str())
                .collect::<Vec<_>>(),
            ["p.Main", "p.Outer$Inner"]
        );
    }

    #[test]
    fn rejects_invalid_xml_and_non_manifest_context() {
        let classes = ["p.Main".to_owned()];
        assert!(links("<manifest><application>", &classes).is_empty());
        assert!(links(r#"<resources xmlns:a="http://schemas.android.com/apk/res/android"><activity a:name="p.Main"/></resources>"#, &classes).is_empty());
    }
}
