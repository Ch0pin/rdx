//! Bounded statement presentation: adjacent returns and loop-entry guards.
//! No expression is moved across another executable statement.
use super::condition_cleanup::{Edit, Syntax, apply, syntax};
use crate::{engine::DecompiledCode, native_dex::DexMethod};
use std::collections::HashMap;

const MAX_BYTES: usize = 512 * 1024;
const MAX_EDITS: usize = 4096;
const MAX_WORK: usize = 2_000_000;
const PRIMITIVES: [&str; 8] = [
    "boolean", "byte", "short", "char", "int", "long", "float", "double",
];

fn local(name: &str) -> bool {
    name.strip_prefix('v')
        .or_else(|| name.strip_prefix('p'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
}

fn counts<'a>(source: &'a str, parsed: &Syntax) -> HashMap<&'a str, usize> {
    let bytes = source.as_bytes();
    let mut result = HashMap::new();
    let mut i = 0;
    while i < bytes.len() {
        if !parsed.code[i] || !identifier_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && parsed.code[i] && identifier_byte(bytes[i]) {
            i += 1;
        }
        if local(&source[start..i]) {
            *result.entry(&source[start..i]).or_insert(0) += 1;
        }
    }
    result
}

fn declaration(line: &str) -> Option<(&str, &str, &str)> {
    let (left, expression) = line.trim().strip_suffix(';')?.split_once(" = ")?;
    let (ty, name) = left.split_once(' ')?;
    (local(name)
        && ty.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
        }))
    .then_some((ty, name, expression))
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

fn scalar_at(source: &str, bytes: &[usize]) -> Vec<usize> {
    let mut result = Vec::with_capacity(bytes.len());
    let (mut byte, mut scalar) = (0, 0);
    for &end in bytes {
        scalar += source[byte..end].chars().count();
        result.push(scalar);
        byte = end;
    }
    result
}

// Local names inside anonymous classes/lambdas belong to different scopes.
// This pass accepts ordinary method/control blocks only, before class layout.
pub(super) fn ordinary_method_scopes(source: &str, parsed: &Syntax) -> bool {
    if source.contains("->") {
        return false;
    }
    let mut root = false;
    for (start, line) in lines(source) {
        for (at, byte) in line.bytes().enumerate() {
            if byte != b'{' || !parsed.code[start + at] {
                continue;
            }
            if !root {
                root = true;
                continue;
            }
            let header = line[..at].trim().trim_start_matches('}').trim_start();
            if ![
                "if (",
                "else",
                "while (",
                "for (",
                "switch (",
                "synchronized (",
                "try",
                "catch (",
                "finally",
                "do",
            ]
            .iter()
            .any(|prefix| header.starts_with(prefix))
            {
                return false;
            }
        }
    }
    root
}

fn adjacent_returns(code: DecompiledCode, method: &DexMethod) -> DecompiledCode {
    let Ok(return_type) = super::java_type(&method.return_type) else {
        return code;
    };
    if return_type == "void" {
        return code;
    }
    let Some(parsed) = syntax(&code.source) else {
        return code;
    };
    let uses = counts(&code.source, &parsed);
    let mut edits = Vec::new();
    let mut removed_types = Vec::new();
    let mut work = 0usize;
    for pair in lines(&code.source).windows(2) {
        let (start, line) = pair[0];
        let (next_start, next) = pair[1];
        if line.len() > 32_768 {
            continue;
        }
        let Some((ty, name, expression)) = declaration(line) else {
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        if indent != 8
            || uses.get(name) != Some(&2)
            || next.len() - next.trim_start().len() != indent
            || next.trim() != format!("return {name};")
            || !parsed.code[start + indent]
        {
            continue;
        }
        let expression_start = start + line.find(" = ").unwrap() + 3;
        let expression_end = expression_start + expression.len();
        // Keep nested declarations, switch expressions and captures out of this
        // source-only rule. Primitive assignment and return contexts are equal.
        if code.source.as_bytes()[expression_start..expression_end]
            .iter()
            .enumerate()
            .any(|(i, b)| parsed.code[expression_start + i] && matches!(b, b'{' | b'}' | b';'))
            || expression.contains("->")
        {
            continue;
        }
        // Assignment and return provide the same target type. This also covers
        // reference-returning calls (including generic factories) without
        // changing inference or moving the call across another statement.
        let same_context = ty == return_type;
        let constructor = expression
            .strip_prefix("new ")
            .and_then(|s| s.strip_prefix(ty))
            .is_some_and(|s| {
                s.starts_with('(')
                    && s.ends_with(')')
                    && parsed.pair[expression_start + 4 + ty.len()] == expression_end - 1
            });
        // An exact, non-generic `new T(...)` has no target-dependent overload
        // inference. Preserve all argument casts and the original allocation site.
        if !(same_context || constructor && return_type == "java.lang.Object") {
            continue;
        }
        work = work.saturating_add(code.links.len() + code.definitions.len());
        if edits.len() + 2 > MAX_EDITS || work > MAX_WORK {
            return code;
        }
        removed_types.push((start + indent, start + indent + ty.len(), ty));
        edits.push(Edit {
            start: start + indent,
            end: expression_start,
            text: "return ",
        });
        edits.push(Edit {
            start: next_start,
            end: next_start + next.len(),
            text: "",
        });
    }
    if edits.is_empty() {
        return code;
    }
    // Only the exact redundant declaration-type link may disappear. Constructor
    // and argument links stay attached to their original expression tokens.
    let boundaries: Vec<_> = removed_types.iter().flat_map(|&(a, b, _)| [a, b]).collect();
    let scalars = scalar_at(&code.source, &boundaries);
    let mut candidate = code.clone();
    candidate.links.retain(|link| {
        !removed_types.iter().enumerate().any(|(i, &(_, _, ty))| {
            link.start == scalars[2 * i] && link.end == scalars[2 * i + 1] && link.label == ty
        })
    });
    let (updated, changed) = apply(candidate, edits);
    if changed { updated } else { code }
}

fn local_types<'a>(source: &'a str, parsed: &Syntax) -> HashMap<&'a str, Option<&'a str>> {
    let mut result = HashMap::new();
    for (start, line) in lines(source) {
        let declared = declaration(line)
            .map(|(ty, name, _)| (ty, name))
            .or_else(|| {
                let (ty, name) = line.trim().strip_suffix(';')?.split_once(' ')?;
                local(name).then_some((ty, name))
            });
        if let Some((ty, name)) = declared
            && (PRIMITIVES.contains(&ty) || ty.contains('.'))
            && parsed.code[start + line.len() - line.trim_start().len()]
        {
            let ty = (line.len() - line.trim_start().len() == 8).then_some(ty);
            result.entry(name).and_modify(|ty| *ty = None).or_insert(ty);
        }
    }
    result
}

fn loop_guards(code: DecompiledCode) -> DecompiledCode {
    let Some(parsed) = syntax(&code.source) else {
        return code;
    };
    let types = local_types(&code.source, &parsed);
    let integer = |atom: &str| {
        atom.strip_suffix('L')
            .unwrap_or(atom)
            .parse::<i64>()
            .is_ok()
            || types
                .get(atom)
                .is_some_and(|ty| matches!(ty, Some("byte" | "short" | "char" | "int" | "long")))
    };
    let mut edits = Vec::new();
    for group in lines(&code.source).windows(4) {
        let (start, first) = group[0];
        let (_, guard) = group[1];
        let (_, exit) = group[2];
        let (end_start, close) = group[3];
        let indent = first.len() - first.trim_start().len();
        if first.trim() != "while (true) {"
            || !parsed.code[start + indent]
            || guard.len() - guard.trim_start().len() != indent + 4
            || exit.len() - exit.trim_start().len() != indent + 8
            || close.len() - close.trim_start().len() != indent + 4
            || exit.trim() != "break;"
            || close.trim() != "}"
        {
            continue;
        }
        let Some(condition) = guard
            .trim()
            .strip_prefix("if (")
            .and_then(|s| s.strip_suffix(") {"))
        else {
            continue;
        };
        let parts: Vec<_> = condition.split(' ').collect();
        if parts.len() != 3
            || !integer(parts[0])
            || !integer(parts[2])
            || !(local(parts[0]) || local(parts[2]))
        {
            continue;
        }
        let inverted = match parts[1] {
            ">=" => "<",
            ">" => "<=",
            "<=" => ">",
            "<" => ">=",
            "==" => "!=",
            "!=" => "==",
            _ => continue,
        };
        let condition_start = group[1].0 + indent + 4 + "if (".len();
        let operator = condition_start + parts[0].len() + 1;
        let condition_end = condition_start + condition.len();
        if edits.len() + 3 > MAX_EDITS {
            return code;
        }
        edits.push(Edit {
            start: start + indent,
            end: condition_start,
            text: "while (",
        });
        edits.push(Edit {
            start: operator,
            end: operator + parts[1].len(),
            text: inverted,
        });
        edits.push(Edit {
            start: condition_end,
            end: end_start + close.len(),
            text: ") {\n",
        });
    }
    apply(code, edits).0
}

// A cast to a local's own declared reference type cannot affect overloads or
// introduce a new check. Restrict this to return operands, never call arguments.
fn identical_return_casts(code: DecompiledCode) -> DecompiledCode {
    let Some(parsed) = syntax(&code.source) else {
        return code;
    };
    let types = local_types(&code.source, &parsed);
    let mut edits = Vec::new();
    for (start, line) in lines(&code.source) {
        let indent = line.len() - line.trim_start().len();
        let Some((ty, tail)) = line
            .trim()
            .strip_prefix("return ((")
            .and_then(|s| s.split_once(") "))
        else {
            continue;
        };
        let Some(name) = tail.strip_suffix(");") else {
            continue;
        };
        if !local(name)
            || PRIMITIVES.contains(&ty)
            || types.get(name) != Some(&Some(ty))
            || !parsed.code[start + indent]
        {
            continue;
        }
        if edits.len() + 2 > MAX_EDITS {
            return code;
        }
        let operand = start + indent + "return ".len();
        let close = operand + "((".len() + ty.len() + ") ".len() + name.len();
        edits.push(Edit {
            start: operand,
            end: close - name.len(),
            text: "",
        });
        edits.push(Edit {
            start: close,
            end: close + 1,
            text: "",
        });
    }
    apply(code, edits).0
}

pub(super) fn simplify(code: DecompiledCode, method: &DexMethod) -> DecompiledCode {
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
    if !ordinary_method_scopes(&code.source, &parsed) {
        return code;
    }
    loop_guards(adjacent_returns(identical_return_casts(code), method))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{CodeDefinition, CodeLink},
        native_dex::DexCode,
    };

    fn method(ret: &str) -> DexMethod {
        DexMethod {
            declaring_type: "Lsample/Example;".into(),
            name: "run".into(),
            return_type: ret.into(),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: 9,
            code: Some(DexCode {
                registers: 0,
                ins: 0,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x000e],
                offset: 0,
            }),
        }
    }
    fn code(source: &str) -> DecompiledCode {
        DecompiledCode {
            source: source.into(),
            source_hash: crate::engine::source_identity(source),
            links: vec![],
            definitions: vec![],
        }
    }

    #[test]
    fn adjacent_reference_calls_keep_the_same_target_type() {
        let source = "    public java.lang.String run() {\n        java.lang.String v0 = Factory.make();\n        return v0;\n    }\n";
        let after = simplify(code(source), &method("Ljava/lang/String;"));
        assert_eq!(
            after.source,
            "    public java.lang.String run() {\n        return Factory.make();\n    }\n"
        );
        // A wider return target can change generic method inference; retain the
        // original String assignment context in that case.
        let wider = source.replace("public java.lang.String run", "public java.lang.Object run");
        assert_eq!(
            simplify(code(&wider), &method("Ljava/lang/Object;")).source,
            wider
        );
    }
    fn source(body: &str) -> String {
        format!("    public static java.lang.Object run() {{\n{body}    }}\n")
    }

    #[test]
    fn constructor_return_keeps_expression_order_and_navigation() {
        let before = source(
            "        // λ😀\n        sample.Response v0 = new sample.Response(Effects.first(), Effects.second());\n        return v0;\n",
        );
        let mut original = code(&before);
        for (needle, label) in [
            ("sample.Response v0", "sample.Response"),
            ("sample.Response(", "sample.Response.<init>(II)V"),
            ("first", "Effects.first()I"),
            ("second", "Effects.second()I"),
        ] {
            let start = before[..before.find(needle).unwrap()].chars().count();
            original.links.push(CodeLink {
                start,
                end: start
                    + if needle.starts_with("sample") {
                        15
                    } else {
                        needle.len()
                    },
                label: label.into(),
            });
        }
        let start = before[..before.find("run").unwrap()].chars().count();
        original.definitions.push(CodeDefinition {
            start,
            end: start + 3,
            kind: "method".into(),
            name: "run".into(),
        });
        let after = simplify(original.clone(), &method("Ljava/lang/Object;"));
        assert!(
            after
                .source
                .contains("return new sample.Response(Effects.first(), Effects.second());"),
            "{}",
            after.source
        );
        assert_eq!(after.links.len(), 3);
        for (old, new) in original.links[1..].iter().zip(&after.links) {
            assert_eq!(old.label, new.label);
            assert_eq!(
                before
                    .chars()
                    .skip(old.start)
                    .take(old.end - old.start)
                    .collect::<String>(),
                after
                    .source
                    .chars()
                    .skip(new.start)
                    .take(new.end - new.start)
                    .collect::<String>()
            );
        }
        assert_eq!(
            after.source_hash,
            crate::engine::source_identity(&after.source)
        );
        assert_eq!(
            simplify(after.clone(), &method("Ljava/lang/Object;")).source,
            after.source
        );
    }

    #[test]
    fn primitive_return_preserves_assignment_conversion_context() {
        for (ty, descriptor, expression) in [
            ("int", "I", "Effects.read() / 2"),
            ("float", "F", "p0 * p1"),
            ("byte", "B", "1"),
            ("boolean", "Z", "Effects.check()"),
            ("long", "J", "p0 + p1"),
        ] {
            let before = source(&format!(
                "        {ty} v0 = {expression};\n        return v0;\n"
            ));
            let after = simplify(code(&before), &method(descriptor));
            assert!(
                after.source.contains(&format!("return {expression};")),
                "{}",
                after.source
            );
        }
        let before = source("        float v0 = p0;\n        return v0;\n");
        assert_eq!(simplify(code(&before), &method("D")).source, before);
    }

    #[test]
    fn return_refusals_keep_captures_conversions_and_statement_boundaries() {
        for body in [
            "        sample.Response v0 = new sample.Response(v0);\n        return v0;\n",
            "        sample.Response v0 = new sample.Response();\n        Effects.touch();\n        return v0;\n",
            "        sample.Response v0 = new sample.Response();\n        return v0; // note\n",
            "        sample.Response v0 = new sample.Response();\n        return v0;\n        // Finally may read a captured local\n        consume(v0);\n",
            "        sample.Response v0 = new sample.Response(() -> v0);\n        return v0;\n",
            "        sample.Response v0 = new sample.Response() {};\n        return v0;\n",
            "        sample.Response v0 = factory();\n        return v0;\n",
            "        sample.Response v0 = new sample.Response<String>();\n        return v0;\n",
            "        /* comment\n        sample.Response v0 = new sample.Response();\n        return v0;\n        */\n",
            "            sample.Response v0 = new sample.Response();\n            return v0;\n",
        ] {
            let before = source(body);
            assert_eq!(
                simplify(code(&before), &method("Ljava/lang/Object;")).source,
                before,
                "{body}"
            );
        }
    }

    #[test]
    fn opaque_metadata_refuses_return_edits_atomically() {
        let before =
            source("        sample.Response v0 = new sample.Response();\n        return v0;\n");
        let mut original = code(&before);
        let start = before.find("v0").unwrap();
        original.links.push(CodeLink {
            start,
            end: start + 2,
            label: "opaque".into(),
        });
        let after = simplify(original.clone(), &method("Ljava/lang/Object;"));
        assert_eq!(after.source, original.source);
        assert_eq!(after.links.len(), original.links.len());
        assert_eq!(after.links[0].label, original.links[0].label);
        assert_eq!(
            (after.links[0].start, after.links[0].end),
            (original.links[0].start, original.links[0].end)
        );
    }

    #[test]
    fn return_casts_require_the_exact_declared_reference_type() {
        let before = source(
            "        sample.Response v0 = factory();\n        return ((sample.Response) v0);\n",
        );
        let after = simplify(code(&before), &method("Ljava/lang/Object;"));
        assert!(after.source.contains("return v0;"));
        for refused in [
            before.replace("sample.Response v0", "java.lang.Object v0"),
            before
                .replace("return ((", "consume(((")
                .replace("v0);", "v0));"),
            before.replace(
                "        return",
                "        java.lang.Object v0;\n        return",
            ),
        ] {
            assert_eq!(
                simplify(code(&refused), &method("Ljava/lang/Object;")).source,
                refused
            );
        }
    }

    fn loop_source(ty: &str, comparison: &str, body: &str) -> String {
        source(&format!(
            "        {ty} v0 = 0;\n        {ty} v1 = 3;\n        while (true) {{\n            if (v0 {comparison} v1) {{\n                break;\n            }}\n{body}        }}\n        return null;\n"
        ))
    }

    #[test]
    fn immediate_integer_loop_guard_becomes_loop_condition() {
        for (comparison, inverse) in [
            (">=", "<"),
            (">", "<="),
            ("<=", ">"),
            ("<", ">="),
            ("==", "!="),
            ("!=", "=="),
        ] {
            let before = loop_source(
                "int",
                comparison,
                "            v0++;\n            continue;\n",
            );
            let after = simplify(code(&before), &method("Ljava/lang/Object;"));
            assert!(
                after
                    .source
                    .contains(&format!("while (v0 {inverse} v1) {{")),
                "{}",
                after.source
            );
            assert!(!after.source.contains("break;"));
            assert!(after.source.contains("continue;"));
            assert_eq!(
                simplify(after.clone(), &method("Ljava/lang/Object;")).source,
                after.source
            );
        }
    }

    #[test]
    fn loop_refusals_keep_nan_side_effects_labels_and_redeclarations() {
        let before = loop_source("double", ">=", "            v0++;\n");
        assert_eq!(
            simplify(code(&before), &method("Ljava/lang/Object;")).source,
            before
        );
        let basic = loop_source("int", ">=", "            v0++;\n");
        for before in [
            basic.replace("v0 >= v1", "v0++ >= v1"),
            basic.replace("break;", "break outer;"),
            basic.replace("            if (", "            touch();\n            if ("),
            basic.replace("            if (", "            // guard\n            if ("),
            basic.replace("        while", "        double v0;\n        while"),
            basic.replace("v0 >= v1", "v0 >= Effects.limit()"),
        ] {
            assert_eq!(
                simplify(code(&before), &method("Ljava/lang/Object;")).source,
                before
            );
        }
    }

    #[test]
    fn different_lexical_scopes_do_not_supply_type_proofs() {
        for before in [
            source(
                "        if (p0) {\n            sample.Response v0 = null;\n        }\n        try { work(); } catch (java.lang.Object v0) {\n            return ((sample.Response) v0);\n        }\n",
            ),
            source(
                "        sample.Response v0 = null;\n        Object v1 = new Object() {\n            Object v0;\n            Object read() {\n                return ((sample.Response) v0);\n            }\n        };\n        return v1;\n",
            ),
            source(
                "        double v0 = 0;\n        int v1 = 2;\n        java.util.function.Supplier v2 = () -> { return v0; };\n        return null;\n",
            ),
        ] {
            assert_eq!(
                simplify(code(&before), &method("Ljava/lang/Object;")).source,
                before
            );
        }
    }

    #[test]
    #[ignore = "requires javac and java on PATH"]
    fn statement_rewrites_match_jvm_effects_and_boundaries() {
        use std::{fs, process::Command};
        let fixtures = [
            (
                "Ljava/lang/String;",
                concat!(
                    "    public static java.lang.String generic(int p0) {\n",
                    "        java.lang.String v0 = Effects.make(p0);\n",
                    "        return v0;\n    }\n"
                ),
            ),
            (
                "Ljava/lang/Object;",
                concat!(
                    "    public static Object create(int p0) {\n",
                    "        sample.Response v0 = new sample.Response(Effects.first(p0), ((Object) Effects.second(p0)));\n",
                    "        return v0;\n    }\n"
                ),
            ),
            (
                "Lsample/Response;",
                concat!(
                    "    public static sample.Response cast(int p0) {\n",
                    "        sample.Response v0 = Effects.existing(p0);\n",
                    "        return ((sample.Response) v0);\n    }\n"
                ),
            ),
            (
                "I",
                concat!(
                    "    public static int division(int p0, int p1) {\n",
                    "        int v0 = p0 / p1;\n        return v0;\n    }\n"
                ),
            ),
            (
                "I",
                concat!(
                    "    public static int loop(int p0) {\n",
                    "        int v0 = 0;\n        int v1 = p0;\n        int v2 = 0;\n",
                    "        while (true) {\n            if (v0 >= v1) {\n                break;\n            }\n",
                    "            try {\n                v0++;\n                if (v0 == 2) { continue; }\n",
                    "                if (v0 == 4) { break; }\n                v2 += v0;\n",
                    "            } finally { Effects.log.append(v0); }\n",
                    "        }\n        return v2;\n    }\n"
                ),
            ),
        ];
        let mut before = String::new();
        let mut after = String::new();
        for (ret, source) in fixtures {
            before.push_str(source);
            let updated = simplify(code(source), &method(ret));
            assert_ne!(updated.source, source, "fixture was not transformed");
            after.push_str(&updated.source);
        }
        let directory =
            std::env::temp_dir().join(format!("rdx-statement-readability-{}", std::process::id()));
        fs::create_dir_all(directory.join("sample")).unwrap();
        for (name, body) in [("Before", before), ("After", after)] {
            fs::write(
                directory.join(format!("sample/{name}.java")),
                format!("package sample; public class {name} {{\n{body}}}"),
            )
            .unwrap();
        }
        let support = r#"package sample;
class Effects {
 static StringBuilder log = new StringBuilder();
 static final RuntimeException marker = new RuntimeException("marker");
 static int mode;
 static int first(int mode) { log.append("first|"); if(mode==1) throw marker; return mode; }
 static String second(int mode) { log.append("second|"); if(mode==2) throw marker; return "value"; }
 static Response existing(int mode) { log.append("existing|"); return null; }
 @SuppressWarnings("unchecked") static <T> T make(int mode) { log.append("generic|"); if(mode==1)throw marker; return (T)(mode==2?null:"value"); }
}
class Response {
 static { Effects.log.append("init|"); }
 Response(int mode, Object value) { Effects.log.append("object|"); if(mode==3) throw Effects.marker; }
 Response(int mode, String value) { throw new AssertionError("wrong overload"); }
}
public class Check {
 public static void main(String[] args) throws Exception {
  Class<?> c=Class.forName("sample."+args[0]);
  int mode=Integer.parseInt(args[1]);
  try { Object r=c.getMethod("create",int.class).invoke(null,mode); System.out.print(r.getClass().getSimpleName()); }
  catch(java.lang.reflect.InvocationTargetException e) { if(e.getCause()!=Effects.marker) throw e; System.out.print("marker"); }
  System.out.println(":"+Effects.log);
  String[] expected={"init|first|second|object|","init|first|","init|first|second|","init|first|second|object|"};
  if(!Effects.log.toString().equals(expected[mode])) throw new AssertionError(Effects.log);
  if(c.getMethod("cast",int.class).invoke(null,0)!=null) throw new AssertionError();
  Effects.log.setLength(0);
  try { System.out.println("generic:"+c.getMethod("generic",int.class).invoke(null,mode)); }
  catch(java.lang.reflect.InvocationTargetException e) { if(e.getCause()!=Effects.marker)throw e; System.out.println("generic:marker"); }
  if(!Effects.log.toString().equals("generic|"))throw new AssertionError(Effects.log);
  for(int limit: new int[]{-2,0,1,2,3,4,10}) {
   Effects.log.setLength(0);
   Object r=c.getMethod("loop",int.class).invoke(null,limit);
   System.out.println(limit+":"+r+":"+Effects.log);
  }
  for(int x:new int[]{Integer.MIN_VALUE,-1,0,1,Integer.MAX_VALUE}) {
   for(int y:new int[]{-1,0,1,2}) {
    try { System.out.println(c.getMethod("division",int.class,int.class).invoke(null,x,y)); }
    catch(java.lang.reflect.InvocationTargetException e) { if(!(e.getCause() instanceof ArithmeticException)) throw e; System.out.println("divide-zero"); }
   }
  }
 }
}"#;
        fs::write(directory.join("sample/Check.java"), support).unwrap();
        let compiled = Command::new("javac")
            .args([
                "sample/Before.java",
                "sample/After.java",
                "sample/Check.java",
            ])
            .current_dir(&directory)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        for mode in 0..4 {
            let run = |name: &str| {
                let output = Command::new("java")
                    .args(["sample.Check", name, &mode.to_string()])
                    .current_dir(&directory)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                output.stdout
            };
            assert_eq!(run("Before"), run("After"));
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
