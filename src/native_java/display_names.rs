//! Token-aware display names for generated method locals and parameters.
//! Raw symbols stay unchanged; edits remap Unicode-scalar source positions.
use crate::engine::{DecompiledCode, source_identity};
use std::collections::{HashMap, HashSet};

const MAX_TOKENS: usize = 250_000;

#[derive(Clone)]
struct Token<'a> {
    text: &'a str,
    byte_start: usize,
    byte_end: usize,
    start: usize,
    end: usize,
    identifier: bool,
}

fn identifier_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}
fn generated(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() > 1 && matches!(bytes[0], b'v' | b'p') && bytes[1..].iter().all(u8::is_ascii_digit)
}
fn keyword(name: &str) -> bool {
    matches!(
        name,
        "abstract"
            | "assert"
            | "boolean"
            | "break"
            | "byte"
            | "case"
            | "catch"
            | "char"
            | "class"
            | "const"
            | "continue"
            | "default"
            | "do"
            | "double"
            | "else"
            | "enum"
            | "extends"
            | "final"
            | "finally"
            | "float"
            | "for"
            | "goto"
            | "if"
            | "implements"
            | "import"
            | "instanceof"
            | "int"
            | "interface"
            | "long"
            | "native"
            | "new"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "short"
            | "static"
            | "strictfp"
            | "super"
            | "switch"
            | "synchronized"
            | "this"
            | "throw"
            | "throws"
            | "transient"
            | "try"
            | "void"
            | "volatile"
            | "while"
            | "true"
            | "false"
            | "null"
            | "record"
            | "sealed"
            | "permits"
            | "yield"
            | "var"
            | "_"
    )
}
fn primitive(name: &str) -> bool {
    matches!(
        name,
        "boolean" | "byte" | "char" | "double" | "float" | "int" | "long" | "short"
    )
}

fn tokens(source: &str) -> Option<Vec<Token<'_>>> {
    let chars: Vec<(usize, char)> = source.char_indices().collect();
    let mut result = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        let next = chars.get(i + 1).map(|v| v.1);
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && next == Some('/') {
            i += 2;
            while i < chars.len() && chars[i].1 != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i].1 == '*' && chars[i + 1].1 == '/') {
                i += 1;
            }
            if i + 1 >= chars.len() {
                return None;
            }
            i += 2;
            continue;
        }
        if c == '"' && next == Some('"') && chars.get(i + 2).is_some_and(|v| v.1 == '"') {
            i += 3;
            let mut closed = false;
            while i + 2 < chars.len() {
                if chars[i].1 == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i].1 == '"' && chars[i + 1].1 == '"' && chars[i + 2].1 == '"' {
                    i += 3;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                return None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                if chars[i].1 == '\\' {
                    i = (i + 2).min(chars.len());
                    continue;
                }
                if chars[i].1 == quote {
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                return None;
            }
            continue;
        }
        let start = i;
        let identifier = identifier_start(c);
        i += 1;
        if identifier {
            while i < chars.len() && (identifier_start(chars[i].1) || chars[i].1.is_numeric()) {
                i += 1;
            }
        }
        let byte_start = chars[start].0;
        let byte_end = chars.get(i).map_or(source.len(), |v| v.0);
        result.push(Token {
            text: &source[byte_start..byte_end],
            byte_start,
            byte_end,
            start,
            end: i,
            identifier,
        });
        if result.len() > MAX_TOKENS {
            return None;
        }
    }
    Some(result)
}

// Only generated declaration syntax is accepted: Type vN followed by an
// initializer/terminator, or Type pN followed by a parameter separator.
fn declaration_type<'a>(tokens: &'a [Token<'a>], index: usize) -> Option<(&'a str, bool, usize)> {
    if index == 0 || !matches!(tokens.get(index + 1)?.text, "=" | ";" | "," | ")" | ":") {
        return None;
    }
    let mut previous = index - 1;
    let mut array = false;
    while tokens[previous].text == "]" {
        if previous < 2 || tokens[previous - 1].text != "[" {
            return None;
        }
        array = true;
        previous -= 2;
    }
    if tokens[previous].text == ">" {
        let mut depth = 1usize;
        while previous > 0 && depth != 0 {
            previous -= 1;
            match tokens[previous].text {
                ">" => depth += 1,
                "<" => depth -= 1,
                _ => {}
            }
        }
        if depth != 0 || previous == 0 {
            return None;
        }
        previous -= 1;
    }
    let ty = &tokens[previous];
    if !ty.identifier || (keyword(ty.text) && !primitive(ty.text)) {
        return None;
    }
    // A declaration cannot have a member selector immediately before its name.
    let mut start = previous;
    while start >= 2 && tokens[start - 1].text == "." && tokens[start - 2].identifier {
        start -= 2;
    }
    if start > 0
        && matches!(
            tokens[start - 1].text,
            "return" | "throw" | "new" | "instanceof"
        )
    {
        return None;
    }
    Some((ty.text, array, start))
}

fn lower_camel(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let caps = chars.iter().take_while(|c| c.is_uppercase()).count();
    let lower = if caps > 1 && caps < chars.len() {
        caps - 1
    } else {
        caps.max(1)
    };
    chars
        .into_iter()
        .enumerate()
        .flat_map(|(i, c)| {
            if i < lower {
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
fn type_name(ty: &str, array: bool) -> String {
    if array {
        return match ty {
            "byte" => "data".into(),
            "char" => "chars".into(),
            "String" => "strings".into(),
            _ if primitive(ty) => "values".into(),
            _ => format!("{}Array", lower_camel(ty.rsplit('$').next().unwrap_or(ty))),
        };
    }
    match ty {
        "boolean" => "flag".into(),
        "byte" | "short" | "int" | "long" | "float" | "double" => "value".into(),
        "char" => "character".into(),
        "String" => "text".into(),
        "Object" => "value".into(),
        _ => lower_camel(ty.rsplit('$').next().unwrap_or(ty)),
    }
}
fn inferred_name(tokens: &[Token<'_>], index: usize, ty: &str, array: bool) -> String {
    if tokens.get(index + 1).is_some_and(|t| t.text == "=") {
        let mut depth = 0usize;
        let mut selected: Option<(usize, &str)> = None;
        for i in index + 2..tokens.len().min(index + 160) {
            if tokens[i].text == "new" {
                return type_name(ty, array);
            }
            if matches!(tokens[i].text, ";" | "{" | "}") {
                break;
            }
            // A chained expression's last outer call determines the result.
            // Calls nested inside its arguments must not supply the name.
            if tokens[i].identifier
                && tokens.get(i + 1).is_some_and(|t| t.text == "(")
                && selected.is_none_or(|(selected_depth, _)| depth <= selected_depth)
            {
                selected = Some((depth, tokens[i].text));
            }
            match tokens[i].text {
                "(" => depth += 1,
                ")" => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if let Some((_, call)) = selected {
            if let Some(suffix) = call
                .strip_prefix("get")
                .filter(|s| s.chars().next().is_some_and(char::is_uppercase))
            {
                return lower_camel(suffix);
            }
            if ty == "boolean" && !keyword(call) && !generated(call) && call.len() > 2 {
                return call.into();
            }
        }
    }
    type_name(ty, array)
}

struct Edit {
    byte_start: usize,
    byte_end: usize,
    start: usize,
    end: usize,
    replacement: String,
    delta_after: isize,
}
fn remap(position: usize, edits: &[Edit]) -> usize {
    let preceding = edits.partition_point(|edit| edit.end <= position);
    let delta = preceding.checked_sub(1).map_or(0, |i| edits[i].delta_after);
    if let Some(edit) = edits.get(preceding).filter(|edit| edit.start < position) {
        return (edit.start as isize + delta) as usize
            + (position - edit.start).min(edit.replacement.chars().count());
    }
    (position as isize + delta) as usize
}

pub(super) fn rename(mut code: DecompiledCode) -> DecompiledCode {
    if code.source.len() > 4 * 1024 * 1024 {
        return code;
    }
    let Some(tokens) = tokens(&code.source) else {
        return code;
    };
    let mut spans: Vec<_> = code
        .links
        .iter()
        .map(|link| (link.start, link.end))
        .chain(
            code.definitions
                .iter()
                .filter(|definition| {
                    matches!(definition.kind.as_str(), "class" | "method" | "field")
                })
                .map(|definition| (definition.start, definition.end)),
        )
        .collect();
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        if let Some(last) = merged.last_mut().filter(|last| last.1 >= start) {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    let mut protected = vec![false; tokens.len()];
    let mut span = 0;
    for (i, token) in tokens.iter().enumerate() {
        while merged.get(span).is_some_and(|range| range.1 <= token.start) {
            span += 1;
        }
        protected[i] = merged
            .get(span)
            .is_some_and(|range| token.start < range.1 && token.end > range.0);
    }
    let mut reserved: HashSet<String> = tokens
        .iter()
        .enumerate()
        .filter(|(i, token)| {
            token.identifier
                && !generated(token.text)
                && i.checked_sub(1).is_none_or(|p| tokens[p].text != ".")
                && tokens.get(i + 1).is_none_or(|next| next.text != "(")
        })
        .map(|(_, t)| t.text.to_string())
        .collect();
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let mut repeated = HashSet::new();
    let mut type_tokens = HashSet::new();
    for (i, token) in tokens.iter().enumerate() {
        if !generated(token.text) || protected[i] {
            continue;
        }
        if let Some((ty, array, start)) = declaration_type(&tokens, i) {
            for token in &tokens[start..i] {
                if token.identifier {
                    type_tokens.insert(token.text);
                }
            }
            if !seen.insert(token.text) {
                repeated.insert(token.text);
            }
            candidates.push((token.text, inferred_name(&tokens, i, ty, array)));
        }
    }
    let mut names = HashMap::new();
    let mut next_suffix = HashMap::new();
    for (original, base) in candidates {
        if repeated.contains(original)
            || type_tokens.contains(original)
            || base.is_empty()
            || generated(&base)
        {
            continue;
        }
        let base = if !base.chars().next().is_some_and(identifier_start)
            || !base.chars().all(|c| identifier_start(c) || c.is_numeric())
        {
            "value".to_string()
        } else {
            base
        };
        let base = if keyword(&base) {
            format!("{base}Value")
        } else {
            base
        };
        let mut name = base.clone();
        let mut suffix = *next_suffix.get(&base).unwrap_or(&2usize);
        while reserved.contains(&name) {
            name = format!("{base}{suffix}");
            suffix += 1;
        }
        next_suffix.insert(base, suffix);
        reserved.insert(name.clone());
        names.insert(original, name);
    }
    let mut edits = Vec::new();
    let mut delta = 0isize;
    for (i, token) in tokens.iter().enumerate() {
        if protected[i]
            || i.checked_sub(1).is_some_and(|p| tokens[p].text == ".")
            || tokens.get(i + 1).is_some_and(|next| next.text == "(")
        {
            continue;
        }
        let Some(name) = names.get(token.text) else {
            continue;
        };
        delta += name.chars().count() as isize - (token.end - token.start) as isize;
        edits.push(Edit {
            byte_start: token.byte_start,
            byte_end: token.byte_end,
            start: token.start,
            end: token.end,
            replacement: name.clone(),
            delta_after: delta,
        });
    }
    if edits.is_empty() {
        return code;
    }
    let mut source = String::with_capacity(code.source.len());
    let mut previous = 0;
    for edit in &edits {
        source.push_str(&code.source[previous..edit.byte_start]);
        source.push_str(&edit.replacement);
        previous = edit.byte_end;
    }
    source.push_str(&code.source[previous..]);
    for link in &mut code.links {
        link.start = remap(link.start, &edits);
        link.end = remap(link.end, &edits);
    }
    for definition in &mut code.definitions {
        definition.start = remap(definition.start, &edits);
        definition.end = remap(definition.end, &edits);
        if matches!(definition.kind.as_str(), "local" | "parameter")
            && let Some(name) = names.get(definition.name.as_str())
        {
            definition.name = name.clone();
        }
    }
    code.source_hash = source_identity(&source);
    code.source = source;
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CodeDefinition, CodeLink};
    fn code(source: &str) -> DecompiledCode {
        DecompiledCode {
            source: source.into(),
            links: vec![],
            definitions: vec![],
            source_hash: source_identity(source),
        }
    }
    #[test]
    fn types_and_calls_name_generated_bindings_consistently() {
        let c = rename(code(
            "public void test(Intent p0, Bundle p1) { byte[] v0 = p0.getData(); boolean v1 = p0.startsWith(p1); return v0; }",
        ));
        assert_eq!(
            c.source,
            "public void test(Intent intent, Bundle bundle) { byte[] data = intent.getData(); boolean startsWith = intent.startsWith(bundle); return data; }"
        );
        assert_eq!(c.source_hash, source_identity(&c.source));
    }
    #[test]
    fn members_comments_and_literals_remain_unchanged() {
        let c = rename(code(
            "void f(Intent p0) { Intent v0 = p0; this.v0 = v0; v0.v0(); // v0 p0\n String v1 = \"v0 \\\" p0\"; char v2 = 'v'; }",
        ));
        assert!(
            c.source
                .contains("this.v0 = intent2; intent2.v0(); // v0 p0"),
            "{}",
            c.source
        );
        assert!(c.source.contains("\"v0 \\\" p0\""));
    }
    #[test]
    fn collisions_are_avoided_across_the_whole_method() {
        let c = rename(code(
            "void f(Intent p0) { Object value = null; Intent intent = null; if (true) { Intent v0 = p0; } Intent v1 = p0; }",
        ));
        assert!(c.source.contains("Intent intent2"));
        assert!(c.source.contains("Intent intent3"));
        assert!(c.source.contains("Intent intent4"));
    }
    #[test]
    fn unicode_positions_keep_exact_link_and_definition_text() {
        let source = "void f(Intent p0) { String v0 = \"λ 🦀\"; p0.open(v0); }";
        let start = source[..source.find("open").unwrap()].chars().count();
        let mut c = code(source);
        c.links.push(CodeLink {
            start,
            end: start + 4,
            label: "Intent.open(String)V".into(),
        });
        c.definitions.push(CodeDefinition {
            start: 5,
            end: 6,
            kind: "method".into(),
            name: "f".into(),
        });
        let c = rename(c);
        assert_eq!(
            c.source
                .chars()
                .skip(c.links[0].start)
                .take(c.links[0].end - c.links[0].start)
                .collect::<String>(),
            "open"
        );
        assert_eq!(c.links[0].label, "Intent.open(String)V");
        assert_eq!(
            c.source
                .chars()
                .skip(c.definitions[0].start)
                .take(c.definitions[0].end - c.definitions[0].start)
                .collect::<String>(),
            "f"
        );
    }
    #[test]
    fn duplicate_generated_declarations_are_left_unchanged() {
        let source = "void f() { if (true) { Intent v0 = null; } else { Bundle v0 = null; } }";
        assert_eq!(rename(code(source)).source, source);
    }
    #[test]
    fn qualified_arrays_generics_and_acronyms_use_type_names() {
        let c = rename(code(
            "void f(java.net.URL p0, java.util.List<String> p1, byte[] p2) { return; }",
        ));
        assert!(c.source.contains("URL url"), "{}", c.source);
        assert!(c.source.contains("List<String> list"));
        assert!(c.source.contains("byte[] data"));
    }
    #[test]
    fn constructor_arguments_do_not_override_declared_type_name() {
        let c = rename(code(
            "void f(Intent p0) { File v0 = new File(p0.getData()); }",
        ));
        assert!(
            c.source.contains("File file = new File(intent.getData())"),
            "{}",
            c.source
        );
    }
    #[test]
    fn text_blocks_and_local_definition_spans_are_preserved() {
        let source = "void f(Intent p0) { String v0 = \"\"\"\n p0 \" v0 \" \n\"\"\"; return p0; }";
        let mut c = code(source);
        let start = source[..source.find("p0").unwrap()].chars().count();
        c.definitions.push(CodeDefinition {
            start,
            end: start + 2,
            kind: "parameter".into(),
            name: "p0".into(),
        });
        let c = rename(c);
        assert!(c.source.contains("\n p0 \" v0 \" \n"), "{}", c.source);
        let definition = &c.definitions[0];
        assert_eq!(definition.name, "intent");
        assert_eq!(
            c.source
                .chars()
                .skip(definition.start)
                .take(definition.end - definition.start)
                .collect::<String>(),
            "intent"
        );
    }
    #[test]
    fn generated_method_and_field_names_do_not_follow_local_renames() {
        let mut c = code("void v0(Intent p0) { Intent v0 = p0; this.p0 = p0; v0.p0(); v0(); }");
        c.definitions.push(CodeDefinition {
            start: 5,
            end: 7,
            kind: "method".into(),
            name: "v0".into(),
        });
        let c = rename(c);
        assert!(c.source.contains("void v0(Intent intent)"));
        assert!(
            c.source.contains("this.p0 = intent; intent2.p0(); v0();"),
            "{}",
            c.source
        );
        assert_eq!(c.definitions[0].name, "v0");
    }
    #[test]
    fn generated_type_names_and_invalid_anonymous_type_suffixes_are_safe() {
        let source = "void f(v0 p0) { Intent v0 = null; return; }";
        assert_eq!(rename(code(source)).source, source);
        let c = rename(code("void f(Foo$1 p0) { return; }"));
        assert!(c.source.contains("Foo$1 value"), "{}", c.source);
    }
    #[test]
    fn chained_boolean_uses_final_outer_call_not_receiver_or_arguments() {
        let c = rename(code(
            "void f(Uri p0) { boolean v0 = p0.toString().startsWith(p0.getPrefix()); boolean v1 = this.b.v(p0.getData()); }",
        ));
        assert!(
            c.source
                .contains("boolean startsWith = uri.toString().startsWith(uri.getPrefix())"),
            "{}",
            c.source
        );
        assert!(
            c.source.contains("boolean flag = this.b.v(uri.getData())"),
            "{}",
            c.source
        );
        assert!(!c.source.contains("boolean toString"));
    }
}
