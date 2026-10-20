//! Declaration presence accounting, separate from semantic correctness.
use crate::{
    engine::{CodeDefinition, DecompiledCode},
    native_dex::{DexClass, DexMethod},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn method_id(name: &str, method: &DexMethod) -> String {
    format!(
        "{name}.{}({}){}",
        method.name,
        method.parameters.join(""),
        method.return_type
    )
}

fn declaration_index<'a>(
    code: &'a DecompiledCode,
    chars: &[char],
) -> BTreeMap<String, &'a CodeDefinition> {
    let definitions: BTreeMap<_, _> = code
        .definitions
        .iter()
        .filter(|d| d.kind == "method" && d.start <= d.end && d.end <= chars.len())
        .map(|d| ((d.start, d.end), d))
        .collect();
    let mut declarations = BTreeMap::new();
    for link in &code.links {
        if let Some(definition) = definitions.get(&(link.start, link.end)) {
            declarations
                .entry(link.label.clone())
                .or_insert(*definition);
        }
    }
    // Raw disassembly declarations intentionally have no CodeLink. Their entire
    // definition span is the canonical DEX ID immediately after `.method `.
    let starts: Vec<_> = std::iter::once(0)
        .chain(
            chars
                .iter()
                .enumerate()
                .filter_map(|(i, c)| (*c == '\n').then_some(i + 1)),
        )
        .collect();
    for definition in definitions.values() {
        let begin = starts[starts.partition_point(|start| *start <= definition.start) - 1];
        let prefix: String = chars[begin..definition.start].iter().collect();
        if prefix.trim_start() == ".method " {
            let id: String = chars[definition.start..definition.end].iter().collect();
            declarations.entry(id).or_insert(*definition);
        }
    }
    declarations
}

/// Refresh declaration spans after source decoration has shifted Unicode offsets.
/// Status, provenance, and source-sugar accounting retain their original proof.
pub(crate) fn refresh_offsets(coverage: &mut Value, code: &DecompiledCode) {
    let chars: Vec<_> = code.source.chars().collect();
    let declarations = declaration_index(code, &chars);
    if let Some(methods) = coverage["methods"].as_array_mut() {
        for method in methods {
            let declaration = method["method_id"]
                .as_str()
                .and_then(|id| declarations.get(id));
            let offset = declaration.map(|d| json!({"start":d.start,"end":d.end,"unit":"unicode_scalar","end_exclusive":true}));
            method["declaration_offset"] = offset.unwrap_or(Value::Null);
        }
    }
}

pub(crate) fn report(
    name: &str,
    class: &DexClass,
    code: &DecompiledCode,
    errors: &BTreeMap<String, String>,
    folded: &BTreeSet<String>,
) -> Value {
    let chars: Vec<char> = code.source.chars().collect();
    let declarations = declaration_index(code, &chars);
    let line_starts: Vec<_> = std::iter::once(0)
        .chain(
            chars
                .iter()
                .enumerate()
                .filter_map(|(i, c)| (*c == '\n').then_some(i + 1)),
        )
        .collect();
    let mut methods = Vec::new();
    let mut counts = BTreeMap::from([("decompiled", 0usize), ("dex-fallback", 0), ("omitted", 0)]);
    for method in &class.methods {
        let id = method_id(name, method);
        let declaration = declarations.get(id.as_str()).copied();
        let (status, representation, reason) = if folded.contains(&id) {
            (
                "decompiled",
                "source_sugar",
                Some("proven_enum_sugar".to_owned()),
            )
        } else if let Some(d) = declaration {
            let begin = line_starts[line_starts.partition_point(|start| *start <= d.start) - 1];
            let prefix: String = chars[begin..d.start].iter().collect();
            if prefix.trim_start().starts_with(".method ") {
                (
                    "dex-fallback",
                    "dex",
                    Some(
                        errors
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| "source_reconstruction_unavailable".into()),
                    ),
                )
            } else {
                (
                    "decompiled",
                    if method.code.is_none() {
                        "declaration_only"
                    } else {
                        "java"
                    },
                    None,
                )
            }
        } else {
            (
                "omitted",
                "absent",
                Some(
                    errors
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| "no_standalone_declaration".into()),
                ),
            )
        };
        *counts.get_mut(status).unwrap() += 1;
        methods.push(json!({"method_id":id,"status":status,"representation":representation,"reason":reason,
            "declaration_offset":declaration.map(|d| json!({"start":d.start,"end":d.end,"unit":"unicode_scalar","end_exclusive":true}))}));
    }
    let omitted: Vec<_> = methods
        .iter()
        .filter(|m| m["status"] == "omitted")
        .map(|m| m["method_id"].clone())
        .collect();
    json!({"total_methods":methods.len(),"counts":counts,"methods":methods,"omitted_methods":omitted,"source_complete":counts["omitted"] == 0,"java_complete":counts["omitted"] == 0 && counts["dex-fallback"] == 0,"boundary":"declaration_presence_only; does_not_prove_semantic_correctness"})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CodeLink;
    fn class() -> DexClass {
        crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0)
    }
    fn method(parameter: &str) -> DexMethod {
        DexMethod {
            declaring_type: "Lsample/Hello;".into(),
            name: "same".into(),
            return_type: "V".into(),
            parameters: if parameter.is_empty() {
                vec![]
            } else {
                vec![parameter.into()]
            },
            thrown_types: vec![],
            access_flags: 0x101,
            code: None,
        }
    }
    #[test]
    fn refresh_offsets_tracks_unicode_decoration_and_preserves_status() {
        let mut class = class();
        class.methods = vec![method("")];
        let id = method_id("sample.Hello", &class.methods[0]);
        let mut code = DecompiledCode {
            source: "void same();".into(),
            source_hash: String::new(),
            links: vec![CodeLink {
                start: 5,
                end: 9,
                label: id,
            }],
            definitions: vec![CodeDefinition {
                start: 5,
                end: 9,
                kind: "method".into(),
                name: "same".into(),
            }],
        };
        let mut coverage = report(
            "sample.Hello",
            &class,
            &code,
            &BTreeMap::new(),
            &BTreeSet::new(),
        );
        code.source.insert_str(0, "// é🙂\n");
        code.links[0].start += 6;
        code.links[0].end += 6;
        code.definitions[0].start += 6;
        code.definitions[0].end += 6;
        refresh_offsets(&mut coverage, &code);
        assert_eq!(coverage["methods"][0]["declaration_offset"]["start"], 11);
        assert_eq!(coverage["methods"][0]["declaration_offset"]["end"], 15);
        assert_eq!(coverage["methods"][0]["status"], "decompiled");
        assert_eq!(coverage["source_complete"], true);
    }

    #[test]
    fn overload_identity_unicode_offsets_fallback_and_omission() {
        let mut class = class();
        class.methods = vec![method(""), method("I"), method("J"), method("D")];
        let mut code = DecompiledCode {
            source: "// é🙂\nvoid same();\n.method sample.Hello.same(I)V\n".into(),
            source_hash: String::new(),
            links: vec![],
            definitions: vec![],
        };
        for (index, text) in ["same();", "sample.Hello.same(I)V"].iter().enumerate() {
            let byte = code.source.find(text).unwrap();
            let start = code.source[..byte].chars().count();
            let end = start + if index == 0 { 4 } else { text.chars().count() };
            code.definitions.push(CodeDefinition {
                start,
                end,
                kind: "method".into(),
                name: "same".into(),
            });
            if index == 0 {
                code.links.push(CodeLink {
                    start,
                    end,
                    label: method_id("sample.Hello", &class.methods[index]),
                });
            }
        }
        let errors = BTreeMap::from([(
            method_id("sample.Hello", &class.methods[1]),
            "unsupported opcode example".into(),
        )]);
        let folded = BTreeSet::from([method_id("sample.Hello", &class.methods[3])]);
        let result = report("sample.Hello", &class, &code, &errors, &folded);
        assert_eq!(
            result["counts"],
            json!({"decompiled":2,"dex-fallback":1,"omitted":1})
        );
        assert_eq!(result["methods"][0]["representation"], "declaration_only");
        assert_eq!(result["methods"][0]["declaration_offset"]["start"], 11);
        assert_eq!(result["methods"][1]["reason"], "unsupported opcode example");
        assert_eq!(result["omitted_methods"], json!(["sample.Hello.same(J)V"]));
        assert_eq!(result["methods"][3]["representation"], "source_sugar");
        assert_eq!(result["source_complete"], false);
    }
}
