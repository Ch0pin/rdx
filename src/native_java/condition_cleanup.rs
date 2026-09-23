//! Bounded presentation cleanup for emitter-owned Java, retaining source mappings.
use crate::engine::DecompiledCode;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_WORK: usize = 16 * 1024 * 1024;
const MAX_EDITS: usize = 8192;

struct Syntax {
    code: Vec<bool>,
    pair: Vec<usize>,
}

// Work in bytes for ASCII syntax, but convert every edit boundary to scalar
// offsets before touching engine metadata. Strings and comments are opaque.
fn syntax(source: &str) -> Option<Syntax> {
    let bytes = source.as_bytes();
    let mut code = vec![true; bytes.len()];
    let mut pair = vec![usize::MAX; bytes.len()];
    let mut stack = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                if quote == b'"' && bytes.get(i..i + 3) == Some(b"\"\"\"") {
                    return None; // Text-block indentation can affect string values.
                }
                i += 1;
                loop {
                    match *bytes.get(i)? {
                        b'\\' => i += 2,
                        b'\n' | b'\r' => return None,
                        value if value == quote => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                code[start..i].fill(false);
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                code[start..i].fill(false);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while bytes.get(i..i + 2) != Some(b"*/") {
                    if i >= bytes.len() {
                        return None;
                    }
                    i += 1;
                }
                i += 2;
                code[start..i].fill(false);
            }
            b'\\' => return None, // Do not reinterpret Java Unicode escapes in code.
            b'(' | b'[' | b'{' => {
                stack.push(i);
                i += 1;
            }
            b')' | b']' | b'}' => {
                let open = stack.pop()?;
                let expected = match bytes[i] {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if bytes[open] != expected {
                    return None;
                }
                pair[open] = i;
                pair[i] = open;
                i += 1;
            }
            _ => i += 1,
        }
    }
    stack.is_empty().then_some(Syntax { code, pair })
}

#[derive(Clone)]
struct Edit<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

struct Header {
    indent: usize,
    condition: usize,
    close: usize,
    brace: usize,
}

fn header(source: &str, start: usize, end: usize, syntax: &Syntax) -> Option<Header> {
    let line = source.get(start..end)?;
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    let begin = start + indent;
    if !source[begin..end].starts_with("if (") || !syntax.code[begin] {
        return None;
    }
    let open = begin + 3;
    let close = syntax.pair[open];
    if close >= end || source[close + 1..end].trim() != "{" {
        return None;
    }
    let brace = close + 1 + source[close + 1..end].find('{')?;
    Some(Header {
        indent,
        condition: open + 1,
        close,
        brace,
    })
}

fn has_else(source: &str, after: usize, syntax: &Syntax) -> bool {
    let bytes = source.as_bytes();
    let mut i = after;
    while i < bytes.len() && (!syntax.code[i] || bytes[i].is_ascii_whitespace()) {
        i += 1;
    }
    source.get(i..).is_some_and(|tail| {
        tail.starts_with("else")
            && tail
                .as_bytes()
                .get(4)
                .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
    })
}

// Atomic receivers/calls and negations bind more tightly than &&. Keep
// parentheses for operators we do not classify, especially || and assignments.
fn needs_group(source: &str, start: usize, end: usize, syntax: &Syntax) -> bool {
    let mut i = start;
    while i < end {
        let ch = source[i..].chars().next().unwrap();
        if !syntax.code[i] {
            i += ch.len_utf8();
        } else if matches!(ch, '(' | '[') && syntax.pair[i] < end {
            i = syntax.pair[i] + 1;
        } else if ch.is_alphanumeric() || ch.is_whitespace() || matches!(ch, '_' | '$' | '.' | '!')
        {
            i += ch.len_utf8();
        } else {
            return true;
        }
    }
    false
}

fn folded_guards(source: &str, syntax: &Syntax) -> Vec<Edit<'static>> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        lines.push((offset, offset + line.len()));
        offset += line.len();
    }
    let mut edits = Vec::new();
    let mut index = 0;
    while index + 1 < lines.len() && edits.len() < MAX_EDITS {
        let (start, end) = lines[index];
        let Some(outer) = header(source, start, end, syntax) else {
            index += 1;
            continue;
        };
        let (inner_start, inner_end) = lines[index + 1];
        let Some(inner) = header(source, inner_start, inner_end, syntax) else {
            index += 1;
            continue;
        };
        let outer_end = syntax.pair[outer.brace];
        let inner_close = syntax.pair[inner.brace];
        if inner.indent <= outer.indent
            || inner_close >= outer_end
            || has_else(source, outer_end + 1, syntax)
        {
            index += 1;
            continue;
        }
        let close_line = lines
            .partition_point(|(start, _)| *start <= inner_close)
            .saturating_sub(1);
        let outer_line = lines
            .partition_point(|(start, _)| *start <= outer_end)
            .saturating_sub(1);
        if outer_line != close_line + 1
            || source[lines[close_line].0..lines[close_line].1].trim() != "}"
            || source[lines[outer_line].0..lines[outer_line].1].trim() != "}"
        {
            index += 1;
            continue;
        }
        let dedent = inner.indent - outer.indent;
        let outer_group = needs_group(source, outer.condition, outer.close, syntax);
        let inner_group = needs_group(source, inner.condition, inner.close, syntax);
        let mut candidate = vec![
            Edit {
                start: outer.condition,
                end: outer.condition,
                text: if outer_group { "(" } else { "" },
            },
            Edit {
                start: outer.close,
                end: inner.condition,
                text: match (outer_group, inner_group) {
                    (true, true) => ") && (",
                    (true, false) => ") && ",
                    (false, true) => " && (",
                    (false, false) => " && ",
                },
            },
            Edit {
                start: inner.close + 1,
                end: inner.close + 1,
                text: if inner_group { ")" } else { "" },
            },
        ];
        let mut safe = true;
        for &(start, end) in &lines[index + 2..close_line] {
            // Do not touch literal or block-comment contents. Normal emitted
            // statement lines lose precisely one indentation level.
            if syntax.code[start] && !source[start..end].trim().is_empty() {
                if !source[start..end]
                    .bytes()
                    .take(dedent)
                    .all(|byte| byte == b' ')
                    || end - start < dedent
                {
                    safe = false;
                    break;
                }
                candidate.push(Edit {
                    start,
                    end: start + dedent,
                    text: "",
                });
            }
        }
        if safe && edits.len() + candidate.len() < MAX_EDITS {
            candidate.push(Edit {
                start: lines[close_line].0,
                end: lines[close_line].1,
                text: "",
            });
            edits.extend(candidate);
            index = outer_line + 1;
        } else {
            index += 1;
        }
    }
    edits
}

fn guard_parentheses(source: &str, syntax: &Syntax) -> Vec<Edit<'static>> {
    let mut edits = Vec::new();
    let mut start = 0;
    for line in source.split_inclusive('\n') {
        let end = start + line.len();
        if let Some(h) = header(source, start, end, syntax) {
            let (mut a, mut b) = (h.condition, h.close);
            while a < b && source.as_bytes()[a] == b'(' && syntax.pair[a] == b - 1 {
                a += 1;
                b -= 1;
            }
            if a != h.condition {
                edits.push(Edit {
                    start: h.condition,
                    end: a,
                    text: "",
                });
                edits.push(Edit {
                    start: b,
                    end: h.close,
                    text: "",
                });
            }
        }
        start = end;
        if edits.len() >= MAX_EDITS {
            break;
        }
    }
    edits
}

fn negations(source: &str, syntax: &Syntax) -> Vec<Edit<'static>> {
    let bytes = source.as_bytes();
    let mut edits = Vec::new();
    for i in 0..bytes.len().saturating_sub(2) {
        if !syntax.code[i] || bytes[i] != b'!' || bytes[i + 1] != b'(' {
            continue;
        }
        let close = syntax.pair[i + 1];
        if close == usize::MAX {
            continue;
        }
        let value = source[i + 2..close].trim();
        let mut chars = value.chars();
        if !chars
            .next()
            .is_some_and(|ch| ch.is_alphabetic() || matches!(ch, '_' | '$'))
            || !chars.all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$'))
        {
            continue;
        }
        let start = i + 2 + source[i + 2..close].find(value).unwrap();
        edits.push(Edit {
            start: i + 1,
            end: start,
            text: "",
        });
        edits.push(Edit {
            start: start + value.len(),
            end: close + 1,
            text: "",
        });
        if edits.len() >= MAX_EDITS {
            break;
        }
    }
    edits
}

fn apply(mut code: DecompiledCode, mut edits: Vec<Edit<'_>>) -> (DecompiledCode, bool) {
    if edits.is_empty() {
        return (code, false);
    }
    edits.sort_by_key(|edit| (edit.start, edit.end));
    if edits.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return (code, false);
    }
    let mut scalar = Vec::with_capacity(edits.len());
    let (mut byte, mut position) = (0, 0);
    for edit in &edits {
        position += code.source[byte..edit.start].chars().count();
        let end = position + code.source[edit.start..edit.end].chars().count();
        scalar.push((position, end, edit.text.chars().count()));
        byte = edit.end;
        position = end;
    }
    if (code.links.len() + code.definitions.len()).saturating_mul(scalar.len()) > 2_000_000 {
        return (code, false);
    }
    // Never remove or split an annotated token. If metadata unexpectedly spans
    // formatting syntax, retain the original source rather than discard links.
    for (start, end) in code.links.iter().map(|link| (link.start, link.end)).chain(
        code.definitions
            .iter()
            .map(|definition| (definition.start, definition.end)),
    ) {
        if scalar.iter().any(|&(a, b, _)| {
            if a == b {
                start < a && a < end
            } else {
                start < b && end > a
            }
        }) {
            return (code, false);
        }
    }
    let map = |offset: usize, beginning: bool| {
        let mut delta = 0isize;
        for &(start, end, replacement) in &scalar {
            if end < offset || (end == offset && (start != end || beginning)) {
                delta += replacement as isize - (end - start) as isize;
            }
        }
        offset.checked_add_signed(delta).unwrap_or(offset)
    };
    for link in &mut code.links {
        link.start = map(link.start, true);
        link.end = map(link.end, false);
    }
    for definition in &mut code.definitions {
        definition.start = map(definition.start, true);
        definition.end = map(definition.end, false);
    }
    let mut source = String::with_capacity(code.source.len());
    byte = 0;
    for edit in edits {
        source.push_str(&code.source[byte..edit.start]);
        source.push_str(edit.text);
        byte = edit.end;
    }
    source.push_str(&code.source[byte..]);
    code.source = source;
    code.source_hash = crate::engine::source_identity(&code.source);
    (code, true)
}

// Reordered source slices retain their original token mappings. Any annotated
// token removed or split by a presentation transform makes the candidate fail.
fn rearrange(code: &DecompiledCode, pieces: &[std::ops::Range<usize>]) -> Option<DecompiledCode> {
    let mut result = code.clone();
    result.source.clear();
    let mut mapped = vec![None; code.source.chars().count()];
    let mut scalar_at = vec![0; code.source.len() + 1];
    for (scalar, (byte, _)) in code.source.char_indices().enumerate() {
        scalar_at[byte] = scalar;
    }
    scalar_at[code.source.len()] = mapped.len();
    let mut next = 0;
    for piece in pieces {
        let text = code.source.get(piece.clone())?;
        for slot in &mut mapped[scalar_at[piece.start]..scalar_at[piece.end]] {
            if slot.replace(next).is_some() {
                return None;
            }
            next += 1;
        }
        result.source.push_str(text);
    }
    let relocate = |start: usize, end: usize| -> Option<(usize, usize)> {
        if start >= end {
            return None;
        }
        let first = *mapped.get(start)?.as_ref()?;
        for (index, item) in mapped.get(start..end)?.iter().enumerate() {
            if *item != Some(first + index) {
                return None;
            }
        }
        Some((first, first + end - start))
    };
    for link in &mut result.links {
        (link.start, link.end) = relocate(link.start, link.end)?;
    }
    for definition in &mut result.definitions {
        (definition.start, definition.end) = relocate(definition.start, definition.end)?;
    }
    result.links.sort_by_key(|link| (link.start, link.end));
    result
        .definitions
        .sort_by_key(|definition| (definition.start, definition.end));
    result.source_hash = crate::engine::source_identity(&result.source);
    Some(result)
}

// Only invert a terminal method-level guard: both paths already return, and
// the shorter failure path has no declarations whose scope could be changed.
fn orient_return_guard(code: &DecompiledCode, parsed: &Syntax) -> Option<DecompiledCode> {
    let source = &code.source;
    let mut offset = 0;
    let lines: Vec<_> = source
        .split_inclusive('\n')
        .map(|line| {
            let start = offset;
            offset += line.len();
            (start, offset)
        })
        .collect();
    let mut work = 0usize;
    for (index, &(start, end)) in lines.iter().enumerate() {
        let Some(h) = header(source, start, end, parsed) else {
            continue;
        };
        let condition = source[h.condition..h.close].trim();
        if !condition.starts_with('!') || condition.starts_with("!=") {
            continue;
        }
        let cond_start = h.condition + source[h.condition..h.close].find('!')?;
        let operand = cond_start + 1;
        // Negation must cover the entire condition, not `!a && b`.
        let atom = &source[operand..h.close];
        if !(atom.starts_with('(') && parsed.pair[operand] == h.close - 1
            || atom
                .chars()
                .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$'))
        {
            continue;
        }
        let close = parsed.pair[h.brace];
        let close_line = lines
            .partition_point(|&(s, _)| s <= close)
            .saturating_sub(1);
        if close_line <= index + 1 || source[lines[close_line].0..lines[close_line].1].trim() != "}"
        {
            continue;
        }
        work += start;
        if work > MAX_WORK {
            break;
        }
        let Some((parent, _)) = source.as_bytes()[..start]
            .iter()
            .enumerate()
            .rev()
            .find(|(i, b)| **b == b'{' && parsed.code[*i] && parsed.pair[*i] > close)
        else {
            continue;
        };
        let parent_close = parsed.pair[parent];
        let parent_line = lines
            .partition_point(|&(s, _)| s <= parent_close)
            .saturating_sub(1);
        if parent_line <= close_line + 1 {
            continue;
        }
        // This is a method body, never a loop/synchronized/try scope.
        let parent_start = source[..parent].rfind('\n').map_or(0, |p| p + 1);
        let parent_header = source[parent_start..parent].trim();
        if !parent_header.contains('(')
            || !parent_header.ends_with(')')
            || ["if", "for", "while", "switch", "catch", "synchronized"]
                .iter()
                .any(|keyword| parent_header.starts_with(&format!("{keyword} (")))
        {
            continue;
        }
        let body = &lines[index + 1..close_line];
        let tail = &lines[close_line + 1..parent_line];
        if tail.len() > 4
            || tail.len() >= body.len()
            || source[body.last()?.0..body.last()?.1].trim() != "return;"
            || source[tail.last()?.0..tail.last()?.1].trim() != "return;"
        {
            continue;
        }
        // Tail contains only standalone calls and return. No declarations,
        // comments, braces, labels, or condition-dependent fall-through.
        if !tail.iter().all(|&(a, b)| {
            let text = source[a..b].trim();
            text == "return;"
                || (text.ends_with(");")
                    && !text.contains('=')
                    && !text.contains('{')
                    && !text.contains('}')
                    && parsed.code[a..b].iter().all(|value| *value))
        }) {
            continue;
        }
        let dedent = 4;
        if !body
            .iter()
            .all(|&(a, b)| b - a >= dedent && source[a..a + dedent] == *"    " && parsed.code[a])
        {
            continue;
        }
        let mut pieces = vec![0..cond_start, operand..end];
        for &(a, b) in tail {
            pieces.push(start..start + dedent);
            pieces.push(a..b);
        }
        pieces.push(lines[close_line].0..lines[close_line].1);
        for &(a, b) in body {
            pieces.push(a + dedent..b);
        }
        pieces.push(lines[parent_line].0..source.len());
        // Reusing indentation slices violates one-to-one mapping. They have no
        // tokens, but use distinct original indentation from the moved body.
        for (n, _) in tail.iter().enumerate() {
            pieces[2 + n * 2] = body[n].0..body[n].0 + dedent;
        }
        if let Some(result) = rearrange(code, &pieces) {
            return Some(result);
        }
    }
    None
}

fn conditional_assignment(mut code: DecompiledCode, work: &mut usize) -> DecompiledCode {
    // Work only on immediately declared locals and two single-expression arms.
    // Numeric arms are restricted to int constants: general Java ternaries can
    // introduce numeric promotion/unboxing absent from the original branches.
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?m)^([ ]*)([\w.$]+) ([\w$]+);\n([ ]*)if \(").unwrap()
    });
    for _ in 0..32 {
        *work += code.source.len();
        if *work > MAX_WORK || !code.source.contains("} else {") || !PATTERN.is_match(&code.source)
        {
            break;
        }
        let Some(parsed) = syntax(&code.source) else {
            break;
        };
        let mut replacement = None;
        for captures in PATTERN.captures_iter(&code.source) {
            if captures[1] != captures[4] {
                continue;
            }
            let found = captures.get(0).unwrap();
            let open = found.end() - 1;
            if !parsed.code[open] {
                continue;
            }
            let close = parsed.pair[open];
            let Some(tail) = code.source.get(close + 1..) else {
                continue;
            };
            if !tail.starts_with(" {\n") {
                continue;
            }
            let brace = close + 2;
            let first_end = parsed.pair[brace];
            let middle = &code.source[first_end..];
            if !middle.starts_with("} else {\n") {
                continue;
            }
            let other_brace = first_end + 7;
            let second_end = parsed.pair[other_brace];
            let name = &captures[3];
            let ty = &captures[2];
            let parse_arm = |start: usize, end: usize| -> Option<(usize, usize)> {
                let text = code.source[start..end].trim();
                let prefix = format!("{name} = ");
                let value = text.strip_prefix(&prefix)?.strip_suffix(';')?;
                if value.contains('\n') || value.contains(';') {
                    return None;
                }
                let a = start + code.source[start..end].find(text)? + prefix.len();
                Some((a, a + value.len()))
            };
            let Some((a, b)) = parse_arm(brace + 1, first_end) else {
                continue;
            };
            let Some((c, d)) = parse_arm(other_brace + 1, second_end) else {
                continue;
            };
            let left = &code.source[a..b];
            let right = &code.source[c..d];
            let safe = match ty {
                "int" => left.parse::<i32>().is_ok() && right.parse::<i32>().is_ok(),
                "boolean" => [left, right].iter().all(|v| matches!(*v, "true" | "false")),
                "byte" | "short" | "char" | "long" | "float" | "double" => false,
                _ => left == "null" || right == "null",
            };
            if !safe {
                continue;
            }
            let declaration_end = captures.get(3).unwrap().end();
            // Keep operand tokens as original source slices; only formatting
            // and duplicate assignment syntax are removed by apply().
            let prefix = " = (";
            let edits = vec![
                Edit {
                    start: declaration_end,
                    end: open + 1,
                    text: prefix,
                },
                Edit {
                    start: close,
                    end: a,
                    text: ") ? (",
                },
                Edit {
                    start: b,
                    end: c,
                    text: ") : (",
                },
                Edit {
                    start: d,
                    end: second_end + 1,
                    text: ");",
                },
            ];
            let (updated, changed) = apply(code.clone(), edits);
            if changed {
                replacement = Some(updated);
                break;
            }
        }
        let Some(updated) = replacement else {
            break;
        };
        code = updated;
    }
    code
}

pub(super) fn simplify(mut code: DecompiledCode) -> DecompiledCode {
    if code.source.len() > MAX_BYTES
        || (!code.source.contains("if (") && !code.source.contains("!("))
    {
        return code;
    }
    let mut work = 0usize;
    if code.source.contains("if (!")
        && let Some(parsed) = syntax(&code.source)
        && let Some(oriented) = orient_return_guard(&code, &parsed)
    {
        code = oriented;
    }
    work += code.source.len();
    code = conditional_assignment(code, &mut work);
    for _ in 0..16 {
        work += code.source.len();
        if work > MAX_WORK {
            return code;
        }
        let Some(parsed) = syntax(&code.source) else {
            return code;
        };
        let edits = folded_guards(&code.source, &parsed);
        let (updated, changed) = apply(code, edits);
        code = updated;
        if !changed {
            break;
        }
    }
    if work + code.source.len() <= MAX_WORK
        && let Some(parsed) = syntax(&code.source)
    {
        let edits = negations(&code.source, &parsed);
        code = apply(code, edits).0;
    }
    if work + code.source.len() <= MAX_WORK
        && let Some(parsed) = syntax(&code.source)
    {
        let edits = guard_parentheses(&code.source, &parsed);
        code = apply(code, edits).0;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CodeDefinition, CodeLink};

    fn input(source: &str) -> DecompiledCode {
        DecompiledCode {
            source: source.into(),
            source_hash: crate::engine::source_identity(source),
            links: vec![],
            definitions: vec![],
        }
    }
    #[test]
    fn terminal_negative_guard_moves_failure_first_and_preserves_unicode_links() {
        let source = "// 🦀\nclass X {\n    void f() {\n        before();\n        if (!(enabled())) {\n            normal();\n            more();\n            after();\n            return;\n        }\n        fail();\n        return;\n    }\n}\n";
        let mut code = input(source);
        for token in ["enabled", "normal", "more", "after", "fail"] {
            let start = source[..source.find(token).unwrap()].chars().count();
            code.links.push(CodeLink {
                start,
                end: start + token.len(),
                label: token.into(),
            });
        }
        let result = simplify(code);
        assert!(result.source.contains("if (enabled()) {\n            fail();\n            return;\n        }\n        normal();"), "{}", result.source);
        for link in &result.links {
            assert_eq!(
                result
                    .source
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>(),
                link.label
            );
        }
        assert_eq!(simplify(result.clone()).source, result.source);
    }

    #[test]
    fn ternary_preserves_conditional_call_and_int_assignments() {
        for (ty, a, b) in [
            ("String", "session.getName()", "null"),
            ("int", "2", "25"),
            ("boolean", "true", "false"),
        ] {
            let source = format!(
                "{ty} result;\nif (check()) {{\n    result = {a};\n}} else {{\n    result = {b};\n}}\n"
            );
            let result = simplify(input(&source));
            assert!(
                result
                    .source
                    .contains(&format!("result = (check()) ? ({a}) : ({b});")),
                "{}",
                result.source
            );
            assert_eq!(result.source.matches("check()").count(), 1);
        }
    }

    #[test]
    fn guard_parentheses_keep_receiver_cast_and_operator_grouping() {
        for (condition, expected) in [
            ("(((aqtw) this.c.a()).e())", "((aqtw) this.c.a()).e()"),
            ("((a || b) && c)", "(a || b) && c"),
            ("((a) || (b))", "(a) || (b)"),
        ] {
            let source = format!("if ({condition}) {{\n    call();\n}}\n");
            assert_eq!(
                simplify(input(&source)).source,
                format!("if ({expected}) {{\n    call();\n}}\n")
            );
        }
        let source = "String text;\nif (session != null) {\n    text = session.getName();\n} else {\n    text = null;\n}\n";
        let result = simplify(input(source));
        assert_eq!(
            result.source,
            "String text = (session != null) ? (session.getName()) : (null);\n"
        );
    }

    #[test]
    fn assignment_and_guard_rejections_preserve_scopes_and_numeric_types() {
        for source in [
            "Object x;\nif (a) {\n    x = 1;\n} else {\n    x = 2.0;\n}\n",
            "int x;\nif (a) {\n    x = call();\n} else {\n    x = 1;\n}\n",
            "String x;\nif (a) {\n    extra();\n    x = get();\n} else {\n    x = null;\n}\n",
            "void f() {\n    if (!a && b) {\n        one();\n        two();\n        return;\n    }\n    fail();\n    return;\n}\n",
            "void f() {\n    if (!a) {\n        one();\n        two();\n        return;\n    }\n    Object x = make();\n    return;\n}\n",
        ] {
            assert_eq!(simplify(input(source)).source, source);
        }
    }

    #[test]
    fn combines_guards_with_short_circuit_order_and_dedents_body() {
        let source = "class X {\n    void f() {\n        if (first()) {\n            if (second()) {\n                effect();\n            }\n        }\n    }\n}\n";
        let result = simplify(input(source));
        assert!(
            result
                .source
                .contains("if (first() && second()) {\n            effect();\n        }"),
            "{}",
            result.source
        );
        for token in ["first()", "second()", "effect()"] {
            assert_eq!(result.source.matches(token).count(), 1);
        }
        assert!(result.source.find("first()").unwrap() < result.source.find("second()").unwrap());
        assert_eq!(
            result.source_hash,
            crate::engine::source_identity(&result.source)
        );
        assert_eq!(simplify(result.clone()).source, result.source);
    }
    #[test]
    fn folded_guards_preserve_operator_precedence() {
        for (outer, inner, expected) in [
            ("a || b", "c", "(a || b) && c"),
            ("a", "b || c", "a && (b || c)"),
            ("a || b", "c || d", "(a || b) && (c || d)"),
            ("a = call()", "b", "(a = call()) && b"),
            ("a ? b : c", "d", "(a ? b : c) && d"),
            ("!ready()", "this.flag", "!ready() && this.flag"),
        ] {
            let source =
                format!("if ({outer}) {{\n    if ({inner}) {{\n        hit();\n    }}\n}}\n");
            let output = simplify(input(&source));
            assert!(
                output.source.contains(&format!("if ({expected})")),
                "{}",
                output.source
            );
            assert_eq!(output.source.matches("hit()").count(), 1);
        }
    }

    #[test]
    fn else_guards_and_trailing_statements_are_not_folded() {
        for source in [
            "if (a) {\n    if (b) {\n        f();\n    } else {\n        g();\n    }\n}\n",
            "if (a) {\n    if (b) {\n        f();\n    }\n} else {\n    g();\n}\n",
            "if (a) {\n    if (b) {\n        f();\n    }\n}\n/* keep else */ else {\n    g();\n}\n",
            "if (a) {\n    if (b) {\n        f();\n    }\n    g();\n}\n",
            "if (a) {\n    sideEffect();\n    if (b) {\n        f();\n    }\n}\n",
        ] {
            assert_eq!(simplify(input(source)).source, source);
        }
    }
    #[test]
    fn literal_and_comment_content_is_opaque() {
        let source = "if (a) {\n    if (b) {\n        String s = \"!(flag) } if (x) {\";\n        char c = '}'; // !(flag)\n        /* !(flag)\n           if (x) { */\n    }\n}\n";
        let output = simplify(input(source)).source;
        assert!(output.contains("if (a && b)"));
        assert!(output.contains("\"!(flag) } if (x) {\""));
        assert!(output.contains("// !(flag)"));
        assert!(output.contains("/* !(flag)\n           if (x) { */"));
        let block = "if (a) {\n    if (b) {\n        String s = \"\"\"\n          !(flag)\n        \"\"\";\n    }\n}\n";
        assert_eq!(simplify(input(block)).source, block);
    }
    #[test]
    fn scalar_links_and_definitions_survive_condition_and_negation_edits() {
        let source = "// 🦀\nclass Δ {\n    void f() {\n        if (!(α)) {\n            if (β()) {\n                γ();\n            }\n        }\n    }\n}\n";
        let mut code = input(source);
        for token in ["Δ", "α", "β", "γ"] {
            let byte = source.find(token).unwrap();
            let start = source[..byte].chars().count();
            code.links.push(CodeLink {
                start,
                end: start + token.chars().count(),
                label: token.into(),
            });
        }
        code.definitions.push(CodeDefinition {
            start: code.links[0].start,
            end: code.links[0].end,
            name: "Δ".into(),
            kind: "class".into(),
        });
        let output = simplify(code);
        assert!(
            output.source.contains("if (!α && β())"),
            "{}",
            output.source
        );
        for link in &output.links {
            let value: String = output
                .source
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect();
            assert_eq!(value, link.label);
        }
        let definition = &output.definitions[0];
        assert_eq!(output.source.chars().nth(definition.start), Some('Δ'));
        assert_eq!(definition.end - definition.start, 1);
        assert_eq!(
            output.source_hash,
            crate::engine::source_identity(&output.source)
        );
    }
    #[test]
    fn negation_only_removes_parentheses_around_single_identifiers() {
        let source = "boolean a = !(flag); boolean b = !( α ); boolean c = !(left && right); boolean d = !(call()); String s = \"!(flag)\"; // !(flag)\n";
        let result = simplify(input(source));
        assert_eq!(
            result.source,
            "boolean a = !flag; boolean b = !α; boolean c = !(left && right); boolean d = !(call()); String s = \"!(flag)\"; // !(flag)\n"
        );
    }
    #[test]
    fn nested_chain_folds_without_duplicate_effects_or_scope_loss() {
        let source = "if (a()) {\n    if (b()) {\n        if (c()) {\n            hit();\n        }\n    }\n}\n";
        let result = simplify(input(source));
        assert_eq!(result.source.matches("if (").count(), 1);
        assert_eq!(result.source.matches("hit()").count(), 1);
        assert!(result.source.find("a()") < result.source.find("b()"));
        assert!(result.source.find("b()") < result.source.find("c()"));
    }
    #[test]
    fn unexpected_metadata_spans_and_malformed_input_remain_unchanged() {
        let source = "if (a) {\n    if (b) {\n        f();\n    }\n}\n";
        let mut code = input(source);
        code.links.push(CodeLink {
            start: 0,
            end: source.chars().count(),
            label: "whole-block".into(),
        });
        assert_eq!(simplify(code).source, source);
        for source in [
            "if (a) {\n    if (b) {\n",
            "String s = \"unterminated",
            "// !(flag)\n/* unterminated",
        ] {
            assert_eq!(simplify(input(source)).source, source);
        }
    }
}
