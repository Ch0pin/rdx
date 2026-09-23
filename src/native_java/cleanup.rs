//! Bounded cleanup of emitter-owned locals, never arbitrary Java input.
use super::MethodBody;
use crate::engine::CodeLink;
use std::collections::{HashMap, HashSet};

struct Edit {
    start: usize,
    end: usize,
    text: String,
    links: Vec<CodeLink>,
}

fn enclosed(expression: &str) -> bool {
    if !expression.starts_with('(') {
        return false;
    }
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (position, c) in expression.char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == delimiter {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                let Some(next) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next;
                if depth == 0 {
                    return position + 1 == expression.len();
                }
            }
            _ => {}
        }
    }
    false
}

fn primary_call(expression: &str) -> bool {
    if !expression
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
    {
        return false;
    }
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut call = false;
    for c in expression.chars() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == delimiter {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' if depth > 0 => quote = Some(c),
            '(' => {
                depth += 1;
                call = true;
            }
            ')' => {
                let Some(next) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next;
            }
            c if depth > 0 || c.is_alphanumeric() || matches!(c, '_' | '$' | '.') => {}
            _ => return false,
        }
    }
    call && depth == 0 && quote.is_none()
}

fn first_argument_or_cast(
    body: &MethodBody,
    receiver_start: usize,
    receiver: &str,
    name: &str,
    declaration: &str,
) -> bool {
    // The guard's first evaluated expression may contain the same cast/call
    // chain. Strip only enclosing parentheses and unary negation; never cross
    // a short-circuit operand or another expression.
    if let Some(condition) = receiver
        .strip_prefix("if (")
        .and_then(|s| s.trim_end().strip_suffix(") {"))
    {
        let mut condition = condition;
        let mut begin = receiver_start + 4;
        loop {
            if enclosed(condition) {
                condition = &condition[1..condition.len() - 1];
                begin += 1;
            } else if let Some(rest) = condition.strip_prefix('!') {
                condition = rest;
                begin += 1;
            } else {
                break;
            }
        }
        return first_argument_or_cast(body, begin, condition, name, declaration);
    }
    // The first this/super argument has no receiver evaluation before it.
    // The caller proves adjacency and exactly one use, so substituting a read
    // here preserves both DEX evaluation order and volatile/read counts.
    if let Some(argument) = receiver
        .strip_prefix("this(")
        .or_else(|| receiver.strip_prefix("super("))
    {
        let direct = argument
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with(')') || rest.starts_with(','));
        let cast = argument.strip_prefix("((").is_some_and(|rest| {
            rest.split_once(") ").is_some_and(|(ty, value)| {
                ty.split('.').all(super::super::identifier)
                    && value
                        .strip_prefix(name)
                        .is_some_and(|tail| tail.starts_with("),") || tail.starts_with("))"))
            })
        });
        if direct || cast {
            return true;
        }
    }
    // Cast operands evaluate before the runtime check, as in the original DEX.
    if let Some(rest) = receiver.strip_prefix("((")
        && let Some((ty, value)) = rest.split_once(") ")
        && ty.split('.').all(super::super::identifier)
        && (value.trim() == format!("{name});") || value.trim().starts_with(&format!("{name}).")))
    {
        return true;
    }
    let Some(open) = receiver.find('(') else {
        return false;
    };
    let Some((target, method)) = receiver[..open].rsplit_once('.') else {
        return false;
    };
    let Some(rest) = receiver[open + 1..].strip_prefix(name) else {
        return false;
    };
    if !rest.starts_with(')') && !rest.starts_with(',') {
        return false;
    }
    let stable_target = target == "this"
        || target
            .strip_prefix('v')
            .or_else(|| target.strip_prefix('p'))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
    let target_links = body
        .links
        .partition_point(|link| link.start < receiver_start);
    let static_target = body.links[target_links..]
        .iter()
        .take_while(|link| link.start == receiver_start)
        .any(|link| {
            link.start == receiver_start
                && link.end == receiver_start + target.chars().count()
                && !link.label.contains('(')
        });
    if !stable_target && !static_target {
        return false;
    }
    let method_start = receiver_start + target.chars().count() + 1;
    let method_links = body.links.partition_point(|link| link.start < method_start);
    let Some(link) = body.links[method_links..]
        .iter()
        .take_while(|link| link.start == method_start)
        .find(|link| {
            link.start == method_start
                && link.end == method_start + method.chars().count()
                && link.label.contains('(')
        })
    else {
        return false;
    };
    let Some(parameters) = link
        .label
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(')').map(|p| p.0))
    else {
        return false;
    };
    let dimensions = parameters.bytes().take_while(|b| *b == b'[').count();
    let Some(base) = parameters.get(dimensions..) else {
        return false;
    };
    let length = if base.starts_with('L') {
        let Some(end) = base.find(';') else {
            return false;
        };
        dimensions + end + 1
    } else {
        dimensions + 1
    };
    let Some(descriptor) = parameters.get(..length) else {
        return false;
    };
    let Some(ty) = declaration.strip_suffix(name).map(str::trim) else {
        return false;
    };
    super::super::java_type(descriptor).is_ok_and(|expected| expected == ty)
}

// Count exact identifiers outside literals/comments. Positions are Unicode scalar
// offsets, matching the viewer's navigation contract.
fn uses(text: &str, names: &HashSet<String>) -> HashMap<String, Vec<(usize, usize)>> {
    let chars: Vec<_> = text.chars().collect();
    let mut found: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '\'' | '"') {
            let quote = chars[i];
            i += 1;
            while i < chars.len() {
                let c = chars[i];
                i += 1;
                if c == '\\' {
                    i = (i + 1).min(chars.len());
                } else if c == quote {
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
            let name: String = chars[start..i].iter().collect();
            if names.contains(&name) {
                found.entry(name).or_default().push((start, i));
            }
        } else {
            i += 1;
        }
    }
    found
}

pub(super) fn inline_receivers(body: &mut MethodBody, names: &HashSet<String>) {
    inline_locals(body, names, false);
}

pub(super) fn readable(body: &mut MethodBody) {
    inline_class_literals(body);
    let names = body
        .text
        .lines()
        .filter_map(|line| {
            let (declaration, expression) = line.trim().strip_suffix(';')?.split_once(" = ")?;
            if expression.contains("new ") {
                return None;
            }
            let name = declaration.split_whitespace().last()?;
            let digits = name.strip_prefix('v')?;
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| name.to_owned())
        })
        .collect();
    inline_locals(body, &names, true);
    standalone_casts(body);
}

fn inline_locals(body: &mut MethodBody, names: &HashSet<String>, arguments: bool) {
    if names.is_empty() {
        return;
    }
    // Fixed passes prevent quadratic repeated whole-method rewriting on long chains.
    for _ in 0..8 {
        body.links.sort_by_key(|link| link.start);
        let counts = uses(&body.text, names);
        let mut offset = 0;
        let lines: Vec<_> = body
            .text
            .split_inclusive('\n')
            .map(|line| {
                let start = offset;
                offset += line.chars().count();
                (start, line)
            })
            .collect();
        let mut edits = Vec::new();
        let mut consumed = false;
        for pair in lines.windows(2) {
            if consumed {
                consumed = false;
                continue;
            }
            let (start, line) = pair[0];
            let (next_start, next) = pair[1];
            let Some((declaration, expression)) = line
                .trim()
                .strip_suffix(';')
                .and_then(|s| s.split_once(" = "))
            else {
                continue;
            };
            let Some(name) = declaration.split_whitespace().last() else {
                continue;
            };
            let Some(occurrences) = counts.get(name).filter(|v| v.len() == 2) else {
                continue;
            };
            // Both statements must be in exactly the same lexical block.
            let indent = line.len() - line.trim_start().len();
            if indent != next.len() - next.trim_start().len() {
                continue;
            }
            let next_text = next.trim_start();
            let grouped_negation = next_text.starts_with(&format!("if (!({name})) {{"));
            let negated_condition = declaration.starts_with("boolean ")
                && (next_text.starts_with(&format!("if (!{name}) {{")) || grouped_negation);
            let direct_condition = declaration.starts_with("boolean ")
                && (next_text.starts_with(&format!("if ({name}) {{")) || negated_condition);
            // `this` cannot be null and has no evaluation effects. Do not extend
            // this to arbitrary field/array receivers, which can throw first.
            let direct_assignment = next_text
                .strip_suffix(";\n")
                .or_else(|| next_text.strip_suffix(';'))
                .and_then(|s| s.split_once(" = "))
                .is_some_and(|(lhs, rhs)| {
                    rhs == name
                        && (lhs
                            .strip_prefix("this.")
                            .is_some_and(super::super::identifier)
                            || lhs.strip_prefix('v').is_some_and(|suffix| {
                                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                            }))
                });
            let receiver = if direct_assignment {
                next_text
            } else if let Some((decl, rhs)) = next_text.split_once(" = ") {
                // Only another emitter-owned local declaration, never a field
                // assignment whose receiver could have observable evaluation.
                if !decl
                    .split_whitespace()
                    .last()
                    .is_some_and(|n| names.contains(n))
                {
                    continue;
                }
                rhs
            } else {
                next_text
            };
            let receiver_start = next_start + next[..next.len() - receiver.len()].chars().count();
            let extended = arguments
                && (direct_assignment
                    || first_argument_or_cast(body, receiver_start, receiver, name, declaration));
            if !direct_condition && !receiver.starts_with(&format!("{name}.")) && !extended {
                continue;
            }
            let use_start = if direct_condition {
                next_start
                    + indent
                    + 4
                    + usize::from(negated_condition)
                    + usize::from(grouped_negation)
            } else if extended {
                occurrences[1].0
            } else {
                next_start + next[..next.len() - receiver.len()].chars().count()
            };
            if occurrences[1].0 != use_start {
                continue;
            }
            let expr_byte = line.find(" = ").unwrap() + 3;
            let expr_start = start + line[..expr_byte].chars().count();
            let expr_end = expr_start + expression.chars().count();
            let path = |text: &str| {
                text.split('.').enumerate().all(|(i, part)| {
                    super::super::identifier(part) || (i == 0 && matches!(part, "this" | "super"))
                })
            };
            let call = primary_call(expression);
            let already_primary = path(expression) || enclosed(expression) || call;
            let wrap = usize::from(
                (negated_condition && !grouped_negation && !already_primary)
                    || (!direct_condition
                        && (!extended || receiver.starts_with("(("))
                        && !already_primary),
            );
            let link_start = body.links.partition_point(|link| link.start < expr_start);
            let copied_links = body.links[link_start..]
                .iter()
                .take_while(|link| link.start < expr_end)
                .filter(|l| l.start >= expr_start && l.end <= expr_end)
                .map(|l| CodeLink {
                    start: l.start - expr_start + wrap,
                    end: l.end - expr_start + wrap,
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
                text: if wrap == 1 {
                    format!("({expression})")
                } else {
                    expression.into()
                },
                links: copied_links,
            });
            consumed = true;
        }
        if edits.is_empty() {
            break;
        }
        apply_edits(body, edits);
    }
}

fn apply_edits(body: &mut MethodBody, mut edits: Vec<Edit>) {
    body.links.sort_by_key(|link| link.start);
    edits.sort_by_key(|e| e.start);
    let chars: Vec<_> = body.text.chars().collect();
    let mut result = String::new();
    let mut links = Vec::new();
    let mut cursor = 0;
    let mut written = 0;
    // Map unchanged intervals and moved expression links independently.
    for edit in edits.into_iter().chain(std::iter::once(Edit {
        start: chars.len(),
        end: chars.len(),
        text: String::new(),
        links: vec![],
    })) {
        result.extend(chars[cursor..edit.start].iter());
        let link_start = body.links.partition_point(|link| link.start < cursor);
        for link in body.links[link_start..]
            .iter()
            .take_while(|link| link.start < edit.start)
        {
            if link.start >= cursor && link.end <= edit.start {
                links.push(CodeLink {
                    start: written + link.start - cursor,
                    end: written + link.end - cursor,
                    label: link.label.clone(),
                });
            }
        }
        written += edit.start - cursor;
        for mut link in edit.links {
            link.start += written;
            link.end += written;
            links.push(link);
        }
        written += edit.text.chars().count();
        result.push_str(&edit.text);
        cursor = edit.end;
    }
    links.sort_by_key(|l| (l.start, l.end));
    body.text = result;
    body.links = links;
}

fn generated_local(name: &str) -> bool {
    name.strip_prefix('v')
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

// Permit only a direct call with stable earlier arguments. In particular, do
// not delay first class resolution past another call, branch, or field read.
fn stable_call_prefix(original: &str, start: usize, links: &[CodeLink]) -> bool {
    let mut prefix = original.trim();
    if let Some((_, rhs)) = prefix.split_once(" = ") {
        prefix = rhs;
    }
    if let Some(rest) = prefix.strip_prefix("((") {
        let Some((ty, rest)) = rest.split_once(") ") else {
            return false;
        };
        if !ty.split('.').all(super::super::identifier) {
            return false;
        }
        prefix = rest;
    }
    let Some((target, arguments)) = prefix.split_once('(') else {
        return false;
    };
    let Some((receiver, method)) = target.rsplit_once('.') else {
        return false;
    };
    if !super::super::identifier(method) {
        return false;
    }
    let receiver_start = start + original[..original.find(prefix).unwrap()].chars().count();
    let stable_receiver = receiver == "this"
        || generated_local(receiver)
        || receiver
            .strip_prefix('p')
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
    let static_receiver = links.iter().any(|l| {
        l.start == receiver_start
            && l.end == receiver_start + receiver.chars().count()
            && l.label == receiver
    });
    if !stable_receiver && !static_receiver {
        return false;
    }
    arguments.split(',').all(|arg| {
        let arg = arg.trim();
        arg.is_empty()
            || arg == "this"
            || arg == "null"
            || generated_local(arg)
            || arg
                .strip_prefix('p')
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn inline_class_literals(body: &mut MethodBody) {
    if body.text.len() > 512 * 1024 || !body.text.contains(".class;") {
        return;
    }
    let mut offset = 0;
    let lines: Vec<_> = body
        .text
        .split_inclusive('\n')
        .map(|line| {
            let start = offset;
            offset += line.chars().count();
            (start, line)
        })
        .collect();
    let mut candidates = Vec::new();
    for pair in lines.windows(2) {
        let (start, line) = pair[0];
        let Some((decl, expr)) = line
            .trim()
            .strip_suffix(';')
            .and_then(|s| s.split_once(" = "))
        else {
            continue;
        };
        let Some((ty, name)) = decl.rsplit_once(' ') else {
            continue;
        };
        if !matches!(ty, "java.lang.Class" | "Class") || !generated_local(name) {
            continue;
        }
        let Some(ty) = expr.strip_suffix(".class") else {
            continue;
        };
        if !ty.split('.').all(super::super::identifier) {
            continue;
        }
        candidates.push((start, pair[1].0, pair[1].1, name, expr, line));
    }
    if candidates.is_empty() || candidates.len().saturating_mul(body.links.len()) > 2_000_000 {
        return;
    }
    let names = candidates.iter().map(|c| c.3.to_owned()).collect();
    let counts = uses(&body.text, &names);
    let chars: Vec<_> = body.text.chars().collect();
    let mut edits = Vec::new();
    for (start, next, next_line, name, expr, line) in candidates {
        let Some(occurrences) = counts.get(name).filter(|v| v.len() >= 2) else {
            continue;
        };
        let first = occurrences[1].0;
        if first < next || first >= next + next_line.chars().count() {
            continue;
        }
        let prefix: String = chars[next..first].iter().collect();
        if !stable_call_prefix(&prefix, next, &body.links) {
            continue;
        }
        if occurrences[1..].iter().any(|&(a, b)| {
            (a > 0 && chars[a - 1] == '.')
                || chars[b..]
                    .iter()
                    .find(|c| !c.is_whitespace())
                    .is_some_and(|c| matches!(c, '=' | '+' | '-'))
        }) {
            continue;
        }
        let expr_start = start + line[..line.find(" = ").unwrap() + 3].chars().count();
        let expr_end = expr_start + expr.chars().count();
        let links: Vec<_> = body
            .links
            .iter()
            .filter(|l| l.start >= expr_start && l.end <= expr_end)
            .map(|l| CodeLink {
                start: l.start - expr_start,
                end: l.end - expr_start,
                label: l.label.clone(),
            })
            .collect();
        edits.push(Edit {
            start,
            end: next,
            text: String::new(),
            links: vec![],
        });
        for &(start, end) in &occurrences[1..] {
            edits.push(Edit {
                start,
                end,
                text: expr.into(),
                links: links.clone(),
            });
        }
    }
    if !edits.is_empty()
        && edits.len() <= 8192
        && edits.iter().map(|e| e.text.len()).sum::<usize>() <= 4 * 1024 * 1024
    {
        apply_edits(body, edits);
    }
}

fn standalone_casts(body: &mut MethodBody) {
    if !body.text.contains(" = ((") {
        return;
    }
    let mut offset = 0;
    let mut edits = Vec::new();
    for line in body.text.split_inclusive('\n') {
        let start = offset;
        offset += line.chars().count();
        let Some((decl, expr)) = line
            .trim()
            .strip_suffix(';')
            .and_then(|s| s.split_once(" = "))
        else {
            continue;
        };
        let Some((_, name)) = decl.rsplit_once(' ') else {
            continue;
        };
        if !generated_local(name) || !expr.starts_with("((") || !enclosed(expr) {
            continue;
        }
        let Some((ty, _)) = expr[2..].split_once(") ") else {
            continue;
        };
        if !ty
            .trim_end_matches("[]")
            .split('.')
            .all(super::super::identifier)
        {
            continue;
        }
        let begin = start + line[..line.find(" = ").unwrap() + 3].chars().count();
        edits.push(Edit {
            start: begin,
            end: begin + 1,
            text: String::new(),
            links: vec![],
        });
        let end = begin + expr.chars().count();
        edits.push(Edit {
            start: end - 1,
            end,
            text: String::new(),
            links: vec![],
        });
    }
    if !edits.is_empty() {
        apply_edits(body, edits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn clean(text: &str, names: &[&str]) -> MethodBody {
        let mut body = MethodBody {
            text: text.into(),
            links: vec![],
        };
        inline_receivers(&mut body, &names.iter().map(|s| s.to_string()).collect());
        body
    }
    #[test]
    fn negated_conditions_and_this_field_assignments_inline_once() {
        let mut body = MethodBody { text: "    boolean v0 = source.ready();\n    if (!v0) {\n        hit();\n    }\n    sample.A v1 = source.next();\n    this.field = v1;\n".into(), links: vec![] };
        readable(&mut body);
        assert!(body.text.contains("if (!source.ready())"), "{}", body.text);
        assert!(body.text.contains("this.field = source.next();"));
        assert_eq!(body.text.matches("source.next()").count(), 1);
        let original = "    sample.A v1 = source.next();\n    other.field = v1;\n";
        let mut body = MethodBody {
            text: original.into(),
            links: vec![],
        };
        readable(&mut body);
        assert_eq!(body.text, original);
    }

    #[test]
    fn guard_receiver_inlining_does_not_cross_short_circuit_effects() {
        let mut body = MethodBody { text: "    java.lang.Object v0 = source.next();\n    if (!(((sample.T) v0).ready())) {\n        hit();\n    }\n".into(), links: vec![] };
        readable(&mut body);
        assert!(!body.text.contains("v0"), "{}", body.text);
        assert_eq!(body.text.matches("source.next()").count(), 1);
        let original = "    java.lang.Object v0 = source.next();\n    if (gate() && ((sample.T) v0).ready()) {\n        hit();\n    }\n";
        let mut body = MethodBody {
            text: original.into(),
            links: vec![],
        };
        readable(&mut body);
        assert_eq!(body.text, original);
    }

    #[test]
    fn class_literals_inline_repeated_uses_and_preserve_type_links() {
        let source = "    java.lang.Class v0 = sample.Type.class;\n    sample.Type v1 = ((sample.Type) v9.get(v0));\n    check(v1, v0);\n";
        let begin = source.find("sample.Type.class").unwrap();
        let mut body = MethodBody {
            text: source.into(),
            links: vec![CodeLink {
                start: begin,
                end: begin + "sample.Type".len(),
                label: "sample.Type".into(),
            }],
        };
        readable(&mut body);
        assert!(!body.text.contains("v0"));
        assert!(
            body.text
                .contains("sample.Type v1 = (sample.Type) v9.get(sample.Type.class);")
        );
        assert!(body.text.contains("check(v1, sample.Type.class);"));
        assert_eq!(body.links.len(), 2);
        for link in body.links {
            assert_eq!(
                body.text
                    .chars()
                    .skip(link.start)
                    .take(link.end - link.start)
                    .collect::<String>(),
                "sample.Type"
            );
        }
    }
    #[test]
    fn class_resolution_is_not_moved_past_effects_or_reassignment() {
        for source in [
            "    Class v0 = sample.Type.class;\n    factory.get(effect(), v0);\n",
            "    Class v0 = sample.Type.class;\n    effect();\n    v9.get(v0);\n",
            "    Class v0 = sample.Type.class;\n    v9.get(v0);\n    v0 = other;\n",
        ] {
            let mut body = MethodBody {
                text: source.into(),
                links: vec![],
            };
            inline_class_literals(&mut body);
            assert_eq!(body.text, source);
        }
        let source = "    sample.Type v0 = ((sample.Type) value).next();\n";
        let mut body = MethodBody {
            text: source.into(),
            links: vec![],
        };
        standalone_casts(&mut body);
        assert_eq!(body.text, source);
    }

    #[test]
    fn adjacent_receiver_chain_keeps_call_order() {
        let body = clean(
            "        sample.A v0 = source.first();\n        sample.B v1 = v0.second();\n        v1.consume();\n",
            &["v0", "v1"],
        );
        assert_eq!(body.text, "        source.first().second().consume();\n");
    }
    #[test]
    fn does_not_move_across_effects_blocks_or_multiple_uses() {
        for text in [
            "        sample.A v0 = first();\n        other();\n        v0.run();\n",
            "        sample.A v0 = first();\n        v0.run();\n        v0.again();\n",
            "        sample.A v0 = first();\n        if (flag) {\n            v0.run();\n        }\n",
            "        sample.A v0 = first();\n        consume(other(), v0);\n",
            "        sample.A v0 = first();\n        this.field = v0.run();\n",
        ] {
            assert_eq!(clean(text, &["v0"]).text, text);
        }
    }
    #[test]
    fn exact_tokens_ignore_literals_and_keep_unicode_navigation() {
        let text = "        sample.A v0 = source.first(\"λ v0\");\n        v0.consume(\"v0\");\n";
        let start = text[..text.find("first").unwrap()].chars().count();
        let mut body = MethodBody {
            text: text.into(),
            links: vec![CodeLink {
                start,
                end: start + 5,
                label: "sample.Source.first".into(),
            }],
        };
        inline_receivers(&mut body, &HashSet::from(["v0".into()]));
        assert_eq!(
            body.text,
            "        source.first(\"λ v0\").consume(\"v0\");\n"
        );
        assert_eq!(body.links.len(), 1);
        let l = &body.links[0];
        assert_eq!(
            body.text
                .chars()
                .skip(l.start)
                .take(l.end - l.start)
                .collect::<String>(),
            "first"
        );
        assert_eq!(l.label, "sample.Source.first");
    }
    #[test]
    fn untracked_and_reassigned_locals_are_kept() {
        let text = "        sample.A v0 = first();\n        v0.run();\n        v0 = next();\n";
        assert_eq!(clean(text, &["v0"]).text, text);
        let text = "        sample.A v0 = new sample.A();\n        v0.run();\n";
        assert_eq!(clean(text, &[]).text, text);
    }
}

#[cfg(test)]
mod condition_tests {
    use super::*;
    #[test]
    fn adjacent_condition_inlines_but_short_circuit_and_loop_do_not() {
        for (use_line, inlined) in [
            ("if (v0) {", true),
            ("if (other && v0) {", false),
            ("while (v0) {", false),
        ] {
            let text =
                format!("        boolean v0 = source.check();\n        {use_line}\n        }}\n");
            let mut body = MethodBody {
                text: text.clone(),
                links: vec![],
            };
            inline_receivers(&mut body, &HashSet::from(["v0".into()]));
            if inlined {
                assert_eq!(body.text, "        if (source.check()) {\n        }\n");
            } else {
                assert_eq!(body.text, text);
            }
        }
    }
    #[test]
    fn parentheses_must_enclose_the_entire_expression() {
        assert!(enclosed("((sample.A) value)"));
        assert!(enclosed("(get(\")\"))"));
        assert!(!enclosed("((sample.A) left) + ((sample.A) right)"));
    }
}

#[cfg(test)]
mod argument_tests {
    use super::*;
    fn link(text: &str, visible: &str, label: &str) -> CodeLink {
        let start = text[..text.find(visible).unwrap()].chars().count();
        CodeLink {
            start,
            end: start + visible.chars().count(),
            label: label.into(),
        }
    }
    #[test]
    fn class_argument_cast_and_receiver_chain_keep_runtime_check_and_links() {
        let text = "        java.lang.Class v0 = sample.A.class;\n        java.lang.Object v1 = sample.Factory.create(v0);\n        ((sample.A) v1).run();\n";
        let mut body = MethodBody {
            text: text.into(),
            links: vec![
                link(text, "sample.A", "sample.A"),
                link(text, "sample.Factory", "sample.Factory"),
                link(
                    text,
                    "create",
                    "sample.Factory.create(Ljava/lang/Class;)Ljava/lang/Object;",
                ),
                link(text, "run", "sample.A.run()V"),
            ],
        };
        readable(&mut body);
        assert_eq!(
            body.text,
            "        ((sample.A) sample.Factory.create(sample.A.class)).run();\n"
        );
        for link in body.links {
            let shown: String = body
                .text
                .chars()
                .skip(link.start)
                .take(link.end - link.start)
                .collect();
            assert!(
                matches!(
                    shown.as_str(),
                    "sample.A" | "sample.Factory" | "create" | "run"
                ),
                "{shown}"
            );
        }
    }
    #[test]
    fn argument_inlining_requires_exact_type_and_first_evaluated_operand() {
        for (expression, signature) in [
            (
                "sample.Factory.create(v0)",
                "sample.Factory.create(Ljava/lang/Object;)V",
            ),
            (
                "sample.Factory.create(effect(), v0)",
                "sample.Factory.create(ILjava/lang/Class;)V",
            ),
            (
                "factory().create(v0)",
                "sample.Factory.create(Ljava/lang/Class;)V",
            ),
        ] {
            let text =
                format!("        java.lang.Class v0 = classSource();\n        {expression};\n");
            let mut links = vec![link(&text, "create", signature)];
            if expression.starts_with("sample.Factory") {
                links.push(link(&text, "sample.Factory", "sample.Factory"));
            }
            let mut body = MethodBody {
                text: text.clone(),
                links,
            };
            readable(&mut body);
            assert_eq!(body.text, text);
        }
    }
    #[test]
    fn only_primary_call_chains_omit_receiver_parentheses() {
        assert!(primary_call("source.first(\"(λ)\").second()"));
        assert!(!primary_call("source.first() + source.second()"));
        assert!(!primary_call("flag ? first() : second()"));
        assert!(!primary_call("source.first(\"unterminated)"));
    }
}
