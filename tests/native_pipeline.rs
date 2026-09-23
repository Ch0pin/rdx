use rdx::engine::{DecompilerEngine, NativeEngine};
use std::path::Path;
fn engine() -> NativeEngine {
    let mut engine = NativeEngine::start().unwrap();
    engine
        .open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"))
        .unwrap();
    engine
}
#[test]
fn native_overloaded_navigation_and_usages_use_exact_symbols() {
    let mut engine = engine();
    let code = engine.decompile_with_metadata("sample.Caller").unwrap();
    for id in [
        "sample.Target.doubleValue(I)I",
        "sample.Target.doubleValue(Ljava/lang/String;)Ljava/lang/String;",
        "sample.Target.value:I",
    ] {
        let link = code.links.iter().find(|link| link.label == id).unwrap();
        let target = engine
            .navigate("sample.Caller", link.start, &code.source_hash)
            .unwrap();
        assert_eq!(target.class, "sample.Target");
        // Rendered text can be a Java identifier or a disassembly symbol;
        // navigation identity must stay exact, including overloaded signatures.
        let declaration = target
            .code
            .definitions
            .iter()
            .find(|definition| definition.start == target.position)
            .unwrap();
        let text: String = target
            .code
            .source
            .chars()
            .skip(declaration.start)
            .take(declaration.end - declaration.start)
            .collect();
        assert!(text == id || text == declaration.name, "{text}");
        let resolved = engine
            .resolve_usage_target("sample.Target", target.position, &target.code.source_hash)
            .unwrap();
        assert_eq!(resolved.id, id);
        let usages = engine
            .resolve_usage_target("sample.Caller", link.start, &code.source_hash)
            .unwrap();
        assert_eq!(usages.id, id);
        let occurrences = engine.usages_in_class(&usages.id, "sample.Caller").unwrap();
        assert!(!occurrences.occurrences.is_empty());
        assert!(
            engine
                .navigate("sample.Caller", link.start, "stale")
                .is_err()
        );
    }
}
#[test]
fn native_declaration_navigation_and_resources() {
    let mut engine = engine();
    let code = engine.decompile_with_metadata("sample.Target").unwrap();
    let method = code
        .definitions
        .iter()
        .find(|d| d.kind == "method" && d.name == "doubleValue")
        .unwrap();
    let destination = engine
        .navigate("sample.Target", method.start, &code.source_hash)
        .unwrap();
    assert_eq!(destination.position, method.start);
    assert!(
        engine
            .read_resource("AndroidManifest.xml")
            .unwrap()
            .contains("android:targetActivity")
    );
    assert!(engine.read_resource("missing.xml").is_err());
}

#[test]
fn mixed_java_and_dex_keep_one_definition_and_exact_usage_per_instruction() {
    let mut engine = engine();
    let code = engine.decompile_with_metadata("sample.Caller").unwrap();
    assert!(code.source.starts_with("package sample;"));
    assert!(code.source.contains("public class Caller {"));
    assert!(code.source.ends_with("}\n"));
    assert!(code.source.contains("public static int compute()"));
    assert!(code.source.contains("public static Class targetType()"));
    assert!(code.source.contains("Target.class"));
    let class_literal = code
        .links
        .iter()
        .find(|link| {
            link.label == "sample.Target"
                && code
                    .source
                    .chars()
                    .skip(link.end)
                    .take(6)
                    .collect::<String>()
                    == ".class"
        })
        .unwrap();
    let destination = engine
        .navigate("sample.Caller", class_literal.start, &code.source_hash)
        .unwrap();
    assert_eq!(destination.class, "sample.Target");
    assert!(code.source.contains("Ελληνικά 🦀"));
    let dex = rdx::native_dex::parse(include_bytes!("fixtures/navigation.dex")).unwrap();
    let class = dex
        .classes
        .iter()
        .find(|c| c.descriptor.as_ref() == "Lsample/Caller;")
        .unwrap();
    assert_eq!(
        code.definitions
            .iter()
            .filter(|d| d.kind == "method")
            .count(),
        class.methods.len()
    );
    for method in &class.methods {
        assert_eq!(
            code.definitions
                .iter()
                .filter(|d| d.kind == "method" && d.name == method.name.as_ref())
                .count(),
            1
        );
    }
    let char_count = code.source.chars().count();
    for (start, end) in code
        .links
        .iter()
        .map(|l| (l.start, l.end))
        .chain(code.definitions.iter().map(|d| (d.start, d.end)))
    {
        assert!(
            start < end && end <= char_count,
            "invalid source mapping {start}..{end}"
        );
    }
    for id in [
        "sample.Target.doubleValue(I)I",
        "sample.Target.doubleValue(Ljava/lang/String;)Ljava/lang/String;",
        "sample.Target.value:I",
    ] {
        let links: Vec<_> = code.links.iter().filter(|l| l.label == id).collect();
        assert_eq!(links.len(), 1, "duplicate or missing reference {id}");
        let link = links[0];
        let text: String = code
            .source
            .chars()
            .skip(link.start)
            .take(link.end - link.start)
            .collect();
        assert_eq!(
            text,
            if id.ends_with(":I") {
                "value"
            } else {
                "doubleValue"
            }
        );
        let usages = engine.usages_in_class(id, "sample.Caller").unwrap();
        assert_eq!(usages.occurrences.len(), 1, "duplicate usage {id}");
        assert_eq!(
            (usages.occurrences[0].start, usages.occurrences[0].end),
            (link.start, link.end)
        );
    }
    assert_eq!(code.source_hash, rdx::engine::source_identity(&code.source));
}
