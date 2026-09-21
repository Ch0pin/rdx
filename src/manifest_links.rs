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

/// Scalar offsets of component android:name values whose exported flag is true.
/// This is manifest metadata, not a claim that a disabled or permission-gated
/// component can be reached. Names need not exist in the DEX class inventory.
///
/// Omitted flags use documented target-SDK defaults. Resource references and
/// preview SDK codenames remain unknown; no resource resolution is attempted.
/// https://developer.android.com/guide/topics/manifest/uses-sdk-element
/// https://developer.android.com/guide/topics/manifest/provider-element
/// https://developer.android.com/about/versions/12/behavior-changes-12#exported
pub fn exported_components(text: &str) -> Vec<std::ops::Range<usize>> {
    #[derive(Default)]
    struct Component {
        name: Option<std::ops::Range<usize>>,
        exported: Option<String>,
        provider: bool,
        filter: bool,
    }
    let mut reader = NsReader::from_str(text);
    reader.config_mut().check_comments = true;
    let mut stack: Vec<(String, Option<usize>)> = Vec::new();
    let mut components: Vec<Component> = Vec::new();
    let mut root_seen = false;
    let mut declaration_seen = false;
    let mut application_seen = false;
    let mut sdk_seen = false;
    let mut minimum: Option<Option<u32>> = None;
    let mut target: Option<Option<u32>> = None;
    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(_) => return Vec::new(),
        };
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let (namespace, local) = reader.resolver().resolve_element(element.name());
                if matches!(namespace, ResolveResult::Unknown(_)) {
                    return Vec::new();
                }
                let unqualified = matches!(namespace, ResolveResult::Unbound);
                let tag = local.as_ref();
                if stack.is_empty() {
                    if root_seen || !unqualified || tag != "manifest" {
                        return Vec::new();
                    }
                    root_seen = true;
                }
                let in_manifest = stack.len() == 1 && stack[0].0 == "manifest";
                let in_application =
                    stack.len() == 2 && stack[0].0 == "manifest" && stack[1].0 == "application";
                if unqualified && in_manifest && tag == "application" {
                    if application_seen {
                        return Vec::new();
                    }
                    application_seen = true;
                }
                let sdk = unqualified && in_manifest && tag == "uses-sdk";
                if sdk {
                    if sdk_seen {
                        return Vec::new();
                    }
                    sdk_seen = true;
                }
                let is_component = unqualified
                    && in_application
                    && matches!(
                        tag,
                        "activity" | "activity-alias" | "service" | "receiver" | "provider"
                    );
                let mut component = Component {
                    provider: tag == "provider",
                    ..Default::default()
                };
                let mut attributes = HashSet::new();
                for attribute in element.attributes() {
                    let attribute = match attribute {
                        Ok(attribute) => attribute,
                        Err(_) => return Vec::new(),
                    };
                    let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    else {
                        return Vec::new();
                    };
                    if attribute.key.as_ref() == "xmlns"
                        || attribute.key.as_ref().starts_with("xmlns:")
                    {
                        continue;
                    }
                    let (namespace, name) = reader.resolver().resolve_attribute(attribute.key);
                    let uri = match namespace {
                        ResolveResult::Bound(ns) => ns.as_ref().to_owned(),
                        ResolveResult::Unbound => String::new(),
                        ResolveResult::Unknown(_) => return Vec::new(),
                    };
                    if !attributes.insert((uri.clone(), name.as_ref().to_owned())) {
                        return Vec::new();
                    }
                    if uri != ANDROID {
                        continue;
                    }
                    if sdk {
                        let number = if !value.is_empty()
                            && value.bytes().all(|byte| byte.is_ascii_digit())
                        {
                            value.parse::<u32>().ok().filter(|number| *number > 0)
                        } else {
                            None
                        };
                        match name.as_ref() {
                            "minSdkVersion" => minimum = Some(number),
                            "targetSdkVersion" => target = Some(number),
                            _ => {}
                        }
                    }
                    if is_component {
                        match name.as_ref() {
                            "exported" => component.exported = Some(value.into_owned()),
                            "name"
                                if !value.is_empty()
                                    && !value.starts_with(['@', '?'])
                                    && !value.chars().any(char::is_whitespace) =>
                            {
                                let Some(start) = (attribute.value.as_ptr() as usize)
                                    .checked_sub(text.as_ptr() as usize)
                                else {
                                    return Vec::new();
                                };
                                let Some(end) = start.checked_add(attribute.value.len()) else {
                                    return Vec::new();
                                };
                                if text.get(start..end).is_none() {
                                    return Vec::new();
                                }
                                component.name = Some(start..end);
                            }
                            _ => {}
                        }
                    }
                }
                if unqualified
                    && tag == "intent-filter"
                    && stack.len() == 3
                    && let Some(index) = stack.last().and_then(|frame| frame.1)
                {
                    components[index].filter = true;
                }
                let component_index = if is_component {
                    let index = components.len();
                    components.push(component);
                    Some(index)
                } else {
                    None
                };
                if matches!(event, Event::Start(_)) {
                    stack.push((
                        if unqualified {
                            tag.to_owned()
                        } else {
                            String::new()
                        },
                        component_index,
                    ));
                }
            }
            Event::End(_) => {
                if stack.pop().is_none() {
                    return Vec::new();
                }
            }
            Event::Decl(_) => {
                if root_seen || declaration_seen {
                    return Vec::new();
                }
                declaration_seen = true;
            }
            // Manifests have no character-data entity content. Attribute
            // entities were validated and normalized above; never expand DTDs.
            Event::DocType(_) | Event::GeneralRef(_) => return Vec::new(),
            Event::Text(value) if stack.is_empty() => {
                if !value
                    .as_ref()
                    .as_bytes()
                    .iter()
                    .all(u8::is_ascii_whitespace)
                {
                    return Vec::new();
                }
            }
            Event::CData(_) if stack.is_empty() => return Vec::new(),
            Event::Eof => break,
            _ => {}
        }
    }
    if !root_seen || !stack.is_empty() {
        return Vec::new();
    }
    let sdk = target.unwrap_or_else(|| minimum.unwrap_or(Some(1)));
    let mut last_byte = 0;
    let mut last_char = 0;
    components
        .into_iter()
        .filter_map(|component| {
            let exported = match component.exported.as_deref() {
                Some("true") => true,
                Some(_) => false,
                None if component.provider => sdk.is_some_and(|sdk| sdk <= 16),
                None => component.filter && sdk.is_some_and(|sdk| sdk < 31),
            };
            if !exported {
                return None;
            }
            let span = component.name?;
            let start = last_char + text[last_byte..span.start].chars().count();
            let end = start + text[span.clone()].chars().count();
            last_byte = span.end;
            last_char = end;
            Some(start..end)
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

    fn exported_names(xml: &str) -> Vec<String> {
        exported_components(xml)
            .into_iter()
            .map(|span| {
                xml.chars()
                    .skip(span.start)
                    .take(span.end - span.start)
                    .collect()
            })
            .collect()
    }
    fn manifest(sdk: &str, body: &str) -> String {
        format!(
            r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android">{sdk}<application>{body}</application></manifest>"#
        )
    }
    #[test]
    fn explicit_export_marks_only_component_names_without_inventory() {
        let xml = manifest(
            r#"<uses-sdk a:targetSdkVersion="35"/>"#,
            r#"
            <activity a:name=".Activity" a:exported="true"/>
            <activity-alias a:name=".Alias" a:targetActivity=".Activity" a:exported="true"/>
            <service a:name=".Service" a:exported="true"/>
            <receiver a:name=".Receiver" a:exported="true"/>
            <provider a:name=".Provider" a:exported="true"/>
            <activity a:name=".Private" a:exported="false"><intent-filter/></activity>
            <activity a:name=".Unknown" a:exported="@bool/exported"><intent-filter/></activity>
            <activity a:name=".Invalid" a:exported="yes"><intent-filter/></activity>
            <activity a:name="@string/name" a:exported="true"/>
        "#,
        );
        assert_eq!(
            exported_names(&xml),
            [".Activity", ".Alias", ".Service", ".Receiver", ".Provider"]
        );
    }
    #[test]
    fn exported_legacy_intent_defaults_require_known_target_below_31() {
        let body = r#"<activity a:name=".A"><intent-filter/></activity><activity-alias a:name=".Alias"><intent-filter/></activity-alias><receiver a:name=".R"><intent-filter/></receiver><service a:name=".S"><intent-filter/></service><activity a:name=".NoFilter"/>"#;
        for sdk in ["1", "16", "17", "30"] {
            let xml = manifest(&format!(r#"<uses-sdk a:targetSdkVersion="{sdk}"/>"#), body);
            assert_eq!(exported_names(&xml), [".A", ".Alias", ".R", ".S"], "{sdk}");
        }
        for sdk in [
            "31",
            "35",
            "@integer/target",
            "VanillaIceCream",
            "",
            "0",
            "-1",
            "99999999999999999",
        ] {
            let xml = manifest(&format!(r#"<uses-sdk a:targetSdkVersion="{sdk}"/>"#), body);
            assert!(exported_names(&xml).is_empty(), "{sdk}");
        }
    }
    #[test]
    fn exported_provider_defaults_use_target_then_minimum_then_one() {
        let body = r#"<provider a:name=".P"/><provider a:name=".False" a:exported="false"/><provider a:name=".Unknown" a:exported="@bool/e"/>"#;
        for sdk in [
            "",
            r#"<uses-sdk/>"#,
            r#"<uses-sdk a:minSdkVersion="16"/>"#,
            r#"<uses-sdk a:targetSdkVersion="16"/>"#,
        ] {
            assert_eq!(exported_names(&manifest(sdk, body)), [".P"]);
        }
        for sdk in [
            r#"<uses-sdk a:minSdkVersion="17"/>"#,
            r#"<uses-sdk a:minSdkVersion="1" a:targetSdkVersion="17"/>"#,
            r#"<uses-sdk a:minSdkVersion="@integer/min"/>"#,
        ] {
            assert!(exported_names(&manifest(sdk, body)).is_empty());
        }
        // SDK declarations can follow application in the source; resolve after the full parse.
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android"><application><provider a:name=".P"/></application><uses-sdk a:targetSdkVersion="17"/></manifest>"#;
        assert!(exported_names(xml).is_empty());
    }
    #[test]
    fn exported_preserves_unicode_entity_spans_and_disabled_flags() {
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android"><!-- 🦀 --><application a:enabled="false"><activity a:name=".M&#97;in" a:exported="tr&#117;e" a:enabled="false"/><service a:name=".Δelta" a:exported="true"/></application></manifest>"#;
        assert_eq!(exported_names(xml), [".M&#97;in", ".Δelta"]);
        let ranges = exported_components(xml);
        assert!(ranges[0].end < ranges[1].start);
    }
    #[test]
    fn exported_ignores_wrong_namespaces_comments_and_nested_filters() {
        let xml = r#"<manifest xmlns:a="http://schemas.android.com/apk/res/android" xmlns:x="wrong"><application>
        <!-- <activity a:name=".Comment" a:exported="true"/> -->
        <x:activity a:name=".WrongTag" a:exported="true"/>
        <activity x:name=".WrongName" a:exported="true"/>
        <activity a:name=".WrongFlag" x:exported="true"/>
        <activity a:name=".Nested"><meta-data><intent-filter/></meta-data><x:intent-filter/></activity>
        <meta-data><activity a:name=".NestedActivity" a:exported="true"/></meta-data>
        <activity xmlns:a="wrong" a:name=".Shadow" a:exported="true"/>
        <activity a:name=".Good" a:exported="true"/>
        </application><activity a:name=".Outside" a:exported="true"/></manifest>"#;
        assert_eq!(exported_names(xml), [".Good"]);
    }
    #[test]
    fn exported_rejects_malformed_or_ambiguous_xml_without_partial_results() {
        let good = r#"<activity a:name=".A" a:exported="true"/>"#;
        for xml in [
            manifest("", good).replace("</application>", "</activity>"),
            manifest("", good).replace("</manifest>", ""),
            format!("{} trailing", manifest("", good)),
            format!("{}<manifest/>", manifest("", good)),
            manifest(
                "",
                r#"<activity a:name=".A" a:exported="true" a:exported="false"/>"#,
            ),
            manifest(
                "",
                r#"<activity xmlns:b="http://schemas.android.com/apk/res/android" a:name=".A" a:exported="true" b:exported="false"/>"#,
            ),
            manifest("", r#"<activity a:name=".A" bad:exported="true"/>"#),
            manifest("", good).replace("</application>", "<!-- bad -- comment --></application>"),
            manifest(r#"<uses-sdk/><uses-sdk/>"#, good),
            format!("<!DOCTYPE manifest>{}", manifest("", good)),
            manifest("", good).replace("</application>", "&unknown;</application>"),
            manifest("", good).replace("</application>", "<?xml version=\"1.0\"?></application>"),
        ] {
            assert!(exported_components(&xml).is_empty(), "{xml}");
        }
    }

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
