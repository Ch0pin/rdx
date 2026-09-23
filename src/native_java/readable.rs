use crate::{
    engine::{CodeDefinition, CodeLink, DecompiledCode},
    native_dex::DexClass,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Shortened {
    pub codes: Vec<DecompiledCode>,
    pub imports: BTreeSet<String>,
}

fn descriptor_name(descriptor: &str) -> Option<String> {
    descriptor
        .trim_start_matches('[')
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
        .and_then(|s| super::names::qualified(s, '/').ok())
}

fn candidates(class: &DexClass, codes: &[DecompiledCode]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for ty in class
        .superclass
        .iter()
        .chain(class.interfaces.iter())
        .chain(class.fields.iter().map(|f| &f.field_type))
        .chain(class.methods.iter().flat_map(|m| {
            std::iter::once(&m.return_type)
                .chain(m.parameters.iter())
                .chain(m.thrown_types.iter())
        }))
    {
        if let Some(name) = descriptor_name(ty) {
            names.insert(name);
        }
    }
    for link in codes.iter().flat_map(|code| &code.links) {
        let label = link.label.as_str();
        let possible = if label.contains('(') || label.contains(':') {
            label.rsplit_once('.').map(|p| p.0)
        } else {
            Some(label)
        };
        if let Some(possible) = possible
            && possible.contains('.')
            && let Ok(display) = super::names::qualified(possible, '.')
        {
            names.insert(display);
        }
    }
    names
}

fn java_ranges(chars: &[char]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        let quoted = match (chars[i], chars.get(i + 1)) {
            ('/', Some('/')) => Some((false, '\n')),
            ('/', Some('*')) => Some((false, '\0')),
            ('"', _) => Some((true, '"')),
            ('\'', _) => Some((true, '\'')),
            _ => None,
        };
        let Some((escaped, end)) = quoted else {
            i += 1;
            continue;
        };
        if start < i {
            ranges.push((start, i));
        }
        if chars[i] == '/' {
            i += 2;
        } else {
            i += 1;
        }
        while i < chars.len() {
            if end == '\n' && chars[i] == '\n' {
                break;
            }
            if end == '\0' && chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                i += 2;
                break;
            }
            if end != '\0' && end != '\n' && chars[i] == end {
                i += 1;
                break;
            }
            if escaped && chars[i] == '\\' {
                i += 1;
            }
            i += 1;
        }
        start = i;
    }
    if start < chars.len() {
        ranges.push((start, chars.len()));
    }
    ranges
}

fn tokens(source: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    for (begin, end) in java_ranges(&chars) {
        let mut i = begin;
        while i < end {
            if chars[i].is_ascii_alphabetic() || matches!(chars[i], '_' | '$') {
                let start = i;
                i += 1;
                while i < end
                    && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '$' | '.'))
                {
                    i += 1;
                }
                tokens.push((start, i, chars[start..i].iter().collect()));
            } else {
                i += 1;
            }
        }
    }
    tokens
}

fn remap(position: usize, edits: &[(usize, usize, String)], deltas: &[isize]) -> usize {
    let count = edits.partition_point(|(start, _, _)| *start <= position);
    if count == 0 {
        return position;
    }
    let (start, end, replacement) = &edits[count - 1];
    let before = if count == 1 { 0 } else { deltas[count - 2] };
    if position == *start && start != end {
        return (*start as isize + before) as usize;
    }
    if position <= *end {
        return ((*start as isize + before) + replacement.chars().count() as isize) as usize;
    }
    (position as isize + deltas[count - 1]) as usize
}

fn apply(mut code: DecompiledCode, mut edits: Vec<(usize, usize, String)>) -> DecompiledCode {
    if edits.is_empty() {
        return code;
    }
    edits.sort_by_key(|e| e.0);
    let mut total = 0isize;
    let deltas: Vec<_> = edits
        .iter()
        .map(|(start, end, replacement)| {
            total += replacement.chars().count() as isize - (*end - *start) as isize;
            total
        })
        .collect();
    let chars: Vec<char> = code.source.chars().collect();
    let mut source = String::new();
    let mut cursor = 0;
    for (start, end, replacement) in &edits {
        source.extend(&chars[cursor..*start]);
        source.push_str(replacement);
        cursor = *end;
    }
    source.extend(&chars[cursor..]);
    for CodeLink { start, end, .. } in &mut code.links {
        *start = remap(*start, &edits, &deltas);
        *end = remap(*end, &edits, &deltas);
    }
    for CodeDefinition { start, end, .. } in &mut code.definitions {
        *start = remap(*start, &edits, &deltas);
        *end = remap(*end, &edits, &deltas);
    }
    code.source = source;
    code.source_hash = crate::engine::source_identity(&code.source);
    code
}

// Binary '$' is legal in top-level names, so only a proven member relationship
// authorizes source nesting. Android's public API declares this exact type as
// PackageInstaller.SessionInfo (API21+):
// https://developer.android.com/reference/android/content/pm/PackageInstaller.SessionInfo
fn platform_member_type(binary: &str) -> Option<(&'static str, &'static str)> {
    match binary {
        "android.content.pm.PackageInstaller$SessionInfo" => {
            Some(("android.content.pm.PackageInstaller", "SessionInfo"))
        }
        _ => None,
    }
}

pub(crate) fn shorten(name: &str, class: &DexClass, codes: Vec<DecompiledCode>) -> Shortened {
    let display_name = super::names::qualified(name, '.').unwrap_or_else(|_| name.to_owned());
    let name = display_name.as_str();
    let package = name.rsplit_once('.').map_or("", |p| p.0);
    let own_simple = name.rsplit('.').next().unwrap_or(name);
    let mut blocked: BTreeSet<String> = class.fields.iter().map(|f| f.name.to_string()).collect();
    blocked.extend(class.methods.iter().map(|m| m.name.to_string()));
    blocked.insert(own_simple.to_owned());
    let mut candidates = candidates(class, &codes);
    candidates.insert(name.to_owned());
    // Include outer owners in the ordinary ambiguity/shadow analysis. An
    // unrelated PackageInstaller type or local prevents importing this owner.
    let owners: Vec<_> = candidates
        .iter()
        .filter_map(|candidate| platform_member_type(candidate).map(|(owner, _)| owner.to_owned()))
        .collect();
    candidates.extend(owners);
    let tokenized: Vec<_> = codes.iter().map(|code| tokens(&code.source)).collect();
    for member_tokens in &tokenized {
        for (_, _, token) in member_tokens {
            if !token.contains('.') {
                blocked.insert(token.clone());
            }
        }
    }
    let mut by_simple: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for fq in &candidates {
        if fq
            .rsplit('.')
            .next()
            .is_some_and(|simple| simple.contains('$'))
        {
            continue;
        }
        by_simple
            .entry(fq.rsplit('.').next().unwrap())
            .or_default()
            .push(fq);
    }
    let allowed: BTreeMap<&str, &str> = by_simple
        .into_iter()
        .filter_map(|(simple, names)| {
            if names.contains(&name) {
                Some((name, simple))
            } else {
                (names.len() == 1 && !blocked.contains(simple)).then_some((names[0], simple))
            }
        })
        .collect();
    struct Display<'a> {
        text: String,
        import: Option<&'a str>,
    }
    let mut displays: BTreeMap<&str, Display<'_>> = allowed
        .iter()
        .map(|(&qualified, &simple)| {
            (
                qualified,
                Display {
                    text: simple.to_owned(),
                    import: Some(qualified),
                },
            )
        })
        .collect();
    for qualified in &candidates {
        if let Some((owner, member)) = platform_member_type(qualified) {
            let short = allowed.get(owner).copied();
            displays.insert(
                qualified,
                Display {
                    text: format!("{}.{member}", short.unwrap_or(owner)),
                    import: short.map(|_| owner),
                },
            );
        }
    }
    let mut used = BTreeSet::new();
    let codes = codes
        .into_iter()
        .zip(tokenized)
        .map(|(code, member_tokens)| {
            let mut edits = Vec::new();
            for (start, end, token) in member_tokens {
                let matched = displays
                    .get(token.as_str())
                    .map(|display| (display, end))
                    .or_else(|| {
                        token.match_indices('.').rev().find_map(|(at, _)| {
                            let prefix = &token[..at];
                            displays
                                .get(prefix)
                                .map(|display| (display, start + prefix.chars().count()))
                        })
                    });
                if let Some((display, type_end)) = matched {
                    edits.push((start, type_end, display.text.clone()));
                    if let Some(owner) = display.import {
                        let owner_package = owner.rsplit_once('.').map_or("", |p| p.0);
                        if owner_package != package && owner_package != "java.lang" {
                            used.insert(owner.to_owned());
                        }
                    }
                }
            }
            apply(code, edits)
        })
        .collect();
    Shortened {
        codes,
        imports: used,
    }
}

pub(crate) fn add_imports(code: DecompiledCode, imports: &BTreeSet<String>) -> DecompiledCode {
    if imports.is_empty() {
        return code;
    }
    let at_byte = if code.source.starts_with("package ") {
        code.source.find('\n').map_or(code.source.len(), |n| n + 1)
    } else {
        0
    };
    let at = code.source[..at_byte].chars().count();
    let text = format!(
        "\n{}\n",
        imports
            .iter()
            .map(|i| format!("import {i};\n"))
            .collect::<String>()
    );
    let mut code = apply(code, vec![(at, at, text)]);
    let mut position = at + 1;
    for import in imports {
        let start = position + "import ".chars().count();
        code.links.push(CodeLink {
            start,
            end: start + import.chars().count(),
            label: import.clone(),
        });
        position += "import ".chars().count() + import.chars().count() + ";\n".chars().count();
    }
    code
}

pub(crate) fn insert(code: DecompiledCode, at: usize, text: &str) -> DecompiledCode {
    apply(code, vec![(at, at, text.to_owned())])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::DexField;

    fn code(source: &str, linked: &str) -> DecompiledCode {
        let byte = source.find(linked).unwrap();
        let start = source[..byte].chars().count();
        DecompiledCode {
            source: source.into(),
            links: vec![CodeLink {
                start,
                end: start + linked.chars().count(),
                label: linked.into(),
            }],
            definitions: Vec::new(),
            source_hash: String::new(),
        }
    }

    fn activity_class() -> DexClass {
        let mut class = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.fields = vec![DexField {
            declaring_type: class.descriptor.clone(),
            name: "screen".into(),
            field_type: "Landroid/app/Activity;".into(),
            access_flags: 0,
            is_static: false,
        }];
        class
    }

    #[test]
    fn shortens_code_only_and_preserves_unicode_link_offsets() {
        let class = activity_class();
        let source = "package sample;\n\nclass Hello {\n    android.app.Activity écran;\n    String s = \"android.app.Activity\"; // android.app.Activity\n}\n";
        let shortened = shorten(
            "sample.Hello",
            &class,
            vec![code(source, "android.app.Activity")],
        );
        let result = add_imports(
            shortened.codes.into_iter().next().unwrap(),
            &shortened.imports,
        );
        assert!(result.source.contains("import android.app.Activity;"));
        assert!(result.source.contains("Activity écran;"));
        assert!(
            result
                .source
                .contains("\"android.app.Activity\"; // android.app.Activity")
        );
        let link = &result.links[0];
        assert_eq!(
            result
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "Activity"
        );
        let import = result.links.iter().find(|link| {
            link.label == "android.app.Activity"
                && result
                    .source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>()
                    == "android.app.Activity"
        });
        assert!(import.is_some());
        assert_eq!(
            result.source_hash,
            crate::engine::source_identity(&result.source)
        );
    }

    #[test]
    fn collisions_keep_both_types_qualified() {
        let mut class = activity_class();
        class.fields.push(DexField {
            declaring_type: class.descriptor.clone(),
            name: "other".into(),
            field_type: "Lexample/Activity;".into(),
            access_flags: 0,
            is_static: false,
        });
        let source = "android.app.Activity a; example.Activity b;\n";
        let shortened = shorten(
            "sample.Hello",
            &class,
            vec![code(source, "android.app.Activity")],
        );
        assert_eq!(shortened.codes[0].source, source);
        assert!(shortened.imports.is_empty());
    }

    #[test]
    fn plans_collisions_and_identifier_shadows_across_members() {
        let class = activity_class();
        let first = code("android.app.Activity v0;\n", "android.app.Activity");
        let second = code(
            "example.Activity v1; int Activity = 0;\n",
            "example.Activity",
        );
        let shortened = shorten("sample.Hello", &class, vec![first, second]);
        assert!(shortened.codes[0].source.contains("android.app.Activity"));
        assert!(shortened.codes[1].source.contains("example.Activity"));
        assert!(shortened.imports.is_empty());
    }

    #[test]
    fn own_class_shortens_while_external_same_simple_name_stays_qualified() {
        let mut class = activity_class();
        class.fields.push(DexField {
            declaring_type: class.descriptor.clone(),
            name: "other".into(),
            field_type: "Lexample/Hello;".into(),
            access_flags: 0,
            is_static: false,
        });
        let source = "sample.Hello self; example.Hello other; sample.Hello.FIELD;\n";
        let shortened = shorten("sample.Hello", &class, vec![code(source, "sample.Hello")]);
        let result = &shortened.codes[0];
        assert!(
            result
                .source
                .starts_with("Hello self; example.Hello other;")
        );
        assert!(result.source.contains("Hello.FIELD"));
        let link = &result.links[0];
        assert_eq!(
            result
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "Hello"
        );
        assert!(shortened.imports.is_empty());
    }
}

#[cfg(test)]
mod static_call_tests {
    use super::*;
    #[test]
    fn shortens_static_call_owner_and_preserves_method_link() {
        let class = crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let source = "com.example.Log.g(\"λ com.example.Log\");";
        let owner = "com.example.Log";
        let method = owner.len() + 1;
        let code = DecompiledCode {
            source: source.into(),
            links: vec![
                CodeLink {
                    start: 0,
                    end: owner.len(),
                    label: owner.into(),
                },
                CodeLink {
                    start: method,
                    end: method + 1,
                    label: "com.example.Log.g(Ljava/lang/String;)V".into(),
                },
            ],
            definitions: vec![],
            source_hash: String::new(),
        };
        let result = shorten("sample.Hello", &class, vec![code]);
        assert_eq!(result.codes[0].source, "Log.g(\"λ com.example.Log\");");
        assert!(result.imports.contains(owner));
        let link = result.codes[0]
            .links
            .iter()
            .find(|l| l.label.contains(".g("))
            .unwrap();
        assert_eq!(
            result.codes[0]
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "g"
        );
    }
}

#[cfg(test)]
mod nested_member_tests {
    use super::*;
    const BINARY: &str = "android.content.pm.PackageInstaller$SessionInfo";
    fn class() -> DexClass {
        crate::native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0)
    }
    fn document(source: &str, names: &[&str]) -> DecompiledCode {
        let links = names
            .iter()
            .map(|name| {
                let start = source[..source.find(name).unwrap()].chars().count();
                CodeLink {
                    start,
                    end: start + name.chars().count(),
                    label: (*name).into(),
                }
            })
            .collect();
        DecompiledCode {
            source: source.into(),
            links,
            definitions: vec![],
            source_hash: crate::engine::source_identity(source),
        }
    }
    #[test]
    fn documented_member_imports_outer_owner_and_keeps_binary_navigation() {
        let source = format!("String marker = \"λ\";\n{BINARY} info;\n");
        let result = shorten("sample.Hello", &class(), vec![document(&source, &[BINARY])]);
        assert_eq!(
            result.imports,
            BTreeSet::from(["android.content.pm.PackageInstaller".into()])
        );
        let code = add_imports(result.codes.into_iter().next().unwrap(), &result.imports);
        assert!(code.source.contains("PackageInstaller.SessionInfo info;"));
        assert!(
            code.source
                .contains("import android.content.pm.PackageInstaller;")
        );
        let link = code.links.iter().find(|link| link.label == BINARY).unwrap();
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "PackageInstaller.SessionInfo"
        );
        assert_eq!(
            code.source_hash,
            crate::engine::source_identity(&code.source)
        );
    }
    #[test]
    fn outer_name_collisions_keep_canonical_member_fully_qualified() {
        for source in [
            format!("int PackageInstaller = 0;\n{BINARY} info;\n"),
            format!("example.PackageInstaller other;\n{BINARY} info;\n"),
        ] {
            let mut names = vec![BINARY];
            if source.contains("example.PackageInstaller") {
                names.push("example.PackageInstaller");
            }
            let result = shorten("sample.Hello", &class(), vec![document(&source, &names)]);
            assert!(result.imports.is_empty());
            assert!(
                result.codes[0]
                    .source
                    .contains("android.content.pm.PackageInstaller.SessionInfo info;")
            );
            assert!(
                result.codes[0]
                    .links
                    .iter()
                    .any(|link| link.label == BINARY)
            );
        }
    }
    #[test]
    fn static_member_links_and_literal_binary_names_remain_exact() {
        let source = format!("{BINARY}.CREATOR;\nString literal = \"{BINARY}\"; // {BINARY}\n");
        let mut code = document(&source, &[BINARY]);
        code.links.push(CodeLink {
            start: BINARY.len() + 1,
            end: BINARY.len() + 8,
            label: format!("{BINARY}.CREATOR:Landroid/os/Parcelable$Creator;"),
        });
        let result = shorten("sample.Hello", &class(), vec![code]);
        let code = &result.codes[0];
        assert!(
            code.source
                .starts_with("PackageInstaller.SessionInfo.CREATOR;")
        );
        assert!(code.source.contains(&format!("\"{BINARY}\"; // {BINARY}")));
        let link = code
            .links
            .iter()
            .find(|link| link.label.contains(".CREATOR:"))
            .unwrap();
        assert_eq!(
            code.source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect::<String>(),
            "CREATOR"
        );
    }
    #[test]
    fn unproven_dollar_types_are_not_treated_as_members() {
        let source = "example.Price$Tag item;\n";
        let result = shorten(
            "sample.Hello",
            &class(),
            vec![document(source, &["example.Price$Tag"])],
        );
        assert_eq!(result.codes[0].source, source);
        assert!(result.imports.is_empty());
    }
}
