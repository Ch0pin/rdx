//! Opt-in reconstruction/navigation checks for the reported Expedia Kotlin interface.
use rdx::engine::{DecompilerEngine, NativeEngine};

#[test]
#[ignore = "Set RDX_EXPEDIA_APK and run --ignored"]
fn kotlin_default_methods_and_interface_header_keep_exact_navigation() {
    let path = std::env::var_os("RDX_EXPEDIA_APK").expect("RDX_EXPEDIA_APK required");
    let name = "com.expedia.bookings.androidcommon.config.ProductFlavourFeatureConfig";
    let mut engine = NativeEngine::start().unwrap();
    engine.open(std::path::Path::new(&path)).unwrap();
    let code = engine.decompile_with_metadata(name).unwrap();
    assert!(
        code.source.contains(
            "public interface ProductFlavourFeatureConfig extends BaseFeatureConfigurationInterface"
        ),
        "{}",
        code.source
    );
    for banner in [
        "Native mixed Java / DEX",
        "Strings preserve lone UTF-16",
        "Offsets are DEX code units",
        ".class ",
        ".super ",
        ".implements ",
    ] {
        assert!(
            !code.source.contains(banner),
            "old display remains: {banner}"
        );
    }
    for method_name in ["getPdpKeyComponents$default", "getSrpKeyComponents$default"] {
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
            "{method_name} still DEX: {member}"
        );
        assert!(
            member.contains("UnsupportedOperationException"),
            "missing marker rejection: {member}"
        );
        assert!(member.contains("Super calls with default arguments not supported in this target"));
        assert!(member.contains("throw "));
        assert_eq!(
            code.source
                .chars()
                .skip(definition.start)
                .take(definition.end - definition.start)
                .collect::<String>(),
            method_name
        );
        let target = engine
            .resolve_usage_target(name, definition.start, &code.source_hash)
            .unwrap();
        assert!(
            target.id.starts_with(&format!("{name}.{method_name}(")),
            "{}",
            target.id
        );
        let destination = engine
            .navigate(name, definition.start, &code.source_hash)
            .unwrap();
        assert_eq!(destination.class, name);
        assert_eq!(destination.code.source_hash, code.source_hash);
    }
    let phone = "com.expedia.bookings.launch.PhoneLaunchActivity";
    let code = engine.decompile_with_metadata(phone).unwrap();
    assert!(
        !code.source.contains("\n.field "),
        "DEX field presentation remains"
    );
    let init = code
        .definitions
        .iter()
        .find(|d| d.kind == "method" && d.name == "<clinit>")
        .unwrap();
    assert_eq!(
        code.source
            .chars()
            .skip(init.start)
            .take(init.end - init.start)
            .collect::<String>(),
        "static"
    );
    let end = code
        .definitions
        .iter()
        .filter(|d| d.kind == "method" && d.start > init.start)
        .map(|d| d.start)
        .min()
        .unwrap_or(code.source.chars().count());
    let body: String = code
        .source
        .chars()
        .skip(init.start)
        .take(end - init.start)
        .collect();
    assert!(
        !body.contains(".end method"),
        "initializer remains DEX: {body}"
    );
    assert!(body.contains("PropertyReference1Impl"));
    assert!(body.contains("\"rootLayout\""));
    assert!(body.contains("$$delegatedProperties"));
    let literal = code
        .links
        .iter()
        .find(|link| {
            link.label == phone
                && link.start > init.start
                && link.start < end
                && code
                    .source
                    .chars()
                    .skip(link.end)
                    .take(6)
                    .collect::<String>()
                    == ".class"
        })
        .unwrap();
    assert_eq!(
        engine
            .navigate(phone, literal.start, &code.source_hash)
            .unwrap()
            .class,
        phone
    );
    for name in ["$$delegatedProperties", "Companion", "$stable"] {
        let field = code
            .definitions
            .iter()
            .find(|d| d.kind == "field" && d.name == name)
            .unwrap();
        assert_eq!(
            code.source
                .chars()
                .skip(field.start)
                .take(field.end - field.start)
                .collect::<String>(),
            name
        );
        assert!(
            engine
                .resolve_usage_target(phone, field.start, &code.source_hash)
                .unwrap()
                .id
                .starts_with(&format!("{phone}.{name}:"))
        );
    }
}
