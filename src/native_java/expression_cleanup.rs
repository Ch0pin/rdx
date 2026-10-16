//! Typed expression presentation. Only total, single-use expressions cross
//! statements; calls, allocations, field/array reads and integer division stay put.
use super::condition_cleanup::{Edit, Syntax, apply, syntax};
use crate::{
    engine::DecompiledCode,
    native_dex::{DexClass, DexMethod},
    native_hierarchy::Relation,
};
use std::collections::{HashMap, HashSet};

const MAX_BYTES: usize = 512 * 1024;
const MAX_WORK: usize = 4_000_000;
const MAX_EXPRESSION: usize = 120;
const MAX_PASSES: usize = 8;

fn local(name: &str) -> bool {
    name.strip_prefix('v')
        .or_else(|| name.strip_prefix('p'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn identifier(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$') || !b.is_ascii()
}

fn occurrences<'a>(source: &'a str, parsed: &Syntax) -> HashMap<&'a str, Vec<usize>> {
    let mut result: HashMap<&str, Vec<usize>> = HashMap::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !parsed.code[i] || !identifier(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && parsed.code[i] && identifier(bytes[i]) {
            i += 1;
        }
        let name = &source[start..i];
        if local(name) {
            result.entry(name).or_default().push(start);
        }
    }
    result
}

fn type_name(ty: &str) -> bool {
    let base = ty.trim_end_matches("[]");
    numeric(base) || base == "boolean" || !base.is_empty() && base.split('.').all(super::identifier)
}

fn declaration(line: &str) -> Option<(&str, &str, &str)> {
    let (decl, rhs) = line.trim().strip_suffix(';')?.split_once(" = ")?;
    let (ty, name) = decl.split_once(' ')?;
    (type_name(ty) && local(name)).then_some((ty, name, rhs))
}

fn lines(source: &str) -> Vec<(usize, &str)> {
    let mut offset = 0;
    source
        .split_inclusive('\n')
        .map(|line| {
            let start = offset;
            offset += line.len();
            (start, line)
        })
        .collect()
}

// Emitter locals have unique names. A duplicate declaration invalidates that
// name everywhere, including disjoint catch/branch scopes.
fn types(source: &str, parsed: &Syntax, method: &DexMethod) -> HashMap<String, String> {
    let mut found = HashMap::new();
    let mut repeated = HashSet::new();
    for (index, ty) in method.parameters.iter().enumerate() {
        if let Ok(display) = super::java_type(ty) {
            found.insert(format!("p{index}"), display);
        }
    }
    for (start, line) in lines(source) {
        let indent = line.len() - line.trim_start().len();
        if indent < 8 || !parsed.code.get(start + indent).copied().unwrap_or(false) {
            continue;
        }
        let decl = declaration(line)
            .map(|(ty, name, _)| (ty, name))
            .or_else(|| {
                let (ty, name) = line.trim().strip_suffix(';')?.split_once(' ')?;
                (type_name(ty) && local(name)).then_some((ty, name))
            });
        if let Some((ty, name)) = decl
            && found.insert(name.into(), ty.into()).is_some()
        {
            repeated.insert(name.to_owned());
        }
    }
    found.retain(|name, _| !repeated.contains(name));
    found
}

fn numeric(ty: &str) -> bool {
    matches!(
        ty,
        "byte" | "short" | "char" | "int" | "long" | "float" | "double"
    )
}
fn promoted(ty: &str) -> &str {
    if matches!(ty, "byte" | "short" | "char") {
        "int"
    } else {
        ty
    }
}
fn binary_type<'a>(a: &'a str, b: &'a str) -> Option<&'a str> {
    if !numeric(a) || !numeric(b) {
        return None;
    }
    Some(if a == "double" || b == "double" {
        "double"
    } else if a == "float" || b == "float" {
        "float"
    } else if a == "long" || b == "long" {
        "long"
    } else {
        "int"
    })
}

struct Expression {
    ty: String,
    dependencies: HashSet<String>,
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
    types: &'a HashMap<String, String>,
    dependencies: HashSet<String>,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.text.as_bytes().get(self.at) == Some(&b' ') {
            self.at += 1;
        }
    }
    fn take(&mut self, token: &str) -> bool {
        self.space();
        if self.text[self.at..].starts_with(token) {
            self.at += token.len();
            true
        } else {
            false
        }
    }
    fn atom(&mut self, depth: usize) -> Option<String> {
        if depth > 24 {
            return None;
        }
        self.space();
        // Never turn ++/-- into two unary operators.
        if self.text[self.at..].starts_with("++") || self.text[self.at..].starts_with("--") {
            return None;
        }
        if self.take("-") || self.take("+") {
            let ty = self.atom(depth + 1)?;
            return numeric(&ty).then(|| promoted(&ty).to_owned());
        }
        if self.take("~") {
            let ty = self.atom(depth + 1)?;
            return matches!(ty.as_str(), "byte" | "short" | "char" | "int" | "long")
                .then(|| promoted(&ty).to_owned());
        }
        if self.take("!") {
            return (self.atom(depth + 1)? == "boolean").then(|| "boolean".into());
        }
        if self.take("(") {
            let begin = self.at;
            for ty in ["byte", "short", "char", "int", "long", "float", "double"] {
                self.at = begin;
                if self.take(ty) && self.take(")") {
                    let input = self.atom(depth + 1)?;
                    return numeric(&input).then(|| ty.into());
                }
            }
            self.at = begin;
            let ty = self.expression(0, depth + 1)?;
            return self.take(")").then_some(ty);
        }
        self.space();
        let start = self.at;
        while self
            .text
            .as_bytes()
            .get(self.at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'.'))
        {
            self.at += 1;
        }
        let token = &self.text[start..self.at];
        if let Some(ty) = self.types.get(token) {
            self.dependencies.insert(token.into());
            return Some(ty.clone());
        }
        if matches!(token, "true" | "false") {
            return Some("boolean".into());
        }
        let (number, ty) = if let Some(n) = token.strip_suffix('L') {
            (n, "long")
        } else if let Some(n) = token.strip_suffix('f') {
            (n, "float")
        } else if let Some(n) = token.strip_suffix('d') {
            (n, "double")
        } else {
            (token, "int")
        };
        if number.is_empty() || !number.as_bytes()[0].is_ascii_digit() {
            return None;
        }
        if ty == "int" || ty == "long" {
            return number
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| ty.into());
        }
        number.parse::<f64>().ok().map(|_| ty.into())
    }
    fn expression(&mut self, minimum: u8, depth: usize) -> Option<String> {
        let mut left = self.atom(depth)?;
        loop {
            self.space();
            let Some((op, precedence)) = [
                (">>>", 8),
                (">>", 8),
                ("<<", 8),
                ("<=", 7),
                (">=", 7),
                ("==", 6),
                ("!=", 6),
                ("&&", 2),
                ("||", 1),
                ("*", 10),
                ("/", 10),
                ("%", 10),
                ("+", 9),
                ("-", 9),
                ("<", 7),
                (">", 7),
                ("&", 5),
                ("^", 4),
                ("|", 3),
            ]
            .into_iter()
            .find(|(op, _)| self.text[self.at..].starts_with(op)) else {
                break;
            };
            if precedence < minimum {
                break;
            }
            self.at += op.len();
            let right = self.expression(precedence + 1, depth + 1)?;
            left = match op {
                "&&" | "||" if left == "boolean" && right == "boolean" => "boolean".into(),
                "&" | "|" | "^" if left == "boolean" && right == "boolean" => "boolean".into(),
                "==" | "!=" if left == "boolean" && right == "boolean" => "boolean".into(),
                "==" | "!=" | "<" | ">" | "<=" | ">=" => {
                    binary_type(&left, &right)?;
                    "boolean".into()
                }
                "<<" | ">>" | ">>>"
                    if matches!(promoted(&left), "int" | "long")
                        && matches!(promoted(&right), "int" | "long") =>
                {
                    promoted(&left).into()
                }
                "&" | "|" | "^"
                    if matches!(promoted(&left), "int" | "long")
                        && matches!(promoted(&right), "int" | "long") =>
                {
                    binary_type(&left, &right)?.into()
                }
                "+" | "-" | "*" => binary_type(&left, &right)?.into(),
                "/" | "%" => {
                    let ty = binary_type(&left, &right)?;
                    if !matches!(ty, "float" | "double") {
                        return None;
                    }
                    ty.into()
                }
                _ => return None,
            };
        }
        Some(left)
    }
}

fn expression(text: &str, types: &HashMap<String, String>) -> Option<Expression> {
    if text.len() > MAX_EXPRESSION || !text.is_ascii() {
        return None;
    }
    // Identity aliases preserve static types, including array/reference overloads.
    if let Some(ty) = types.get(text) {
        return Some(Expression {
            ty: ty.clone(),
            dependencies: HashSet::from([text.into()]),
        });
    }
    let mut parser = Parser {
        text,
        at: 0,
        types,
        dependencies: HashSet::new(),
    };
    let ty = parser.expression(0, 0)?;
    parser.space();
    // Keep constant temporaries: substituting one can create a Java constant
    // expression, affecting reachability, narrowing, division errors or interning.
    (parser.at == text.len() && !parser.dependencies.is_empty()).then_some(Expression {
        ty,
        dependencies: parser.dependencies,
    })
}

fn readonly_use(source: &str, position: usize, name: &str) -> bool {
    let before = source[..position].trim_end();
    let after = source[position + name.len()..].trim_start();
    !before.ends_with('.')
        && !before.ends_with("++")
        && !before.ends_with("--")
        && ![
            "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", ">>>=", "(",
        ]
        .iter()
        .any(|op| after.starts_with(op))
        && (!after.starts_with('=') || after.starts_with("=="))
}

fn copy_expressions(mut code: DecompiledCode, method: &DexMethod) -> DecompiledCode {
    for _ in 0..MAX_PASSES {
        let Some(parsed) = syntax(&code.source) else {
            break;
        };
        let types = types(&code.source, &parsed, method);
        let uses = occurrences(&code.source, &parsed);
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        let mut removed_types = Vec::new();
        let mut consumed_until = 0;
        let mut work = 0usize;
        for (start, line) in lines(&code.source) {
            if start < consumed_until {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            if indent < 8 || !parsed.code.get(start + indent).copied().unwrap_or(false) {
                continue;
            }
            let Some((ty, name, rhs)) = declaration(line) else {
                continue;
            };
            if types.get(name).is_none_or(|found| found != ty) {
                continue;
            }
            let Some(positions) = uses.get(name).filter(|p| p.len() == 2) else {
                continue;
            };
            let use_at = positions[1];
            let end = start + line.len();
            if use_at < end || use_at - end > 8192 || !readonly_use(&code.source, use_at, name) {
                continue;
            }
            let use_line_start = code.source[..use_at].rfind('\n').map_or(0, |n| n + 1);
            let use_prefix = code.source[use_line_start..use_at].trim_start();
            // Loop tests execute repeatedly; a snapshot outside the loop must
            // never be substituted into its condition or update expression.
            if ["while", "for", "do", "case", "assert"].iter().any(|word| {
                use_prefix.starts_with(word) || use_prefix.contains(&format!("{word} ("))
            }) {
                continue;
            }
            let Some(expr) =
                expression(rhs, &types).filter(|e| e.ty == ty && !e.dependencies.contains(name))
            else {
                continue;
            };
            let between = &code.source[end..use_at];
            work += between.len() + code.links.len() + code.definitions.len();
            if work > MAX_WORK {
                return code;
            }
            // Keep the rewrite inside one straight-line lexical region. Any
            // occurrence of a dependency between definition and use blocks motion
            // (including assignments and increments in earlier call arguments).
            if between
                .bytes()
                .enumerate()
                .any(|(i, b)| parsed.code[end + i] && matches!(b, b'{' | b'}'))
                || expr.dependencies.iter().any(|name| {
                    uses.get(name.as_str()).is_some_and(|p| {
                        p.get(p.partition_point(|&p| p < end))
                            .is_some_and(|&p| p < use_at)
                    })
                })
            {
                continue;
            }
            // A later argument can assign a dependency, but Java evaluates this
            // operand first. Expression motion remains safe at the exact use site.
            let prefix = code.source[use_line_start..use_at].trim();
            let suffix = code.source[use_at + name.len()..].trim_start();
            let entire_statement =
                suffix.starts_with(';') && (prefix == "return" || prefix.ends_with(" ="));
            let delimited_operand = (prefix.ends_with('(') || prefix.ends_with(','))
                && (suffix.starts_with(')') || suffix.starts_with(','));
            let replacement = if local(rhs) || entire_statement || delimited_operand {
                rhs.to_owned()
            } else {
                format!("({rhs})")
            };
            edits.push((start, end, String::new()));
            removed_types.push((start + indent, start + indent + ty.len(), ty));
            edits.push((use_at, use_at + name.len(), replacement));
            // Disjoint transactions avoid a use edit inside another deletion.
            consumed_until = code.source[use_at..]
                .find('\n')
                .map_or(code.source.len(), |n| use_at + n + 1);
            if edits.len() >= 2048 {
                break;
            }
        }
        if edits.is_empty() {
            break;
        }
        // Pure expressions carry no symbol references. If unexpected metadata
        // annotates an edited declaration/operand, retain the entire batch.
        let borrowed = edits
            .iter()
            .map(|(start, end, text)| Edit {
                start: *start,
                end: *end,
                text,
            })
            .collect();
        let mut candidate = code.clone();
        let mut scalar_at = vec![0; code.source.len() + 1];
        for (scalar, (byte, _)) in code.source.char_indices().enumerate() {
            scalar_at[byte] = scalar;
        }
        scalar_at[code.source.len()] = code.source.chars().count();
        candidate.links.retain(|link| {
            !removed_types.iter().any(|&(a, b, ty)| {
                link.label == ty && link.start == scalar_at[a] && link.end == scalar_at[b]
            })
        });
        let (updated, changed) = apply(candidate, borrowed);
        if changed {
            code = updated;
        }
        if !changed {
            break;
        }
    }
    code
}

fn descriptor(ty: &str) -> Option<String> {
    let base = ty.trim_end_matches("[]");
    if !type_name(ty) || numeric(base) || matches!(base, "boolean" | "void") {
        return None;
    }
    Some(format!(
        "{}L{};",
        "[".repeat((ty.len() - base.len()) / 2),
        base.replace('.', "/")
    ))
}

fn casts(code: DecompiledCode, class: &DexClass, method: &DexMethod) -> DecompiledCode {
    let Some(parsed) = syntax(&code.source) else {
        return code;
    };
    let types = types(&code.source, &parsed, method);
    let mut edits = Vec::new();
    let mut removed = Vec::new();
    let mut work = 0;
    for (line_start, line) in lines(&code.source) {
        for (relative, _) in line
            .as_bytes()
            .windows(2)
            .enumerate()
            .filter(|(_, bytes)| *bytes == b"((")
        {
            let begin = line_start + relative;
            if !parsed.code[begin] {
                continue;
            }
            let inner = begin + 1;
            let close_type = parsed.pair[inner];
            let close = parsed.pair[begin];
            if close_type == usize::MAX || close == usize::MAX || close_type >= close {
                continue;
            }
            let ty = code.source[inner + 1..close_type].trim();
            let operand = code.source[close_type + 1..close].trim();
            let Some(input) = types.get(operand) else {
                continue;
            };
            let identity = ty == input;
            let whole_rhs = {
                let prefix = code.source[line_start..begin].trim();
                let suffix = code.source[close + 1..]
                    .split('\n')
                    .next()
                    .unwrap_or("")
                    .trim();
                let assignment_type = prefix
                    .strip_suffix(" =")
                    .and_then(|decl| decl.split_once(' '))
                    .filter(|(ty, name)| type_name(ty) && local(name))
                    .map(|(ty, _)| ty.to_owned());
                let destination = if prefix == "return" {
                    super::java_type(&method.return_type).ok()
                } else {
                    assignment_type
                };
                suffix == ";" && destination.as_deref() == Some(ty)
            };
            let widening = whole_rhs
                && descriptor(input)
                    .zip(descriptor(ty))
                    .is_some_and(|(source, target)| {
                        class
                            .symbols
                            .hierarchy
                            .get()
                            .is_some_and(|h| h.assignable(&source, &target) == Relation::Proven)
                    });
            if !identity && !widening {
                continue;
            }
            // Primitive identity casts preserve the exact compile-time type;
            // reference widening is allowed only in an identical assignment context.
            let operand_at =
                close_type + 1 + code.source[close_type + 1..close].find(operand).unwrap();
            work += code.links.len() + code.definitions.len();
            if work > MAX_WORK || edits.len() >= 2048 {
                return code;
            }
            removed.push((inner + 1, close_type, ty));
            edits.push(Edit {
                start: begin,
                end: operand_at,
                text: "",
            });
            edits.push(Edit {
                start: operand_at + operand.len(),
                end: close + 1,
                text: "",
            });
        }
    }
    if edits.is_empty() {
        return code;
    }
    let mut scalar_at = vec![0; code.source.len() + 1];
    for (scalar, (byte, _)) in code.source.char_indices().enumerate() {
        scalar_at[byte] = scalar;
    }
    scalar_at[code.source.len()] = code.source.chars().count();
    let mut candidate = code.clone();
    // The removed cast type has no runtime check under the proof. Its exact
    // type link alone can disappear; all other source navigation must survive.
    candidate.links.retain(|link| {
        !removed.iter().any(|&(a, b, ty)| {
            link.label == ty && link.start == scalar_at[a] && link.end == scalar_at[b]
        })
    });
    let (updated, changed) = apply(candidate, edits);
    if changed { updated } else { code }
}

pub(super) fn simplify(
    code: DecompiledCode,
    class: &DexClass,
    method: &DexMethod,
) -> DecompiledCode {
    if code.source.len() > MAX_BYTES
        || method.code.is_none()
        || code.source.contains("/*")
        || code.source.contains("\"\"\"")
    {
        return code;
    }
    let Some(parsed) = syntax(&code.source) else {
        return code;
    };
    if !super::statement_cleanup::ordinary_method_scopes(&code.source, &parsed) {
        return code;
    }
    casts(copy_expressions(code, method), class, method)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{CodeLink, source_identity},
        native_dex,
        native_hierarchy::TypeHierarchy,
    };
    use std::{fs, process::Command, sync::Arc};

    fn fixture(params: &[&str], ret: &str) -> (DexClass, DexMethod) {
        let mut class = native_dex::parse(include_bytes!("../../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        let index = class
            .methods
            .iter()
            .position(|m| m.name.as_ref() == "answer")
            .unwrap();
        let mut method = class.methods.remove(index);
        method.name = "test".into();
        method.parameters = params.iter().map(|p| Arc::from(*p)).collect();
        method.return_type = ret.into();
        (class, method)
    }
    fn code(source: &str) -> DecompiledCode {
        DecompiledCode {
            source: source.into(),
            links: vec![],
            definitions: vec![],
            source_hash: source_identity(source),
        }
    }
    fn rewrite(source: &str, params: &[&str], ret: &str) -> String {
        let (class, method) = fixture(params, ret);
        simplify(code(source), &class, &method).source
    }
    #[test]
    fn pure_single_use_expressions_cross_effects_without_reordering_them() {
        let source = "    static int test(int p0, int p1) {\n        int v0 = p0 + p1;\n        Effects.tick();\n        int v1 = v0 * 3;\n        return v1;\n    }\n";
        let after = rewrite(source, &["I", "I"], "I");
        assert_eq!(
            after,
            "    static int test(int p0, int p1) {\n        Effects.tick();\n        return (p0 + p1) * 3;\n    }\n"
        );
        assert_eq!(rewrite(&after, &["I", "I"], "I"), after);
    }
    #[test]
    fn snapshot_dependencies_throwing_operations_and_repeated_evaluation_are_retained() {
        for body in [
            "        int v0 = p0 + 1;\n        p0 = 9;\n        return v0;\n",
            "        int v0 = p0 + 1;\n        return helper(p0++, v0);\n",
            "        int v0 = p0 / p1;\n        Effects.tick();\n        return v0;\n",
            "        int v0 = p0 % p1;\n        return v0;\n",
            "        int v0 = Effects.next();\n        Effects.tick();\n        return v0;\n",
            "        int v0 = object.field;\n        Effects.tick();\n        return v0;\n",
            "        int v0 = values[p0];\n        Effects.tick();\n        return v0;\n",
            "        boolean v0 = p0 < p1;\n        while (v0) {\n            p0++;\n        }\n        return p0;\n",
            "        boolean v0 = p0 < p1;\n        loop0: while (v0) {\n            p0++;\n        }\n        return p0;\n",
            "        int v0 = 0;\n        return p0 / v0;\n",
            "        long v0 = p0;\n        return overload(v0);\n",
            "        int v0 = p0 + 1;\n        if (p1 > 0) {\n            return v0;\n        }\n        return 0;\n",
            "        int v0 = p0 + 1;\n        v0++;\n        return p1;\n",
            "        int v0 = p0 + 1;\n        return v0 + v0;\n",
        ] {
            let source = format!("    static int test(int p0, int p1) {{\n{body}    }}\n");
            assert_eq!(rewrite(&source, &["I", "I"], "I"), source, "{body}");
        }
    }
    #[test]
    fn parser_preserves_java_promotion_and_rejects_non_total_or_ambiguous_syntax() {
        let types = HashMap::from([
            ("p0".into(), "int".into()),
            ("p1".into(), "long".into()),
            ("p2".into(), "float".into()),
            ("p3".into(), "boolean".into()),
            ("p4".into(), "byte".into()),
        ]);
        for (source, ty) in [
            ("p0 + p1", "long"),
            ("p0 << p1", "int"),
            ("p1 >>> p0", "long"),
            ("p2 / p0", "float"),
            ("(byte) p0", "byte"),
            ("-p4", "int"),
            ("p3 && !p3", "boolean"),
            ("p0 < p1", "boolean"),
            ("(double) (p0 + p1)", "double"),
        ] {
            assert_eq!(expression(source, &types).unwrap().ty, ty, "{source}");
        }
        for source in [
            "p0 / p1",
            "p0 % p1",
            "p0++",
            "++p0",
            "p0 = 1",
            "p0.field",
            "p0()",
            "new Thing(p0)",
            "p0[1]",
            "1 + 2",
            "\"x\" + p0",
            "p3 ? p0 : p1",
            "p0 /*x*/ + 1",
        ] {
            assert!(expression(source, &types).is_none(), "{source}");
        }
    }
    #[test]
    fn exact_casts_preserve_overloads_and_widening_requires_assignment_context() {
        let source = "    static java.lang.Object test(java.lang.String p0) {\n        take(((java.lang.String) p0));\n        take(((java.lang.Object) p0));\n        return ((java.lang.Object) p0);\n    }\n";
        let (class, method) = fixture(&["Ljava/lang/String;"], "Ljava/lang/Object;");
        class
            .symbols
            .hierarchy
            .set(Arc::new(TypeHierarchy::from_classes([]).unwrap()))
            .unwrap();
        let after = simplify(code(source), &class, &method);
        assert!(after.source.contains("take(p0);"), "{}", after.source);
        assert!(after.source.contains("take(((java.lang.Object) p0));"));
        assert!(after.source.contains("return p0;"));
        let down = "    static java.lang.String test(java.lang.Object p0) {\n        return ((java.lang.String) p0);\n    }\n";
        assert_eq!(
            rewrite(down, &["Ljava/lang/Object;"], "Ljava/lang/String;"),
            down
        );
        let narrow = "    static int test(long p0) {\n        return ((int) p0);\n    }\n";
        assert_eq!(rewrite(narrow, &["J"], "I"), narrow);
    }
    #[test]
    fn unicode_navigation_survives_and_opaque_metadata_refuses_atomically() {
        let source = "    static int test(int p0) {\n        // λ é\n        int v0 = p0 * 2;\n        Effects.tick();\n        return v0;\n    }\n";
        let (class, method) = fixture(&["I"], "I");
        let mut original = code(source);
        let start = source[..source.find("tick").unwrap()].chars().count();
        original.links.push(CodeLink {
            start,
            end: start + 4,
            label: "sample.Effects.tick()V".into(),
        });
        let after = simplify(original.clone(), &class, &method);
        assert!(!after.source.contains("v0"));
        assert_eq!(
            after
                .source
                .chars()
                .skip(after.links[0].start)
                .take(4)
                .collect::<String>(),
            "tick"
        );
        assert_eq!(after.source_hash, source_identity(&after.source));
        let start = source[..source.find("v0").unwrap()].chars().count();
        original.links.push(CodeLink {
            start,
            end: start + 2,
            label: "opaque".into(),
        });
        assert_eq!(simplify(original, &class, &method).source, source);
    }
    #[test]
    fn nested_scopes_redeclarations_and_captures_are_conservative() {
        for body in [
            "        int v0 = p0;\n        Runnable v1 = () -> use(v0);\n        return v1;\n",
            "        int v0 = p0;\n        Object v1 = new Object() { int get() { return v0; } };\n        return v1;\n",
            "        int v0 = p0;\n        if (true) {\n            int v0 = 1;\n        }\n        return v0;\n",
        ] {
            let source = format!("    static Object test(int p0) {{\n{body}    }}\n");
            assert_eq!(rewrite(&source, &["I"], "Ljava/lang/Object;"), source);
        }
    }
    #[test]
    #[ignore = "requires javac and java on PATH"]
    fn jvm_expression_cleanup_preserves_results_effects_exceptions_and_overloads() {
        let source = concat!(
            "    static long test(int p0, int p1, double p2, double p3, String p4) {\n",
            "        int v0 = p0 * 31;\n",
            "        tick();\n",
            "        int v1 = v0 + p1;\n",
            "        int v2 = p0 / p1;\n",
            "        double v3 = p2 / p3;\n",
            "        double v4 = v3 + p2;\n",
            "        int v5 = choose(((java.lang.String) p4));\n",
            "        int v6 = choose(((java.lang.Object) p4));\n",
            "        long v7 = (long) v1;\n",
            "        long v8 = v7 ^ Double.doubleToRawLongBits(v4);\n",
            "        long v9 = v8 + v2;\n",
            "        return v9 + v5 + v6;\n",
            "    }\n"
        );
        let after = rewrite(source, &["I", "I", "D", "D", "Ljava/lang/String;"], "J");
        // Fully qualified source types are what the renderer supplies.
        assert!(after.len() < source.len(), "{after}");
        assert!(after.contains("choose(p4)"), "{after}");
        assert!(after.contains("choose(((java.lang.Object) p4))"), "{after}");
        let dir =
            std::env::temp_dir().join(format!("rdx-expression-cleanup-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let helpers = "static String trace; static boolean fail; static RuntimeException sentinel=new RuntimeException(); static void tick(){ trace+=\"T\";if(fail)throw sentinel; } static int choose(String s){trace+=\"S\";return 1;} static int choose(Object o){trace+=\"O\";return 2;}";
        for (name, method) in [("Before", source), ("After", after.as_str())] {
            fs::write(
                dir.join(format!("{name}.java")),
                format!("class {name} {{{helpers}{method}}}"),
            )
            .unwrap();
        }
        let driver = r#"public class Check {
static String before(int a,int b,double c,double d,String s,boolean fail){ Before.trace="";Before.fail=fail;try{return Before.test(a,b,c,d,s)+":"+Before.trace;}catch(RuntimeException e){if(fail&&e!=Before.sentinel)throw new AssertionError();return e.getClass().getName()+":"+Before.trace;}}
static String after(int a,int b,double c,double d,String s,boolean fail){ After.trace="";After.fail=fail;try{return After.test(a,b,c,d,s)+":"+After.trace;}catch(RuntimeException e){if(fail&&e!=After.sentinel)throw new AssertionError();return e.getClass().getName()+":"+After.trace;}}
public static void main(String[] args){int count=0;for(int a:new int[]{Integer.MIN_VALUE,-1,0,1,Integer.MAX_VALUE})for(int b:new int[]{-1,0,1,31})for(double c:new double[]{Double.NaN,Double.NEGATIVE_INFINITY,-0.0,0.0,1.0,Double.POSITIVE_INFINITY})for(double d:new double[]{-0.0,0.0,1.0,Double.NaN})for(String s:new String[]{null,"é"})for(boolean fail:new boolean[]{false,true}){String x=before(a,b,c,d,s,fail),y=after(a,b,c,d,s,fail);if(!x.equals(y))throw new AssertionError(x+" != "+y);count++;}System.out.print(count);}}
"#;
        fs::write(dir.join("Check.java"), driver).unwrap();
        for (program, args) in [
            ("javac", vec!["Before.java", "After.java", "Check.java"]),
            ("java", vec!["-cp", ".", "Check"]),
        ] {
            let result = Command::new(program)
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            if program == "java" {
                assert_eq!(result.stdout, b"1920");
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
