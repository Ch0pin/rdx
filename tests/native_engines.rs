use rdx::{engine::DecompilerEngine, native_engine::NativeDexEngine};
use std::{
    io::{Cursor, Write},
    path::PathBuf,
    process::Command,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn native_cli_works_with_no_java_executable() {
    let output = Command::new(env!("CARGO_BIN_EXE_rdx"))
        .env("PATH", "")
        .args(["--engine", "native", "--decompile"])
        .arg(fixture("hello.apk"))
        .arg("sample.Hello")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("return 42;"));
}

#[test]
fn catalog_and_bad_engine_selection_are_explicit() {
    let output = Command::new(env!("CARGO_BIN_EXE_rdx"))
        .arg("--engines")
        .output()
        .unwrap();
    let catalog = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success() && catalog.contains("native") && !catalog.contains("jadx"));
    let output = Command::new(env!("CARGO_BIN_EXE_rdx"))
        .args(["--engine", "typo", "--list", "unused"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Only the native Rust engine"));
}

#[test]
fn malformed_input_does_not_start_java_and_zip_budget_is_checked() {
    let mut zip_end = vec![0u8; 22];
    zip_end[..4].copy_from_slice(b"PK\x05\x06");
    zip_end[12..16].copy_from_slice(&(33u32 * 1024 * 1024).to_le_bytes());
    for (extension, bytes, expected) in [
        ("dex", b"not a DEX".to_vec(), "DEX"),
        ("apk", zip_end, "central directory exceeds"),
    ] {
        let path = std::env::temp_dir().join(format!(
            "rdx-native-invalid-{}.{extension}",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_rdx"))
            .env("PATH", "")
            .args(["--engine", "native", "--list"])
            .arg(&path)
            .output()
            .unwrap();
        std::fs::remove_file(path).unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("Starting JADX"), "{error}");
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn multidex_indexes_both_inputs_and_rejects_duplicate_classes() {
    for duplicate in [false, true] {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("classes.dex", options).unwrap();
        zip.write_all(include_bytes!("fixtures/hello.dex")).unwrap();
        zip.start_file("classes2.dex", options).unwrap();
        zip.write_all(if duplicate {
            include_bytes!("fixtures/hello.dex").as_slice()
        } else {
            include_bytes!("fixtures/navigation.dex").as_slice()
        })
        .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let path = std::env::temp_dir().join(format!(
            "rdx-native-multidex-{}-{duplicate}.apk",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        let mut engine = NativeDexEngine::default();
        let result = engine.open(&path);
        std::fs::remove_file(&path).unwrap();
        if duplicate {
            assert!(format!("{:#}", result.unwrap_err()).contains("Duplicate class"));
        } else {
            let project = result.unwrap();
            assert!(project.classes.contains(&"sample.Hello".to_owned()));
            assert!(project.classes.contains(&"sample.Target".to_owned()));
        }
    }
}
