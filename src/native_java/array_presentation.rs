//! Compact complete literal-only primitive array initialization sequences.
use super::condition_cleanup::{Edit, apply, syntax};
use crate::engine::DecompiledCode;

fn literal(value: &str, ty: &str) -> bool {
    if ty == "boolean" {
        return matches!(value, "true" | "false");
    }
    let value = value.strip_prefix('-').unwrap_or(value);
    if value.is_empty() || !value.as_bytes()[0].is_ascii_digit() {
        return false;
    }
    let value = match ty {
        "long" => value.strip_suffix('L').unwrap_or(value),
        "float" => value.strip_suffix('f').unwrap_or(value),
        "double" => value.strip_suffix('d').unwrap_or(value),
        "byte" | "short" | "char" | "int" => value,
        _ => return false,
    };
    if matches!(ty, "float" | "double") {
        value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
            && value.parse::<f64>().is_ok()
    } else {
        !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
    }
}

pub(super) fn simplify(code: DecompiledCode) -> DecompiledCode {
    if code.source.len() > 512 * 1024
        || code.source.contains("/*")
        || code.source.contains("\"\"\"")
    {
        return code;
    }
    let Some(parsed) = syntax(&code.source) else {
        return code;
    };
    let mut offset = 0;
    let lines: Vec<_> = code
        .source
        .split_inclusive('\n')
        .map(|line| {
            let start = offset;
            offset += line.len();
            (start, line)
        })
        .collect();
    let mut changes = Vec::new();
    let mut removed_guards = Vec::new();
    let mut cursor = 0;
    while cursor < lines.len() {
        let (start, line) = lines[cursor];
        cursor += 1;
        let indent = line.len() - line.trim_start().len();
        if indent < 8 || !parsed.code.get(start + indent).copied().unwrap_or(false) {
            continue;
        }
        let Some((left, right)) = line
            .trim()
            .strip_suffix(';')
            .and_then(|s| s.split_once(" = "))
        else {
            continue;
        };
        let Some((array, name)) = left.split_once(' ') else {
            continue;
        };
        let Some(ty) = array.strip_suffix("[]") else {
            continue;
        };
        if !matches!(
            ty,
            "boolean" | "byte" | "short" | "char" | "int" | "long" | "float" | "double"
        ) || !name
            .strip_prefix('v')
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let Some(length) = right
            .strip_prefix(&format!("new {ty}["))
            .and_then(|s| s.strip_suffix(']'))
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0 && n <= 4096)
        else {
            continue;
        };
        let pad = " ".repeat(indent);
        let mut at = cursor;
        let mut guard = None;
        if lines
            .get(at)
            .is_some_and(|(_, s)| s.trim() == format!("if ({name}.length < {length}) {{"))
        {
            let expected = format!(
                "{pad}if ({name}.length < {length}) {{\n{pad}    throw new java.lang.ArrayIndexOutOfBoundsException();\n{pad}}}\n"
            );
            let Some(slice) = lines.get(at..at + 3) else {
                continue;
            };
            if slice.iter().map(|(_, s)| *s).collect::<String>() != expected {
                continue;
            }
            guard = Some((lines[at].0, lines[at + 2].0 + lines[at + 2].1.len()));
            at += 3;
        }
        let Some(stores) = lines.get(at..at + length) else {
            continue;
        };
        let mut values = Vec::new();
        for (index, (_, store)) in stores.iter().enumerate() {
            let prefix = format!("{pad}{name}[{index}] = ");
            let Some(value) = store
                .strip_prefix(&prefix)
                .and_then(|s| s.trim_end().strip_suffix(';'))
                .filter(|s| literal(s, ty))
            else {
                break;
            };
            values.push(value);
        }
        if values.len() != length {
            continue;
        }
        let end = stores.last().unwrap().0 + stores.last().unwrap().1.len();
        let mut text = format!("{pad}{array} {name} = new {ty}[] {{\n");
        let mut row = format!("{pad}    ");
        for (index, value) in values.iter().enumerate() {
            if row.len() + value.len() + 2 > 112 && !row.trim().is_empty() {
                text.push_str(row.trim_end());
                text.push('\n');
                row = format!("{pad}    ");
            }
            row.push_str(value);
            if index + 1 < length {
                row.push_str(", ");
            }
        }
        text.push_str(row.trim_end());
        text.push('\n');
        text.push_str(&format!("{pad}}};\n"));
        changes.push((start, end, text));
        if let Some(guard) = guard {
            removed_guards.push(guard);
        }
        cursor = at + length;
        if changes.len() >= 256 {
            return code;
        }
    }
    if changes.is_empty()
        || (code.links.len() + code.definitions.len()).saturating_mul(changes.len()) > 2_000_000
    {
        return code;
    }
    let mut scalars = vec![0; code.source.len() + 1];
    for (scalar, (byte, _)) in code.source.char_indices().enumerate() {
        scalars[byte] = scalar;
    }
    scalars[code.source.len()] = code.source.chars().count();
    let mut candidate = code.clone();
    // The exact impossible bounds-check constructor is the only removable link.
    candidate.links.retain(|link| {
        !removed_guards.iter().any(|&(a, b)| {
            link.start >= scalars[a]
                && link.end <= scalars[b]
                && matches!(
                    link.label.as_str(),
                    "java.lang.ArrayIndexOutOfBoundsException"
                        | "java.lang.ArrayIndexOutOfBoundsException.<init>()V"
                )
        })
    });
    let edits = changes
        .iter()
        .map(|(start, end, text)| Edit {
            start: *start,
            end: *end,
            text,
        })
        .collect();
    let (updated, changed) = apply(candidate, edits);
    if changed { updated } else { code }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CodeLink, source_identity};
    fn code(source: &str) -> DecompiledCode {
        DecompiledCode {
            source: source.into(),
            source_hash: source_identity(source),
            links: vec![],
            definitions: vec![],
        }
    }
    fn source(values: &[&str], ty: &str) -> String {
        let mut s = format!(
            "    static {{\n        {ty}[] v0 = new {ty}[{}];\n        if (v0.length < {}) {{\n            throw new java.lang.ArrayIndexOutOfBoundsException();\n        }}\n",
            values.len(),
            values.len()
        );
        for (i, v) in values.iter().enumerate() {
            s.push_str(&format!("        v0[{i}] = {v};\n"));
        }
        s.push_str("        save(v0);\n    }\n");
        s
    }
    #[test]
    fn ordered_literal_initializers_preserve_values_and_following_navigation() {
        for (ty, values) in [
            ("int", vec!["-2147483648", "0", "2147483647"]),
            (
                "long",
                vec!["-9223372036854775808L", "9223372036854775807L"],
            ),
            ("float", vec!["-0.0f", "1.0E-4f", "1.0f"]),
            ("boolean", vec!["true", "false"]),
        ] {
            let s = source(&values, ty);
            let mut c = code(&s);
            let start = s.find("save").unwrap();
            c.links.push(CodeLink {
                start,
                end: start + 4,
                label: "sample.Target.save([I)V".into(),
            });
            let after = simplify(c);
            assert!(
                after.source.contains(&format!("new {ty}[] {{")),
                "{}",
                after.source
            );
            assert!(!after.source.contains("length <"));
            for v in values {
                assert!(after.source.contains(v));
            }
            assert_eq!(
                after
                    .source
                    .chars()
                    .skip(after.links[0].start)
                    .take(4)
                    .collect::<String>(),
                "save"
            );
            assert_eq!(after.source_hash, source_identity(&after.source));
            assert_eq!(simplify(after.clone()).source, after.source);
        }
    }
    #[test]
    fn incomplete_effectful_reordered_or_annotated_arrays_remain_exact() {
        let original = source(&["1", "2", "3"], "int");
        for s in [
            original.replace("v0[1] = 2", "v0[0] = 2"),
            original.replace("v0[1] = 2", "v0[1] = next()"),
            original.replace("        v0[1] = 2;\n", ""),
            original.replace("length < 3", "length < 4"),
            original.replace("v0[1] = 2;", "v0[1] = 2; // note"),
        ] {
            assert_eq!(simplify(code(&s)).source, s);
        }
        let mut c = code(&original);
        let start = original.find("v0[1]").unwrap();
        c.links.push(CodeLink {
            start,
            end: start + 2,
            label: "opaque".into(),
        });
        assert_eq!(simplify(c).source, original);
    }
    #[test]
    #[ignore = "requires javac and java on PATH"]
    fn literal_arrays_match_jvm_raw_bits_and_large_table_order() {
        use std::{fs, process::Command};
        let dir =
            std::env::temp_dir().join(format!("rdx-array-presentation-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let large: Vec<_> = (0..634)
            .map(|i| format!("{}L", i64::MIN.wrapping_add(i * 19703)))
            .collect();
        let large_refs: Vec<_> = large.iter().map(String::as_str).collect();
        let methods = [
            source(&["-0.0f", "0.0f", "1.0E-4f", "3.4028235E38f"], "float"),
            source(&large_refs, "long"),
        ]
        .join("\n");
        let after = simplify(code(&methods)).source;
        assert!(after.lines().count() < methods.lines().count() / 2);
        let helpers = "static StringBuilder result=new StringBuilder();static void save(float[] a){for(float v:a)result.append(Float.floatToRawIntBits(v)).append(',');}static void save(long[] a){for(long v:a)result.append(v).append(',');}";
        for (name, s) in [("Before", methods.as_str()), ("After", after.as_str())] {
            fs::write(
                dir.join(format!("{name}.java")),
                format!("class {name} {{{helpers}{s}}}"),
            )
            .unwrap();
        }
        fs::write(dir.join("Check.java"),"class Check { public static void main(String[] args) {if(!Before.result.toString().equals(After.result.toString()))throw new AssertionError();System.out.print(After.result.toString().split(\",\").length);} }").unwrap();
        for (program, args) in [
            ("javac", vec!["Before.java", "After.java", "Check.java"]),
            ("java", vec!["-cp", ".", "Check"]),
        ] {
            let r = Command::new(program)
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
            if program == "java" {
                assert_eq!(r.stdout, b"638");
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
