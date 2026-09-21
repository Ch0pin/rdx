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
            let direct_condition = declaration.starts_with("boolean ")
                && next_text.starts_with(&format!("if ({name}) {{"));
            let receiver = if let Some((decl, rhs)) = next_text.split_once(" = ") {
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
            if !direct_condition && !receiver.starts_with(&format!("{name}.")) {
                continue;
            }
            let use_start = if direct_condition {
                next_start + indent + 4
            } else {
                next_start + next[..next.len() - receiver.len()].chars().count()
            };
            if occurrences[1].0 != use_start {
                continue;
            }
            let expr_byte = line.find(" = ").unwrap() + 3;
            let expr_start = start + line[..expr_byte].chars().count();
            let expr_end = expr_start + expression.chars().count();
            let already_primary = expression.split('.').enumerate().all(|(i, part)| {
                super::super::identifier(part) || (i == 0 && matches!(part, "this" | "super"))
            }) || enclosed(expression);
            let wrap = usize::from(!direct_condition && !already_primary);
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
    fn adjacent_receiver_chain_keeps_call_order() {
        let body = clean(
            "        sample.A v0 = source.first();\n        sample.B v1 = v0.second();\n        v1.consume();\n",
            &["v0", "v1"],
        );
        assert_eq!(
            body.text,
            "        ((source.first()).second()).consume();\n"
        );
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
            "        (source.first(\"λ v0\")).consume(\"v0\");\n"
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
