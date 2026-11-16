//! Explicit opt-in: run after building the optional release helper.
use std::{collections::BTreeMap, process::Command};

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[ignore = "requires the separately built release RadGuard helper"]
fn bundled_helper_is_discovered_without_environment_override() {
    const STAGED: &str = "RDX_TEST_BUNDLED_RADGUARD";
    if std::env::var_os(STAGED).is_none() {
        let directory = tempfile::tempdir().unwrap();
        let harness = directory.path().join("radguard-integration");
        let helper = directory.path().join("rdx-radguard-helper");
        std::fs::copy(std::env::current_exe().unwrap(), &harness).unwrap();
        std::fs::copy(
            std::env::var_os("RDX_RADGUARD_HELPER").expect("set helper for staging"),
            &helper,
        )
        .unwrap();
        let output = Command::new(harness)
            .args([
                "--exact",
                "bundled_helper_is_discovered_without_environment_override",
                "--ignored",
                "--nocapture",
            ])
            .env_remove("RDX_RADGUARD_HELPER")
            .env(STAGED, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(std::env::var_os("RDX_RADGUARD_HELPER").is_none());
    let expected = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("rdx-radguard-helper");
    assert_eq!(
        rdx::alternate_source::helper_path_for(rdx::alternate_source::FallbackEngine::RadGuard),
        Some(expected.clone())
    );
    assert!(
        rdx::alternate_source::availability_for(rdx::alternate_source::FallbackEngine::RadGuard)
            .contains("is available")
    );
    let output = Command::new(expected)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/hello.dex"
        ))
        .arg("sample.Hello")
        .output()
        .unwrap();
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["backend"], rdx::alternate_source::RADGUARD_BACKEND);
    assert_eq!(response["methods"][0]["id"], "sample.Hello.answer()I");
    assert!(
        response["methods"][0]["java"]
            .as_str()
            .unwrap()
            .contains("return 42;")
    );
}

#[test]
#[ignore = "requires the separately built release RadGuard helper"]
fn real_backend_preserves_full_ids_and_external_type_output() {
    let helper = std::env::var_os("RDX_RADGUARD_HELPER").expect("set absolute RDX_RADGUARD_HELPER");
    let output = Command::new(helper)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/androguard_context.dex"
        ))
        .arg("sample.Collision")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["backend"], rdx::alternate_source::RADGUARD_BACKEND);
    let methods: BTreeMap<_, _> = result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|method| {
            (
                method["id"].as_str().unwrap(),
                method["java"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(methods.contains_key("sample.Collision.<init>()V"));
    assert!(methods.contains_key("sample.Collision.<init>(I)V"));
    for id in methods.keys() {
        assert!(!id.contains("; "), "presentation separator leaked: {id}");
    }
    assert!(methods.contains_key("sample.Collision.same(I)I"));
    let string = methods["sample.Collision.same(Ljava/lang/String;)Ljava/lang/String;"];
    assert!(string.contains("String") && string.contains("StringBuilder"));
    assert!(methods["sample.Collision.external(Ljava/util/Date;)J"].contains("java.util.Date"));
}
