//! Small, source-only cleanup for Java boilerplate emitted by method reconstruction.

use crate::{
    engine::{CodeDefinition, CodeLink, DecompiledCode},
    native_dex::DexMethod,
};

const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const LEADING_SUPER: &str = "        super();\n";
const TERMINAL_RETURN: &str = "        return;\n";
const METHOD_CLOSE: &str = "    }\n";
const OBJECT_RETURN_PREFIX: &str = "return ((java.lang.Object) ";
const MAX_EDITS: usize = 8192;
const MAX_METADATA_WORK: usize = 2_000_000;
const NUMERIC_TYPES: [&str; 7] = ["byte", "short", "char", "int", "long", "float", "double"];

#[derive(Clone, Copy)]
struct Edit {
    byte_start: usize,
    byte_end: usize,
    char_start: usize,
    char_end: usize,
    removes_super_link: bool,
}

fn byte_at(source: &str, position: usize, total_chars: usize) -> Option<usize> {
    if position == total_chars {
        return Some(source.len());
    }
    source.char_indices().nth(position).map(|(byte, _)| byte)
}

fn overlaps(start: usize, end: usize, edit: Edit) -> bool {
    start < edit.char_end && end > edit.char_start
}

fn safe_edit(source: &str, links: &[CodeLink], definitions: &[CodeDefinition], edit: Edit) -> bool {
    if definitions
        .iter()
        .any(|definition| overlaps(definition.start, definition.end, edit))
    {
        return false;
    }
    let affected: Vec<_> = links
        .iter()
        .filter(|link| overlaps(link.start, link.end, edit))
        .collect();
    if !edit.removes_super_link {
        return affected.is_empty();
    }
    affected.len() == 1
        && affected[0].start >= edit.char_start
        && affected[0].end <= edit.char_end
        && affected[0].label.ends_with(".<init>()V")
        && source[edit.byte_start..edit.byte_end]
            .chars()
            .skip(affected[0].start - edit.char_start)
            .take(affected[0].end - affected[0].start)
            .eq("super".chars())
}

fn edit_at(
    source: &str,
    byte_start: usize,
    byte_end: usize,
    char_start: usize,
    removes_super_link: bool,
) -> Edit {
    Edit {
        byte_start,
        byte_end,
        char_start,
        char_end: char_start + source[byte_start..byte_end].chars().count(),
        removes_super_link,
    }
}

fn generated_local(name: &str) -> bool {
    name.strip_prefix('v')
        .or_else(|| name.strip_prefix('p'))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

// Only emitter-owned local reads and decimal literals are atoms here. Calls,
// field/array reads, casts and compound expressions keep their grouping.
fn arithmetic_atom(text: &str) -> bool {
    if text.len() > 128 {
        return false;
    }
    if generated_local(text) {
        return true;
    }
    let text = text.strip_prefix('-').unwrap_or(text);
    let (number, long) = if let Some(number) = text.strip_suffix('L') {
        (number, true)
    } else {
        (text.strip_suffix(['f', 'd']).unwrap_or(text), false)
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut exponent = number.split(['e', 'E']);
    let mantissa = exponent.next().unwrap_or("");
    if let Some(power) = exponent.next()
        && (long || !digits(power.strip_prefix(['-', '+']).unwrap_or(power)))
    {
        return false;
    }
    if exponent.next().is_some() {
        return false;
    }
    let mut decimal = mantissa.split('.');
    if !digits(decimal.next().unwrap_or("")) {
        return false;
    }
    if let Some(fraction) = decimal.next()
        && (long || !digits(fraction))
    {
        return false;
    }
    decimal.next().is_none()
}

fn arithmetic_operand(text: &str) -> Option<Option<(usize, usize)>> {
    if arithmetic_atom(text) {
        return Some(None);
    }
    let atom = text.strip_prefix('(')?.strip_suffix(')')?;
    arithmetic_atom(atom).then_some(Some((0, text.len() - 1)))
}

fn arithmetic_parentheses(expression: &str) -> Vec<usize> {
    for operator in [
        " >>> ", " >> ", " << ", " + ", " - ", " * ", " / ", " % ", " & ", " | ", " ^ ",
    ] {
        if let Some((left, right)) = expression.split_once(operator)
            && let (Some(left_pair), Some(right_pair)) =
                (arithmetic_operand(left), arithmetic_operand(right))
        {
            let mut removed = Vec::with_capacity(4);
            if let Some((open, close)) = left_pair {
                removed.extend([open, close]);
            }
            if let Some((open, close)) = right_pair {
                let start = left.len() + operator.len();
                removed.extend([start + open, start + close]);
            }
            return removed;
        }
    }
    for operator in ['-', '~'] {
        if let Some(operand) = expression.strip_prefix(operator)
            && let Some(Some((open, close))) = arithmetic_operand(operand)
            // Keep negative operands grouped; removing them after '-' could
            // turn unary negation into a decrement token.
            && !operand[1..].starts_with('-')
        {
            return vec![1 + open, 1 + close];
        }
    }
    for ty in NUMERIC_TYPES {
        let prefix = format!("({ty}) ");
        if let Some(operand) = expression.strip_prefix(&prefix)
            && let Some(Some((open, close))) = arithmetic_operand(operand)
        {
            return vec![prefix.len() + open, prefix.len() + close];
        }
    }
    Vec::new()
}

fn arithmetic_statement(statement: &str, numeric_return: bool) -> Option<&str> {
    let statement = statement.strip_suffix(';')?;
    if numeric_return && let Some(expression) = statement.strip_prefix("return ") {
        return Some(expression);
    }
    let (declaration, expression) = statement.split_once(" = ")?;
    let (ty, local) = declaration.split_once(' ')?;
    (NUMERIC_TYPES.contains(&ty) && generated_local(local)).then_some(expression)
}

// Descend only through exactly linked unary Math calls. The operand grammar
// still excludes compound expressions, fields, casts and other invocations.
fn math_argument_parentheses(
    expression: &str,
    expression_char: usize,
    links: &[CodeLink],
    depth: usize,
) -> Vec<usize> {
    let direct = arithmetic_parentheses(expression);
    if !direct.is_empty() || depth == 8 || !expression.is_ascii() {
        return direct;
    }
    let Some(open) = expression.find('(') else {
        return direct;
    };
    let Some(argument) = expression[open + 1..].strip_suffix(')') else {
        return direct;
    };
    let Some(name) = expression[..open]
        .strip_prefix("java.lang.Math.")
        .or_else(|| expression[..open].strip_prefix("Math."))
    else {
        return direct;
    };
    if !matches!(
        name,
        "abs" | "sin" | "cos" | "tan" | "sqrt" | "ceil" | "floor" | "rint"
    ) {
        return direct;
    }
    let start = expression_char + open - name.len();
    if !links.iter().any(|link| {
        link.start == start
            && link.end == expression_char + open
            && (link.label == format!("java.lang.Math.{name}(D)D")
                || name == "abs"
                    && ["(I)I", "(J)J", "(F)F"]
                        .iter()
                        .any(|signature| link.label == format!("java.lang.Math.abs{signature}")))
    }) {
        return direct;
    }
    math_argument_parentheses(argument, expression_char + open + 1, links, depth + 1)
        .into_iter()
        .map(|position| position + open + 1)
        .collect()
}

fn redundant_arithmetic_parentheses(
    code: &DecompiledCode,
    numeric_return: bool,
) -> Option<Vec<Edit>> {
    if code.source.contains("/*") || code.source.contains("\"\"\"") {
        return Some(Vec::new());
    }
    let mut edits = Vec::new();
    let mut candidates_seen = 0usize;
    let mut call_lookup_work = 0usize;
    let (mut byte_start, mut char_start) = (0, 0);
    for line in code.source.split_inclusive('\n') {
        let statement = line.trim();
        if line.len() <= 512
            && let Some(expression) = arithmetic_statement(statement, numeric_return)
        {
            let indent = line.len() - line.trim_start().len();
            let expression_byte = indent + statement.len() - 1 - expression.len();
            let expression_char = char_start + line[..expression_byte].chars().count();
            call_lookup_work = call_lookup_work.saturating_add(code.links.len().saturating_mul(8));
            if call_lookup_work > MAX_METADATA_WORK {
                return None;
            }
            let positions = math_argument_parentheses(expression, expression_char, &code.links, 0);
            candidates_seen = candidates_seen.checked_add(positions.len())?;
            if candidates_seen > MAX_EDITS
                || code
                    .links
                    .len()
                    .saturating_add(code.definitions.len())
                    .saturating_mul(candidates_seen)
                    > MAX_METADATA_WORK
            {
                return None;
            }
            if !positions.is_empty() {
                // Accepted arithmetic expressions are ASCII, so local byte
                // offsets also index Unicode scalar positions within them.
                let candidates: Vec<_> = positions
                    .into_iter()
                    .map(|position| {
                        edit_at(
                            &code.source,
                            byte_start + expression_byte + position,
                            byte_start + expression_byte + position + 1,
                            expression_char + position,
                            false,
                        )
                    })
                    .collect();
                if candidates
                    .iter()
                    .all(|edit| safe_edit(&code.source, &code.links, &code.definitions, *edit))
                {
                    edits.extend(candidates);
                }
            }
        }
        byte_start += line.len();
        char_start += line.chars().count();
    }
    Some(edits)
}

fn redundant_object_return_casts(code: &DecompiledCode) -> Option<Vec<Edit>> {
    let mut edits = Vec::new();
    let mut candidates = 0usize;
    let (mut line_start, mut line_char_start) = (0, 0);
    for line in code.source.split_inclusive('\n') {
        let content = line.trim_end_matches('\n');
        let indent = content.len() - content.trim_start_matches(' ').len();
        let statement = &content[indent..];
        if let Some(rest) = statement.strip_prefix(OBJECT_RETURN_PREFIX)
            && let Some(local) = rest.strip_suffix(");")
            && generated_local(local)
        {
            candidates += 2;
            if candidates > MAX_EDITS {
                return None;
            }
            if (code.links.len() + code.definitions.len()).saturating_mul(candidates)
                > MAX_METADATA_WORK
            {
                return None;
            }
            let expression = line_start + indent + "return ".len();
            let local_start = expression + "((java.lang.Object) ".len();
            let close = local_start + local.len();
            let expression_char = line_char_start + indent + "return ".len();
            let local_char = expression_char + "((java.lang.Object) ".len();
            let close_char = local_char + local.len();
            let prefix = edit_at(
                &code.source,
                expression,
                local_start,
                expression_char,
                false,
            );
            let suffix = edit_at(&code.source, close, close + 1, close_char, false);
            if safe_edit(&code.source, &code.links, &code.definitions, prefix)
                && safe_edit(&code.source, &code.links, &code.definitions, suffix)
            {
                edits.push(prefix);
                edits.push(suffix);
            }
        }
        line_start += line.len();
        line_char_start += line.chars().count();
    }
    Some(edits)
}

fn apply(mut code: DecompiledCode, mut edits: Vec<Edit>) -> DecompiledCode {
    if edits.is_empty() {
        return code;
    }
    edits.sort_by_key(|edit| edit.byte_start);
    if edits
        .windows(2)
        .any(|pair| pair[0].byte_end > pair[1].byte_start)
    {
        return code;
    }
    code.links.retain(|link| {
        !edits
            .iter()
            .any(|edit| overlaps(link.start, link.end, *edit))
    });
    let remap = |position: usize| {
        let removed: usize = edits
            .iter()
            .filter(|edit| edit.char_end <= position)
            .map(|edit| edit.char_end - edit.char_start)
            .sum();
        position - removed
    };
    for link in &mut code.links {
        link.start = remap(link.start);
        link.end = remap(link.end);
    }
    for definition in &mut code.definitions {
        definition.start = remap(definition.start);
        definition.end = remap(definition.end);
    }
    let mut source = String::with_capacity(code.source.len());
    let mut cursor = 0;
    for edit in edits {
        source.push_str(&code.source[cursor..edit.byte_start]);
        cursor = edit.byte_end;
    }
    source.push_str(&code.source[cursor..]);
    code.source = source;
    code.source_hash = crate::engine::source_identity(&code.source);
    code
}

pub(super) fn simplify(code: DecompiledCode, method: &DexMethod) -> DecompiledCode {
    if code.source.len() > MAX_SOURCE_BYTES || method.code.is_none() {
        return code;
    }
    let void = method.return_type.as_ref() == "V";
    let object = method.return_type.as_ref() == "Ljava/lang/Object;";
    let constructor = method.name.as_ref() == "<init>";
    let numeric_return = matches!(
        method.return_type.as_ref(),
        "B" | "S" | "C" | "I" | "J" | "F" | "D"
    );
    let Some(mut edits) = redundant_arithmetic_parentheses(&code, numeric_return) else {
        return code;
    };
    let total_chars = code.source.chars().count();
    if object {
        let Some(cast_edits) = redundant_object_return_casts(&code) else {
            return code;
        };
        edits.extend(cast_edits);
    }
    if constructor {
        let mut definitions = code
            .definitions
            .iter()
            .filter(|definition| definition.kind == "method");
        if let Some(definition) = definitions.next()
            && definitions.next().is_none()
            && let Some(after_name) = byte_at(&code.source, definition.end, total_chars)
            && let Some(open) = code.source[after_name..].find(" {\n")
        {
            let start = after_name + open + " {\n".len();
            let end = start + LEADING_SUPER.len();
            if code.source.get(start..end) == Some(LEADING_SUPER) {
                let char_start = definition.end + code.source[after_name..start].chars().count();
                let candidate = edit_at(&code.source, start, end, char_start, true);
                if safe_edit(&code.source, &code.links, &code.definitions, candidate) {
                    edits.push(candidate);
                }
            }
        }
    }
    let suffix = format!("{TERMINAL_RETURN}{METHOD_CLOSE}");
    if void
        && let Some(start) = code.source.len().checked_sub(suffix.len())
        && code.source.get(start..) == Some(suffix.as_str())
    {
        let char_start = total_chars - suffix.chars().count();
        let candidate = edit_at(
            &code.source,
            start,
            start + TERMINAL_RETURN.len(),
            char_start,
            false,
        );
        if safe_edit(&code.source, &code.links, &code.definitions, candidate) {
            edits.push(candidate);
        }
    }
    if edits.len() > MAX_EDITS
        || code
            .links
            .len()
            .saturating_add(code.definitions.len())
            .saturating_mul(edits.len())
            > MAX_METADATA_WORK
    {
        return code;
    }
    apply(code, edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{CodeDefinition, CodeLink},
        native_dex::DexCode,
    };
    use std::sync::Arc;

    fn method(name: &str, return_type: &str) -> DexMethod {
        DexMethod {
            declaring_type: Arc::from("Lsample/Example;"),
            name: Arc::from(name),
            return_type: Arc::from(return_type),
            parameters: vec![],
            thrown_types: vec![],
            access_flags: if name == "<init>" { 1 } else { 9 },
            code: Some(DexCode {
                registers: 1,
                ins: u16::from(name == "<init>"),
                outs: 1,
                tries: 0,
                try_regions: vec![],
                instructions: vec![0x000e],
                offset: 0,
            }),
        }
    }

    fn code(source: &str, method_name: &str) -> DecompiledCode {
        let name_start = source.find("Example").unwrap();
        let super_start = source.find("super").unwrap_or(0);
        DecompiledCode {
            source: source.to_owned(),
            source_hash: crate::engine::source_identity(source),
            links: (super_start != 0)
                .then(|| CodeLink {
                    start: super_start,
                    end: super_start + 5,
                    label: "java.lang.Object.<init>()V".into(),
                })
                .into_iter()
                .collect(),
            definitions: vec![CodeDefinition {
                start: name_start,
                end: name_start + "Example".len(),
                kind: "method".into(),
                name: method_name.into(),
            }],
        }
    }

    #[test]
    fn constructor_boilerplate_is_removed_with_its_link() {
        let before = "    public Example() {\n        super();\n        return;\n    }\n";
        let after = simplify(code(before, "<init>"), &method("<init>", "V"));
        assert_eq!(after.source, "    public Example() {\n    }\n");
        assert!(after.links.is_empty());
        assert_eq!(after.definitions[0].start, 11);
        assert_eq!(after.definitions[0].end, 18);
    }

    #[test]
    fn terminal_void_return_is_removed_but_branch_return_remains() {
        let before = "    public static void Example() {\n        if (p0) {\n            return;\n        }\n        return;\n    }\n";
        let after = simplify(code(before, "run"), &method("run", "V"));
        assert_eq!(after.source.matches("return;").count(), 1);
        assert!(after.source.contains("            return;"));
    }

    #[test]
    fn value_method_and_unexpected_super_metadata_stay_unchanged() {
        let value = "    public int Example() {\n        return 1;\n    }\n";
        assert_eq!(
            simplify(code(value, "value"), &method("value", "I")).source,
            value
        );
        let constructor = "    public Example() {\n        super();\n        return;\n    }\n";
        let mut malformed = code(constructor, "<init>");
        malformed.links[0].label = "sample.Effects.call()V".into();
        let after = simplify(malformed, &method("<init>", "V"));
        assert!(after.source.contains("super();"));
        assert!(!after.source.contains("return;"));
    }

    #[test]
    fn arithmetic_atoms_lose_only_redundant_grouping() {
        for (expression, expected) in [
            ("(p0) * (v1)", "p0 * v1"),
            ("v0 + (1)", "v0 + 1"),
            ("(v0) - (-1)", "v0 - -1"),
            ("(v0) / (1.0e-9d)", "v0 / 1.0e-9d"),
            ("(v0) >>> (32)", "v0 >>> 32"),
            ("-(p0)", "-p0"),
            ("~(v0)", "~v0"),
            ("(double) (v0)", "(double) v0"),
            ("(int) (-1.75f)", "(int) -1.75f"),
        ] {
            let before = format!(
                "    public double Example() {{\n        double v2 = {expression};\n        return v2;\n    }}\n"
            );
            let after = simplify(code(&before, "value"), &method("value", "D"));
            assert!(
                after.source.contains(&format!("double v2 = {expected};")),
                "{}",
                after.source
            );
            assert_eq!(
                after.source_hash,
                crate::engine::source_identity(&after.source)
            );
            assert_eq!(
                simplify(after.clone(), &method("value", "D")).source,
                after.source
            );
        }
    }

    #[test]
    fn arithmetic_return_cleanup_requires_a_numeric_method_return() {
        let before = "    public double Example() {\n        return (p0) * (p1);\n    }\n";
        let after = simplify(code(before, "value"), &method("value", "D"));
        assert!(after.source.contains("return p0 * p1;"));
        assert_eq!(
            simplify(
                code(before, "value"),
                &method("value", "Ljava/lang/Object;")
            )
            .source,
            before
        );
    }

    #[test]
    fn math_arguments_require_exact_links_and_keep_compound_grouping() {
        for (expression, expected) in [
            (
                "java.lang.Math.tan((v0) / (v1))",
                "java.lang.Math.tan(v0 / v1)",
            ),
            (
                "java.lang.Math.ceil(java.lang.Math.abs((v0) / (3.141592653589793d)))",
                "java.lang.Math.ceil(java.lang.Math.abs(v0 / 3.141592653589793d))",
            ),
            (
                "java.lang.Math.sqrt((v0) + (v1))",
                "java.lang.Math.sqrt(v0 + v1)",
            ),
            (
                "java.lang.Math.sqrt((v0 + v1) * (v2))",
                "java.lang.Math.sqrt((v0 + v1) * (v2))",
            ),
        ] {
            let before = format!(
                "    public double Example() {{\n        double v3 = {expression};\n        return v3;\n    }}\n"
            );
            let original = code(&before, "value");
            assert_eq!(
                simplify(original.clone(), &method("value", "D")).source,
                before
            );
            let mut linked = original;
            for name in ["tan", "ceil", "abs", "sqrt"] {
                if let Some(start) = before.find(&format!("{name}(")) {
                    linked.links.push(CodeLink {
                        start,
                        end: start + name.len(),
                        label: format!("java.lang.Math.{name}(D)D"),
                    });
                }
            }
            let after = simplify(linked.clone(), &method("value", "D"));
            assert!(after.source.contains(expected), "{}", after.source);
            for link in after.links {
                assert_eq!(
                    after
                        .source
                        .chars()
                        .skip(link.start)
                        .take(link.end - link.start)
                        .collect::<String>(),
                    link.label
                        .split('.')
                        .next_back()
                        .unwrap()
                        .split('(')
                        .next()
                        .unwrap()
                );
            }
            linked
                .links
                .iter_mut()
                .for_each(|link| link.label = link.label.replace("java.lang", "app.fake"));
            assert_eq!(simplify(linked, &method("value", "D")).source, before);
        }
    }

    #[test]
    fn arithmetic_grouping_keeps_compounds_calls_casts_and_unary_token_boundaries() {
        for expression in [
            "(v0 + v1) * (p0)",
            "(v0) * (v1 + p0)",
            "v0 / (v1 * p0)",
            "v0 - (v1 - p0)",
            "v0 << (v1 + p0)",
            "(int) (v0 + v1)",
            "(v0.call()) + (1)",
            "v0(v1)",
            "-(v0 + v1)",
            "-(-1)",
            "-(-2147483648)",
            "(v0.field) + (1)",
            "(v0[1]) + (1)",
            "(v0++) + (1)",
            "(1.5L) + (v0)",
            "(1e2L) + (v0)",
        ] {
            let before = format!(
                "    public double Example() {{\n        double v2 = {expression};\n        return v2;\n    }}\n"
            );
            assert_eq!(
                simplify(code(&before, "value"), &method("value", "D")).source,
                before
            );
        }
        for prefix in ["/*\n", "String text = \"\"\"\n"] {
            let before = format!(
                "    public double Example() {{\n{prefix}        double v2 = (v0) * (v1);\n    }}\n"
            );
            assert_eq!(
                simplify(code(&before, "value"), &method("value", "D")).source,
                before
            );
        }
    }

    #[test]
    fn arithmetic_edits_preserve_unicode_navigation_and_refuse_spanning_metadata() {
        let before = "    // λ😀\n    public double Example() {\n        double v2 = (p0) * (p1);\n        Effects.touch();\n        return v2;\n    }\n";
        let mut original = code(before, "value");
        let scalar = |needle: &str| before[..before.find(needle).unwrap()].chars().count();
        original.definitions[0].start = scalar("Example");
        original.definitions[0].end = scalar("Example") + 7;
        original.links.push(CodeLink {
            start: scalar("touch"),
            end: scalar("touch") + 5,
            label: "sample.Effects.touch()V".into(),
        });
        let after = simplify(original.clone(), &method("value", "D"));
        assert!(after.source.contains("double v2 = p0 * p1;"));
        for (link, token) in after
            .links
            .iter()
            .map(|link| ((link.start, link.end), "touch"))
            .chain(
                after
                    .definitions
                    .iter()
                    .map(|definition| ((definition.start, definition.end), "Example")),
            )
        {
            assert_eq!(
                after
                    .source
                    .chars()
                    .skip(link.0)
                    .take(link.1 - link.0)
                    .collect::<String>(),
                token
            );
        }
        original.links.push(CodeLink {
            start: scalar("(p0)"),
            end: scalar("(p0)") + 4,
            label: "opaque".into(),
        });
        assert_eq!(simplify(original, &method("value", "D")).source, before);
    }

    #[test]
    fn simple_object_return_casts_are_removed_only_from_return_operands() {
        let before = concat!(
            "    public java.lang.Object Example() {\n",
            "        sample.Use.accept(((java.lang.Object) v1));\n",
            "        if (p0) {\n",
            "            return ((java.lang.Object) p2);\n",
            "        }\n",
            "        return ((java.lang.Object) v12);\n",
            "    }\n",
        );
        let after = simplify(
            code(before, "value"),
            &method("value", "Ljava/lang/Object;"),
        );
        assert!(after.source.contains("accept(((java.lang.Object) v1))"));
        assert!(after.source.contains("return p2;"));
        assert!(after.source.contains("return v12;"));
        assert!(!after.source.contains("return ((java.lang.Object)"));
    }

    #[test]
    fn object_return_cleanup_rejects_complex_or_differently_typed_values() {
        for (return_type, expression) in [
            ("Ljava/lang/Object;", "response"),
            ("Ljava/lang/Object;", "v1.value"),
            ("Ljava/lang/String;", "v1"),
        ] {
            let before = format!(
                "    public java.lang.Object Example() {{\n        return ((java.lang.Object) {expression});\n    }}\n"
            );
            let after = simplify(code(&before, "value"), &method("value", return_type));
            assert_eq!(after.source, before);
        }
    }

    #[test]
    fn unicode_prefix_preserves_remaining_link_definition_and_hash() {
        let before = concat!(
            "    // λ\n",
            "    public Example() {\n",
            "        super();\n",
            "        sample.Effects.touch(\"é\");\n",
            "        return;\n",
            "    }\n",
        );
        let position = |needle: &str| before[..before.find(needle).unwrap()].chars().count();
        let name_start = position("Example");
        let super_start = position("super");
        let touch_start = position("touch");
        let original = DecompiledCode {
            source: before.into(),
            source_hash: crate::engine::source_identity(before),
            links: vec![
                CodeLink {
                    start: super_start,
                    end: super_start + 5,
                    label: "java.lang.Object.<init>()V".into(),
                },
                CodeLink {
                    start: touch_start,
                    end: touch_start + 5,
                    label: "sample.Effects.touch(Ljava/lang/String;)V".into(),
                },
            ],
            definitions: vec![CodeDefinition {
                start: name_start,
                end: name_start + 7,
                kind: "method".into(),
                name: "<init>".into(),
            }],
        };
        let after = simplify(original, &method("<init>", "V"));
        assert!(!after.source.contains("super();"));
        assert!(!after.source.contains("return;"));
        assert_eq!(
            after.source_hash,
            crate::engine::source_identity(&after.source)
        );
        assert_eq!(after.links.len(), 1);
        assert_eq!(
            after
                .source
                .chars()
                .skip(after.links[0].start)
                .take(after.links[0].end - after.links[0].start)
                .collect::<String>(),
            "touch"
        );
        assert_eq!(
            after
                .source
                .chars()
                .skip(after.definitions[0].start)
                .take(after.definitions[0].end - after.definitions[0].start)
                .collect::<String>(),
            "Example"
        );
    }
}
