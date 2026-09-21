//! Opt-in regression for the user-provided APK. No external decompiler/runtime.
use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
#[ignore = "Set RDX_TEST_APK to the pinned Zoom APK and run --ignored"]
fn integration_activity_fields_and_exception_method_have_exact_native_mappings() {
    let path = std::env::var_os("RDX_TEST_APK").expect("RDX_TEST_APK required");
    let name = "com.zipow.videobox.IntegrationActivity";
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let code = engine.decompile_with_metadata(name).unwrap();
    assert!(!code.source.contains("// Reconstructed Java field"));
    assert!(!code.source.contains("// Reconstructed Java method"));
    for method_name in [
        "getIMActivity",
        "handleActionCCIMainPage",
        "handleActionCCIIncomingCall",
        "handleActionInputProxyNamePass",
        "handleActionPBXNewSMSFromSchema",
    ] {
        let definition = code
            .definitions
            .iter()
            .find(|d| d.kind == "method" && d.name == method_name)
            .unwrap();
        let end = code
            .definitions
            .iter()
            .filter(|d| d.kind == "method" && d.start > definition.start)
            .map(|d| d.start)
            .min()
            .unwrap_or(code.source.chars().count());
        let member: String = code
            .source
            .chars()
            .skip(definition.start)
            .take(end - definition.start)
            .collect();
        assert!(
            !member.contains(".end method"),
            "{method_name} still uses DEX:\n{member}"
        );
        if method_name == "handleActionInputProxyNamePass" {
            assert!(member.contains("!("), "boolean inversion missing: {member}");
        }
        if method_name == "handleActionPBXNewSMSFromSchema" {
            assert!(
                member.contains(".parseFrom("),
                "parser call missing: {member}"
            );
            assert!(
                member.contains("catch (Exception "),
                "catch missing: {member}"
            );
            assert!(
                member.contains("return true;"),
                "boolean return missing: {member}"
            );
        }
        assert_eq!(
            code.source
                .chars()
                .skip(definition.start)
                .take(definition.end - definition.start)
                .collect::<String>(),
            method_name
        );
    }
    for (field, value) in [
        ("ARG_ENGAGEMENT_ID", "args_engagement_id"),
        ("ARG_ERROR_CONFIRM_MSG", "errorConfirmMsg"),
    ] {
        assert!(code.source.contains(&format!(
            "public static final String {field} = \"{value}\";"
        )));
        let declaration = code
            .definitions
            .iter()
            .find(|d| d.kind == "field" && d.name == field)
            .unwrap();
        assert_eq!(
            code.source
                .chars()
                .skip(declaration.start)
                .take(declaration.end - declaration.start)
                .collect::<String>(),
            field
        );
        let symbol = engine
            .resolve_usage_target(name, declaration.start, &code.source_hash)
            .unwrap();
        assert_eq!(symbol.id, format!("{name}.{field}:Ljava/lang/String;"));
    }
    let method = code
        .definitions
        .iter()
        .find(|d| d.kind == "method" && d.name == "acceptNewIncomingCall")
        .unwrap();
    let end = code
        .definitions
        .iter()
        .filter(|d| d.kind == "method" && d.start > method.start)
        .map(|d| d.start)
        .min()
        .unwrap_or(code.source.chars().count());
    let body: String = code
        .source
        .chars()
        .skip(method.start)
        .take(end - method.start)
        .collect();
    assert!(body.contains("try {"), "{body}");
    assert!(body.contains("catch (Exception "), "{body}");
    assert!(body.contains("new Intent("), "{body}");
    assert!(body.contains(".toByteArray()"), "{body}");
    assert!(!body.contains(".end method"), "{body}");
    assert!(
        !body
            .lines()
            .any(|line| line.trim_start().starts_with("java.lang.Object v")),
        "dead exception/branch merge variables should not be emitted: {body}"
    );
    let class_literal = code
        .links
        .iter()
        .find(|l| {
            l.label == name
                && l.start > method.start
                && l.start < end
                && code.source.chars().skip(l.end).take(6).collect::<String>() == ".class"
        })
        .expect("exact class-literal reference inside constructor");
    let destination = engine
        .navigate(name, class_literal.start, &code.source_hash)
        .unwrap();
    assert_eq!(destination.class, name);
    assert_eq!(destination.code.source_hash, code.source_hash);
}
