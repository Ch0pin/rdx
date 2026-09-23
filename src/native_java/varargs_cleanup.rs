//! Presentation-only expansion of proven Object... calls. Does not affect the
//! semantic renderer or accept arbitrary Java input as an optimization IR.
use crate::{
    engine::{CodeLink, DecompiledCode},
    native_dex::DexClass,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
struct Token {
    text: String,
    start: usize,
    end: usize,
}
fn tokens(text: &str) -> Vec<Token> {
    let chars: Vec<_> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '"' | '\'') {
            let delimiter = chars[i];
            i += 1;
            while i < chars.len() {
                let c = chars[i];
                i += 1;
                if c == '\\' {
                    i = (i + 1).min(chars.len());
                } else if c == delimiter {
                    break;
                }
            }
        } else if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
        } else if chars[i].is_alphanumeric() || matches!(chars[i], '_' | '$') {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '$')) {
                i += 1;
            }
            out.push(Token {
                text: chars[start..i].iter().collect(),
                start,
                end: i,
            });
        } else {
            i += 1;
        }
    }
    out
}
fn generated(name: &str) -> bool {
    name.strip_prefix('v')
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}
fn arguments(text: &str) -> Option<Vec<&str>> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                '"' | '\'' => quote = Some(c),
                ',' => {
                    result.push(text[start..i].trim());
                    start = i + 1;
                }
                '(' | ')' | '{' | '}' | '[' | ']' | ';' => return None,
                _ => {}
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    result.push(text[start..].trim());
    Some(result)
}
fn fixed_argument(text: &str, locals: &HashSet<String>) -> bool {
    locals.contains(text)
        || matches!(text, "null" | "true" | "false")
        || text.parse::<i64>().is_ok()
        || (text.starts_with('"')
            && text.ends_with('"')
            && arguments(text).is_some_and(|a| a.len() == 1))
}
struct Edit {
    start: usize,
    end: usize,
    text: String,
    links: Vec<CodeLink>,
}

pub(super) fn simplify(mut code: DecompiledCode, class: &DexClass) -> DecompiledCode {
    let Some(hierarchy) = class.symbols.hierarchy.get() else {
        return code;
    };
    // Most methods have no expanded-varargs opportunity. In particular an
    // abstract declaration must never scan and allocate every type in its DEX.
    if code.source.len() > 512 * 1024
        || !(code.source.contains("new java.lang.Object[] {")
            || code.source.contains("new Object[] {"))
        || !code
            .links
            .iter()
            .any(|link| hierarchy.is_unambiguous_object_varargs(&link.label))
    {
        return code;
    }
    let all_tokens = tokens(&code.source);
    let mut uses: HashMap<&str, Vec<&Token>> = HashMap::new();
    for token in &all_tokens {
        uses.entry(&token.text).or_default().push(token);
    }
    let mut offset = 0;
    let lines: Vec<_> = code
        .source
        .split_inclusive('\n')
        .map(|line| {
            let start = offset;
            offset += line.chars().count();
            (start, line)
        })
        .collect();
    // Resolve only the local declaration types actually present in this
    // candidate method. Scan descriptor references once, without allocating a
    // formatted Java name for every unrelated class in the DEX pool.
    let needed_types: HashSet<String> = lines
        .iter()
        .filter_map(|(_, line)| {
            let (left, _) = line.trim().split_once(" = ")?;
            let mut parts = left.split_whitespace();
            let ty = parts.next()?;
            let name = parts.next()?;
            (parts.next().is_none() && generated(name))
                .then(|| format!("L{};", ty.replace('.', "/")))
        })
        .collect();
    let types: HashSet<_> = class
        .symbols
        .types
        .iter()
        .filter(|ty| {
            needed_types.contains(ty.as_ref())
                && !matches!(
                    ty.as_ref(),
                    "Ljava/lang/Object;" | "Ljava/lang/Cloneable;" | "Ljava/io/Serializable;"
                )
        })
        .filter_map(|ty| super::java_type(ty).ok())
        .collect();
    // Unique generated reference-local declarations only. Multiple declarations
    // (including other methods/scopes) make the name unavailable.
    let mut locals = HashSet::new();
    let mut duplicate = HashSet::new();
    for (_, line) in &lines {
        if let Some((left, _)) = line.trim().split_once(" = ") {
            let parts: Vec<_> = left.split_whitespace().collect();
            if parts.len() == 2
                && generated(parts[1])
                && types.contains(parts[0])
                && !locals.insert(parts[1].to_owned())
            {
                duplicate.insert(parts[1].to_owned());
            }
        }
    }
    locals.retain(|name| !duplicate.contains(name));
    let chars: Vec<_> = code.source.chars().collect();
    let mut edits = Vec::new();
    let mut skip = false;
    for pair in lines.windows(2) {
        if edits.len() >= 256
            || (edits.len() + 2) * (code.links.len() + code.definitions.len()) > 4_000_000
        {
            break;
        }
        if skip {
            skip = false;
            continue;
        }
        let (start, line) = pair[0];
        let (next_start, next) = pair[1];
        let Some((left, rhs)) = line
            .trim()
            .strip_suffix(';')
            .and_then(|s| s.split_once(" = "))
        else {
            continue;
        };
        let Some(name) = left
            .strip_prefix("java.lang.Object[] ")
            .or_else(|| left.strip_prefix("Object[] "))
        else {
            continue;
        };
        if !generated(name) {
            continue;
        }
        let Some(elements) = rhs
            .strip_prefix("new java.lang.Object[] {")
            .or_else(|| rhs.strip_prefix("new Object[] {"))
            .and_then(|s| s.strip_suffix('}'))
        else {
            continue;
        };
        let Some(args) = arguments(elements) else {
            continue;
        };
        if args.is_empty() || args.iter().any(|arg| !locals.contains(*arg)) {
            continue;
        }
        let Some(occurrences) = uses.get(name).filter(|v| v.len() == 2) else {
            continue;
        };
        if occurrences[0].start < start || occurrences[0].end > next_start {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if next.len() - next.trim_start().len() != indent {
            continue;
        }
        let call = next.trim();
        let Some(open) = call.find('(') else {
            continue;
        };
        let Some(actuals) = call[open + 1..].strip_suffix(");").and_then(arguments) else {
            continue;
        };
        if actuals.last() != Some(&name)
            || actuals[..actuals.len() - 1]
                .iter()
                .any(|arg| !fixed_argument(arg, &locals))
        {
            continue;
        }
        let prefix = &call[..open];
        if !prefix.split('.').all(super::identifier) {
            continue;
        }
        let method_start = next_start
            + indent
            + prefix
                .rfind('.')
                .map_or(0, |n| prefix[..=n].chars().count());
        let method_end = next_start + indent + prefix.chars().count();
        if !code.links.iter().any(|l| {
            l.start == method_start
                && l.end == method_end
                && l.label
                    .split_once('(')
                    .is_some_and(|(target, _)| target == prefix)
                && hierarchy.is_unambiguous_object_varargs(&l.label)
        }) {
            continue;
        }
        let use_start = next_start + next[..next.rfind(name).unwrap()].chars().count();
        if occurrences[1].start != use_start {
            continue;
        }
        // No reassignment/update of captured reference locals in the call or
        // declarations. Plain local reads are the only expanded expressions.
        if args.iter().any(|arg| {
            uses.get(*arg).is_some_and(|entries| {
                entries.iter().any(|t| {
                    let tail: String = chars[t.end..].iter().take(4).collect();
                    let tail = tail.trim_start();
                    t.start >= start
                        && (tail.starts_with('=')
                            || tail.starts_with("++")
                            || tail.starts_with("--"))
                })
            })
        }) {
            continue;
        }
        let element_byte = line.find('{').unwrap() + 1;
        let element_start = start + line[..element_byte].chars().count();
        let element_end = element_start + elements.chars().count();
        let moved = code
            .links
            .iter()
            .filter(|l| l.start >= element_start && l.end <= element_end)
            .map(|l| CodeLink {
                start: l.start - element_start,
                end: l.end - element_start,
                label: l.label.clone(),
            })
            .collect();
        edits.push(Edit {
            start,
            end: next_start,
            text: String::new(),
            links: vec![],
        });
        edits.push(Edit {
            start: use_start,
            end: use_start + name.len(),
            text: elements.into(),
            links: moved,
        });
        skip = true;
    }
    if edits.is_empty() {
        return code;
    }
    edits.sort_by_key(|e| e.start);
    let mut source = String::new();
    let mut links = Vec::new();
    let mut definitions = Vec::new();
    let mut cursor = 0;
    let mut written = 0;
    for edit in edits.into_iter().chain(std::iter::once(Edit {
        start: chars.len(),
        end: chars.len(),
        text: String::new(),
        links: vec![],
    })) {
        source.extend(chars[cursor..edit.start].iter());
        for l in code
            .links
            .iter()
            .filter(|l| l.start >= cursor && l.end <= edit.start)
        {
            let mut l = l.clone();
            l.start = written + l.start - cursor;
            l.end = written + l.end - cursor;
            links.push(l);
        }
        for d in code
            .definitions
            .iter()
            .filter(|d| d.start >= cursor && d.end <= edit.start)
        {
            let mut d = d.clone();
            d.start = written + d.start - cursor;
            d.end = written + d.end - cursor;
            definitions.push(d);
        }
        written += edit.start - cursor;
        for mut l in edit.links {
            l.start += written;
            l.end += written;
            links.push(l);
        }
        source.push_str(&edit.text);
        written += edit.text.chars().count();
        cursor = edit.end;
    }
    code.source_hash = crate::engine::source_identity(&source);
    code.source = source;
    code.links = links;
    code.definitions = definitions;
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{CodeDefinition, source_identity},
        native_dex::{DexMethod, DexSymbols},
        native_hierarchy::TypeHierarchy,
    };
    use std::sync::Arc;
    fn fixture(varargs: bool, overload: bool) -> DexClass {
        let method = || DexMethod {
            declaring_type: "Lsample/Log;".into(),
            name: "e".into(),
            return_type: "V".into(),
            parameters: vec!["Ljava/lang/String;".into(), "[Ljava/lang/Object;".into()],
            thrown_types: vec![],
            access_flags: 9 | if varargs { 0x80 } else { 0 },
            code: None,
        };
        let mut class = DexClass {
            symbols: Arc::new(DexSymbols {
                types: vec![
                    "Landroid/content/Intent;".into(),
                    "Ljava/lang/Object;".into(),
                ],
                ..Default::default()
            }),
            descriptor: "Lsample/Log;".into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![method()],
        };
        if overload {
            let mut other = method();
            other.parameters.pop();
            class.methods.push(other);
        }
        let hierarchy = Arc::new(TypeHierarchy::from_classes([&class]).unwrap());
        class.symbols.hierarchy.set(hierarchy).unwrap();
        class
    }
    fn document(text: &str) -> DecompiledCode {
        let byte = text.find("Log.e").unwrap() + 4;
        let start = text[..byte].chars().count();
        let def = text[..text.find("run").unwrap()].chars().count();
        DecompiledCode {
            source: text.into(),
            links: vec![CodeLink {
                start,
                end: start + 1,
                label: "sample.Log.e(Ljava/lang/String;[Ljava/lang/Object;)V".into(),
            }],
            definitions: vec![CodeDefinition {
                start: def,
                end: def + 3,
                kind: "method".into(),
                name: "run".into(),
            }],
            source_hash: source_identity(text),
        }
    }
    const INPUT: &str = "void run() {\n    android.content.Intent v0 = this.getIntent();\n    java.lang.Object[] v1 = new java.lang.Object[] {v0};\n    sample.Log.e(\"λ %s\", v1);\n}\n";
    #[test]
    fn expands_only_proven_varargs_and_preserves_unicode_mappings() {
        let result = simplify(document(INPUT), &fixture(true, false));
        assert!(!result.source.contains("Object[]"));
        assert!(result.source.contains("sample.Log.e(\"λ %s\", v0);"));
        assert_eq!(result.source_hash, source_identity(&result.source));
        assert_eq!(
            result
                .source
                .chars()
                .skip(result.links[0].start)
                .take(result.links[0].end - result.links[0].start)
                .collect::<String>(),
            "e"
        );
        assert_eq!(
            result
                .source
                .chars()
                .skip(result.definitions[0].start)
                .take(3)
                .collect::<String>(),
            "run"
        );
        for class in [fixture(false, false), fixture(true, true)] {
            assert_eq!(simplify(document(INPUT), &class).source, INPUT);
        }
    }
    #[test]
    fn keeps_ambiguous_arrays_null_literals_effects_and_multiple_uses() {
        for text in [
            INPUT.replace("{v0}", "{null}"),
            INPUT.replace("{v0}", "{this.getIntent()}"),
            INPUT.replace("android.content.Intent v0", "java.lang.Object v0"),
            INPUT.replace("android.content.Intent v0", "android.content.Intent[] v0"),
            INPUT.replace("sample.Log.e", "other();\n    sample.Log.e"),
            INPUT.replace("\n}", "\n    consume(v1);\n}"),
            INPUT.replace("\"λ %s\", v1", "other(), v1"),
        ] {
            assert_eq!(
                simplify(document(&text), &fixture(true, false)).source,
                text
            );
        }
    }
    #[test]
    fn receiver_owner_and_cross_dex_metadata_are_checked() {
        let log = fixture(true, false);
        let mut caller = fixture(false, false);
        caller.symbols = Arc::new(DexSymbols {
            types: vec!["Landroid/content/Intent;".into()],
            ..Default::default()
        });
        caller
            .symbols
            .hierarchy
            .set(Arc::clone(log.symbols.hierarchy.get().unwrap()))
            .unwrap();
        assert!(
            !simplify(document(INPUT), &caller)
                .source
                .contains("Object[]")
        );
        let mut mismatch = document(INPUT);
        mismatch.source = mismatch.source.replace("sample.Log.e", "sample.Sub.e");
        let original = mismatch.source.clone();
        assert_eq!(simplify(mismatch, &caller).source, original);
        let mut instance = fixture(true, false);
        instance.methods[0].access_flags &= !8;
        assert!(
            !TypeHierarchy::from_classes([&instance])
                .unwrap()
                .is_unambiguous_object_varargs(
                    "sample.Log.e(Ljava/lang/String;[Ljava/lang/Object;)V"
                )
        );
    }

    #[test]
    fn duplicate_or_unknown_ancestors_and_inherited_overloads_are_unproven() {
        let original = fixture(true, false);
        let mut child = fixture(true, false);
        child.symbols = Arc::new(DexSymbols::default());
        child.superclass = Some("Lmissing/Base;".into());
        assert!(
            !TypeHierarchy::from_classes([&child])
                .unwrap()
                .is_unambiguous_object_varargs(
                    "sample.Log.e(Ljava/lang/String;[Ljava/lang/Object;)V"
                )
        );
        assert!(
            !TypeHierarchy::from_classes([&original, &original])
                .unwrap()
                .is_unambiguous_object_varargs(
                    "sample.Log.e(Ljava/lang/String;[Ljava/lang/Object;)V"
                )
        );
        let mut parent = fixture(true, false);
        parent.descriptor = "Lsample/Base;".into();
        child.superclass = Some(parent.descriptor.clone());
        assert!(
            !TypeHierarchy::from_classes([&child, &parent])
                .unwrap()
                .is_unambiguous_object_varargs(
                    "sample.Log.e(Ljava/lang/String;[Ljava/lang/Object;)V"
                )
        );
    }
}

#[cfg(test)]
mod type_resolution_regression {
    use super::*;
    #[test]
    #[ignore = "Set RDX_TEST_APK for before/after local type-resolution parity"]
    fn filtered_type_pool_preserves_all_referenced_local_type_verdicts() {
        use crate::{engine::DecompilerEngine, native_engine::NativeDexEngine};
        let path = std::env::var_os("RDX_TEST_APK").unwrap();
        let mut engine = NativeDexEngine::default();
        engine.open(std::path::Path::new(&path)).unwrap();
        let mut checked = 0;
        for name in ["abvc", "absi", "aefa"] {
            let class = engine.class(name).unwrap();
            let old: HashSet<_> = class
                .symbols
                .types
                .iter()
                .filter(|ty| {
                    ty.starts_with('L')
                        && !matches!(
                            ty.as_ref(),
                            "Ljava/lang/Object;"
                                | "Ljava/lang/Cloneable;"
                                | "Ljava/io/Serializable;"
                        )
                })
                .filter_map(|ty| super::super::java_type(ty).ok())
                .collect();
            for method in &class.methods {
                let Ok(code) = super::super::render_method(name, class, method) else {
                    continue;
                };
                let declarations: Vec<_> = code
                    .source
                    .lines()
                    .filter_map(|line| {
                        let (left, _) = line.trim().split_once(" = ")?;
                        let parts: Vec<_> = left.split_whitespace().collect();
                        (parts.len() == 2 && generated(parts[1])).then(|| parts[0])
                    })
                    .collect();
                let needed: HashSet<_> = declarations
                    .iter()
                    .map(|ty| format!("L{};", ty.replace('.', "/")))
                    .collect();
                let new: HashSet<_> = class
                    .symbols
                    .types
                    .iter()
                    .filter(|ty| {
                        needed.contains(ty.as_ref())
                            && !matches!(
                                ty.as_ref(),
                                "Ljava/lang/Object;"
                                    | "Ljava/lang/Cloneable;"
                                    | "Ljava/io/Serializable;"
                            )
                    })
                    .filter_map(|ty| super::super::java_type(ty).ok())
                    .collect();
                for ty in declarations {
                    assert_eq!(
                        old.contains(ty),
                        new.contains(ty),
                        "{name}.{} local type{ty}",
                        method.name
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 100);
        println!("Matched old/new type verdicts for {checked} local declarations");
    }
}
