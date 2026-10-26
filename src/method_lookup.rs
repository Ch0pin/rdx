//! Exact source-method lookup with bounded, declaration-backed suggestions.
use crate::{
    native_dex::{DexClass, DexMethod},
    native_engine::disassembly::method_id,
};
use anyhow::{Context, Result, ensure};

pub(crate) const EXAMPLE: &str = "pkg.Class.method(Ljava/lang/String;)V";
const SMALI_EXAMPLE: &str = "Lpkg/Class;->method(Ljava/lang/String;)V";

// Use the DEX 040+ SimpleNameChar grammar, including Zs spaces, rather than
// Java identifier rules. Selector identity must preserve legal obfuscation.
// https://source.android.com/docs/core/runtime/dex-format#string-syntax
fn simple_name(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            matches!(character,
                'A'..='Z' | 'a'..='z' | '0'..='9' | ' ' | '$' | '-' | '_'
                | '\u{00a0}'..='\u{1fff}' | '\u{2000}'..='\u{200a}'
                | '\u{2010}'..='\u{2027}' | '\u{202f}'..='\u{d7ff}'
                | '\u{e000}'..='\u{ffef}' | '\u{10000}'..='\u{10ffff}')
        })
}

fn owner_path(value: &str, separator: char) -> bool {
    value.split(separator).all(simple_name)
}

fn member_name(value: &str) -> bool {
    let inner = value
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'));
    simple_name(inner.unwrap_or(value))
}

/// Consume one descriptor without recursion or declaration lookup.
fn descriptor_end(value: &str, start: usize, allow_void: bool) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut at = start;
    while bytes.get(at) == Some(&b'[') {
        at += 1;
        if at - start > 255 {
            return None;
        }
    }
    match *bytes.get(at)? {
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' => Some(at + 1),
        b'V' if allow_void && at == start => Some(at + 1),
        b'L' => {
            let begin = at + 1;
            let end = begin + value[begin..].find(';')?;
            owner_path(&value[begin..end], '/').then_some(end + 1)
        }
        _ => None,
    }
}

pub(crate) fn normalize(id: &str) -> Result<String> {
    ensure!(
        !id.is_empty() && id.len() <= 4096,
        "Method ID must contain 1..4096 bytes"
    );
    let example = format!(
        "INVALID_METHOD_SELECTOR: expected {EXAMPLE} or {SMALI_EXAMPLE}; optional ':' is accepted only immediately before '('"
    );
    let normalized = if let Some((owner, member)) = id.split_once("->") {
        let owner = owner
            .strip_prefix('L')
            .and_then(|s| s.strip_suffix(';'))
            .filter(|s| owner_path(s, '/'))
            .with_context(|| example.clone())?;
        format!("{}.{}", owner.replace('/', "."), member)
    } else {
        id.to_owned()
    };
    let open = normalized.find('(');
    let head = open.map_or(normalized.as_str(), |open| &normalized[..open]);
    let head = if open.is_some() {
        head.strip_suffix(':').unwrap_or(head)
    } else {
        head
    };
    let (owner, member) = head.rsplit_once('.').with_context(|| example.clone())?;
    ensure!(owner_path(owner, '.') && member_name(member), "{example}");
    let mut canonical = format!("{owner}.{member}");
    if let Some(open) = open {
        let tail = &normalized[open + 1..];
        let close = tail.find(')').with_context(|| example.clone())?;
        let parameters = &tail[..close];
        let mut at = 0;
        while at < parameters.len() {
            at = descriptor_end(parameters, at, false).with_context(|| example.clone())?;
        }
        let returned = &tail[close + 1..];
        // Missing return descriptors keep the established same-name suggestion
        // path. Complete external signatures remain exact, without lookup.
        ensure!(
            returned.is_empty() || descriptor_end(returned, 0, true) == Some(returned.len()),
            "{example}"
        );
        canonical.push('(');
        canonical.push_str(parameters);
        canonical.push(')');
        canonical.push_str(returned);
    }
    Ok(canonical)
}

pub(crate) fn owner(id: &str) -> Result<&str> {
    let head = id.split_once('(').map_or(id, |(head, _)| head);
    let (owner, name) = head
        .rsplit_once('.')
        .with_context(|| format!("Expected method ID, e.g. {EXAMPLE}"))?;
    ensure!(
        !owner.is_empty() && !name.is_empty(),
        "Expected method ID, e.g. {EXAMPLE}"
    );
    Ok(owner)
}

/// Call graphs keep exact external/inherited selectors intact. Incomplete
/// selectors receive declaration-backed choices and never select an overload.
pub(crate) fn graph_selector<'a>(
    requested: &str,
    class_for: impl Fn(&str) -> Option<&'a DexClass>,
) -> Result<String> {
    let id = normalize(requested)?;
    let owner_name = owner(&id)?;
    if id
        .split_once('(')
        .is_some_and(|(_, tail)| tail.split_once(')').is_some_and(|(_, ret)| !ret.is_empty()))
    {
        return Ok(id);
    }
    let head = id.split_once('(').map_or(id.as_str(), |(head, _)| head);
    let name = head.rsplit('.').next().unwrap();
    let mut current = owner_name.to_owned();
    let mut seen = std::collections::HashSet::new();
    let mut candidates = std::collections::BTreeSet::new();
    let mut total = 0usize;
    while seen.len() < 256 && seen.insert(current.clone()) {
        let Some(class) = class_for(&current) else {
            break;
        };
        for method in &class.methods {
            if method.name.as_ref() == name {
                total += 1;
                candidates.insert(method_id(method));
                if candidates.len() > 20 {
                    candidates.pop_last();
                }
            }
        }
        if matches!(name, "<init>" | "<clinit>") {
            break;
        }
        let Some(parent) = &class.superclass else {
            break;
        };
        current = crate::native_engine::disassembly::type_name(parent);
    }
    let mut bytes = 0;
    let candidates: Vec<_> = candidates
        .into_iter()
        .take_while(|candidate| {
            bytes += candidate.len();
            bytes <= 16384
        })
        .collect();
    let detail = serde_json::json!({"candidates": candidates, "candidate_count": total,
        "candidates_truncated": candidates.len() < total,
        "scope":"same-name declarations in requested class and known superclass chain (up to 256 types)",
        "hint":"Retry with an exact candidate including parameter and return descriptors; no overload was selected"});
    anyhow::bail!(
        "Expected exact DEX method signature, e.g. {EXAMPLE} or {SMALI_EXAMPLE}; include parameter descriptors in parentheses and a return descriptor after ')' (V for void); {detail}"
    )
}

pub(crate) fn resolve<'a>(class: &'a DexClass, id: &str) -> Result<&'a DexMethod> {
    let mut matches = class.methods.iter().filter(|m| method_id(m) == id);
    if let Some(method) = matches.next() {
        ensure!(matches.next().is_none(), "AMBIGUOUS_METHOD: {id}");
        return Ok(method);
    }
    let head = id.split_once('(').map_or(id, |(head, _)| head);
    let name = head.rsplit_once('.').map_or(head, |(_, name)| name);
    let parameters = id
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(')').map(|(args, _)| args));
    let mut candidates: Vec<_> = class
        .methods
        .iter()
        .filter(|m| m.name.as_ref() == name)
        .map(|m| {
            (
                parameters.is_none_or(|p| p != m.parameters.join("")),
                method_id(m),
            )
        })
        .collect();
    candidates.sort();
    candidates.dedup();
    let total = candidates.len();
    let mut bytes = 0;
    let suggestions: Vec<_> = candidates
        .into_iter()
        .take(20)
        .map(|(_, id)| id)
        .take_while(|id| {
            bytes += id.len();
            bytes <= 16384
        })
        .collect();
    let detail = serde_json::json!({"candidates":suggestions,"candidate_count":total,
        "candidates_truncated":suggestions.len()<total,"scope":"same-name declarations in requested class",
        "hint":"Use an exact candidate, including return type; get_methods_of_class lists all declarations"});
    anyhow::bail!("SYMBOL_NOT_FOUND: method_id={id}; expected e.g. {EXAMPLE}; {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn method(parameter: &str, ret: &str) -> DexMethod {
        DexMethod {
            declaring_type: "Lsample/Owner;".into(),
            name: "run".into(),
            parameters: vec![parameter.into()],
            return_type: ret.into(),
            thrown_types: vec![],
            access_flags: 9,
            code: None,
        }
    }
    fn fixture() -> DexClass {
        let mut c = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        c.methods = vec![method("Ljava/lang/String;", "I"), method("I", "V")];
        c
    }
    #[test]
    fn colon_before_signature_normalizes_constructors_and_smali_members_exactly() {
        for (requested, expected) in [
            ("sample.Target.<init>:()V", "sample.Target.<init>()V"),
            ("Lsample/Target;-><init>:()V", "sample.Target.<init>()V"),
            ("sample.Target.<clinit>:()V", "sample.Target.<clinit>()V"),
            ("sample.Owner.run:(I)V", "sample.Owner.run(I)V"),
            ("Lsample/Owner;->run:(I)V", "sample.Owner.run(I)V"),
        ] {
            assert_eq!(normalize(requested).unwrap(), expected);
            assert_eq!(
                graph_selector(requested, |_| panic!(
                    "exact selectors must not require declaration lookup"
                ))
                .unwrap(),
                expected
            );
        }
        let c = fixture();
        let canonical = normalize("sample.Owner.run:(I)V").unwrap();
        assert_eq!(resolve(&c, &canonical).unwrap().return_type.as_ref(), "V");
        let wrong = normalize("sample.Owner.run:(I)Z").unwrap();
        let error = resolve(&c, &wrong).unwrap_err().to_string();
        assert!(error.contains("sample.Owner.run(I)V"));
        assert!(error.contains("SYMBOL_NOT_FOUND"));
    }

    #[test]
    fn complete_external_descriptors_and_unicode_dollar_names_keep_exact_identity() {
        for id in [
            "external.Api.run([[ILjava/lang/String;)[[Ljava/lang/Object;",
            "external.Σ$Owner.δ$method([BJSZ)D",
            "external.Api.run()V",
            "external.Api.run()F",
        ] {
            assert_eq!(normalize(id).unwrap(), id);
            assert_eq!(
                graph_selector(id, |_| panic!("external target lookup")).unwrap(),
                id
            );
        }
        assert_eq!(
            normalize("Lexternal/Σ$Owner;->δ$method:([BJSZ)D").unwrap(),
            "external.Σ$Owner.δ$method([BJSZ)D"
        );
        let array = format!("external.Api.run({}I)V", "[".repeat(255));
        assert_eq!(normalize(&array).unwrap(), array);
    }

    #[test]
    fn dex_040_spaces_and_angle_wrapped_members_keep_their_exact_spelling() {
        for id in [
            " sample.Owner.run()V",
            "sample.Owner.run ()V",
            "sample.Owner. ()V",
            "sample.Owner.<custom>()V",
            "sample.Owner.<custom name-1>()V",
            "sample.\u{00a0}Owner.method\u{00a0}name(Larg/ Space;)[Lret/N\u{2000};",
        ] {
            assert_eq!(normalize(id).unwrap(), id);
            assert_eq!(
                graph_selector(id, |_| panic!("exact spaced external target lookup")).unwrap(),
                id
            );
        }
        assert_eq!(
            normalize("Lsample/ Owner;-><custom\u{00a0}name>:()V").unwrap(),
            "sample. Owner.<custom\u{00a0}name>()V"
        );
        for character in [
            ' ',
            '\u{00a0}',
            '\u{00a1}',
            '\u{1fff}',
            '\u{2000}',
            '\u{200a}',
            '\u{2010}',
            '\u{2027}',
            '\u{202f}',
            '\u{2030}',
            '\u{d7ff}',
            '\u{e000}',
            '\u{ffef}',
            '\u{10000}',
            '\u{10ffff}',
        ] {
            assert!(
                normalize(&format!(
                    "sample.N{character}.m{character}(Lpkg/N{character};)V"
                ))
                .is_ok(),
                "U+{:04X}",
                character as u32
            );
        }
    }

    #[test]
    fn dex_name_grammar_rejects_controls_reserved_characters_and_invalid_angle_forms() {
        for character in [
            '\0', '\t', '\n', '\r', '\u{007f}', '\u{0085}', '\u{009f}', '\u{200b}', '\u{200f}',
            '\u{2028}', '\u{202e}', '\u{fff0}', '\u{ffff}', '!', ',', ':', '\\',
        ] {
            assert!(
                normalize(&format!("sample.Owner.m{character}n()V")).is_err(),
                "member U+{:04X}",
                character as u32
            );
            assert!(
                normalize(&format!("sample.Owner.m(Lpkg/N{character};)V")).is_err(),
                "type U+{:04X}",
                character as u32
            );
        }
        for member in ["<>", "<custom", "custom>", "<<custom>>", "<custom.name>"] {
            assert!(
                normalize(&format!("sample.Owner.{member}()V")).is_err(),
                "{member}"
            );
        }
    }

    #[test]
    fn custom_angle_member_suggestions_follow_the_known_superclass_chain() {
        let mut parent = fixture();
        parent.descriptor = "Lsample/Parent;".into();
        parent.methods = vec![method("I", "V")];
        parent.methods[0].name = "<custom>".into();
        parent.methods[0].declaring_type = parent.descriptor.clone();
        let mut child = fixture();
        child.superclass = Some(parent.descriptor.clone());
        let error = graph_selector("sample.Owner.<custom>", |name| match name {
            "sample.Owner" => Some(&child),
            "sample.Parent" => Some(&parent),
            _ => None,
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("sample.Parent.<custom>(I)V"));
        assert!(error.contains("no overload was selected"));
    }

    #[test]
    fn malformed_selectors_fail_with_actionable_examples_before_graph_lookup() {
        for id in [
            "run(I)V",
            "sample..Owner.run()V",
            "sample/Owner.run()V",
            "Lsample//Owner;->run()V",
            "Lsample.Owner;->run()V",
            "sample/Owner->run()V",
            "sample.Owner.run::()V",
            "sample.Owner.run:V",
            "sample.Owner.run()V:",
            "sample.Owner.run(I",
            "sample.Owner.runI)V",
            "sample.Owner.run((I)V",
            "sample.Owner.run(I))V",
            "sample.Owner.run()Vextra",
            "sample.Owner.run()",
            "sample.Owner.run(V)V",
            "sample.Owner.run([V)V",
            "sample.Owner.run()[V",
            "sample.Owner.run()VV",
            "sample.Owner.run()L;",
            "sample.Owner.run()Ljava.lang.String;",
            "sample.Owner.run(Ljava/lang/String)V",
            "sample.Owner.run(Lfoo//Bar;)V",
            "sample.Owner.run()Ljava/lang/String",
            "sample.Owner.run()I(I)V",
            "sample.Owner.run( I)V",
            "sample.Owner.run()V\n",
            "Lsample/Owner;->run->other()V",
        ] {
            // A complete parameter list without a return type intentionally
            // follows the existing declaration-backed suggestion route.
            if id == "sample.Owner.run()" {
                assert_eq!(normalize(id).unwrap(), id);
                let error = graph_selector(id, |_| None).unwrap_err().to_string();
                assert!(error.contains(EXAMPLE));
                continue;
            }
            let error = graph_selector(id, |_| panic!("malformed target lookup"))
                .unwrap_err()
                .to_string();
            assert!(error.contains("INVALID_METHOD_SELECTOR"), "{id}: {error}");
            assert!(error.contains(EXAMPLE), "{id}: {error}");
        }
        assert!(normalize("").is_err());
        assert!(normalize(&"x".repeat(4097)).is_err());
        assert!(normalize(&format!("sample.Owner.run({}I)V", "[".repeat(256))).is_err());
    }

    #[test]
    fn incomplete_colon_signature_keeps_overload_suggestions_without_selection() {
        let c = fixture();
        let error = graph_selector("sample.Owner.run:(I)", |name| {
            (name == "sample.Owner").then_some(&c)
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("sample.Owner.run(I)V"));
        assert!(error.contains("sample.Owner.run(Ljava/lang/String;)I"));
        assert!(error.contains("no overload was selected"));
    }
    #[test]
    fn graph_names_offer_overloads_without_selecting_and_keep_external_targets() {
        let c = fixture();
        let lookup = |name: &str| (name == "sample.Owner").then_some(&c);
        for id in [
            "sample.Owner.run",
            "Lsample/Owner;->run",
            "sample.Owner.run(I)",
        ] {
            let error = graph_selector(id, lookup).unwrap_err().to_string();
            assert!(error.contains("sample.Owner.run(I)V"), "{error}");
            assert!(error.contains("sample.Owner.run(Ljava/lang/String;)I"));
            assert!(error.contains("no overload was selected"));
        }
        assert_eq!(
            graph_selector("Lexternal/Api;->run(I)V", lookup).unwrap(),
            "external.Api.run(I)V"
        );
        assert_eq!(
            graph_selector("sample.Owner.run(I)V", lookup).unwrap(),
            "sample.Owner.run(I)V"
        );
    }

    #[test]
    fn graph_constructor_candidates_do_not_include_superclass_constructors() {
        let mut parent = fixture();
        parent.descriptor = "Lsample/Parent;".into();
        parent.methods = vec![method("I", "V")];
        parent.methods[0].name = "<init>".into();
        parent.methods[0].declaring_type = parent.descriptor.clone();
        let mut child = fixture();
        child.superclass = Some(parent.descriptor.clone());
        child.methods[0].name = "<init>".into();
        child.methods[0].return_type = "V".into();
        let error = graph_selector("sample.Owner.<init>", |name| match name {
            "sample.Owner" => Some(&child),
            "sample.Parent" => Some(&parent),
            _ => None,
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("sample.Owner.<init>(Ljava/lang/String;)V"));
        assert!(!error.contains("sample.Parent.<init>"));
    }

    #[test]
    fn wrong_return_ranks_exact_parameters_and_never_resolves() {
        let c = fixture();
        let error = resolve(&c, "sample.Owner.run(I)Z").unwrap_err().to_string();
        assert!(
            error.find("sample.Owner.run(I)V").unwrap()
                < error.find("sample.Owner.run(Ljava/lang/String;)I").unwrap()
        );
        assert!(error.contains("\"candidate_count\":2"));
        assert_eq!(
            resolve(&c, "sample.Owner.run(I)V")
                .unwrap()
                .return_type
                .as_ref(),
            "V"
        );
    }
    #[test]
    fn missing_descriptor_smali_and_unknown_names_are_explicit() {
        let c = fixture();
        let id = normalize("Lsample/Owner;->run").unwrap();
        assert_eq!(owner(&id).unwrap(), "sample.Owner");
        assert!(
            resolve(&c, &id)
                .unwrap_err()
                .to_string()
                .contains("sample.Owner.run(I)V")
        );
        assert!(
            resolve(&c, "sample.Owner.missing()V")
                .unwrap_err()
                .to_string()
                .contains("\"candidates\":[]")
        );
        assert!(normalize("sample/Owner->run()V").is_err());
    }
    #[test]
    fn suggestions_are_bounded_and_duplicates_are_not_accepted() {
        let mut c = fixture();
        c.methods = (0..30)
            .map(|i| method(&format!("Lsample/T{i};"), "I"))
            .collect();
        let error = resolve(&c, "sample.Owner.run()V").unwrap_err().to_string();
        assert!(error.contains("\"candidate_count\":30"));
        assert!(error.contains("\"candidates_truncated\":true"));
        c.methods = vec![
            method("Ljava/lang/String;", "I"),
            method("Ljava/lang/String;", "I"),
        ];
        assert!(
            resolve(&c, "sample.Owner.run(Ljava/lang/String;)I")
                .unwrap_err()
                .to_string()
                .contains("AMBIGUOUS_METHOD")
        );
    }
}
