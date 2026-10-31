use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}
fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rdx"))
}
fn summary(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn cli_export_writes_complete_sources_and_preserves_class_decompile_command() {
    let tmp = tempfile::tempdir().unwrap();
    let output = command()
        .args(["--engine", "native", "--export-source"])
        .arg(fixture("navigation.apk"))
        .arg("sources with spaces")
        .args(["--package-prefix", "sample.Target"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let result = summary(&output);
    assert_eq!(result["status"], "complete");
    assert_eq!(result["exported"], 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("[1/1]"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Manifest:"));
    let manifest = fs::read_to_string(result["manifest_path"].as_str().unwrap()).unwrap();
    let rows: Vec<Value> = manifest
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["class_id"], "sample.Target");
    assert!(rows[0]["coverage"].is_object());
    let source = fs::read_to_string(rows[0]["source_path"].as_str().unwrap()).unwrap();
    assert!(source.contains("doubleValue"));
    assert_eq!(
        read_json(Path::new(result["completion_path"].as_str().unwrap()))["complete"],
        true
    );
    let original = command()
        .arg("--decompile")
        .arg(fixture("hello.dex"))
        .arg("sample.Hello")
        .output()
        .unwrap();
    assert!(original.status.success());
    assert!(String::from_utf8_lossy(&original.stdout).contains("return 42;"));
}

#[test]
fn cli_export_refuses_overwrite_and_returns_setup_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("existing");
    fs::create_dir(&out).unwrap();
    fs::write(out.join("keep.txt"), "user content").unwrap();
    let output = command()
        .arg("--export-source")
        .arg(fixture("hello.dex"))
        .arg(&out)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be new"));
    assert_eq!(
        fs::read_to_string(out.join("keep.txt")).unwrap(),
        "user content"
    );
    let missing = command()
        .arg("--export-source")
        .arg(fixture("hello.dex"))
        .arg(tmp.path().join("missing/child"))
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Output parent directory missing"));
    let usage = command()
        .args(["--export-source", "input.apk"])
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&usage.stderr).contains("Usage:"));
    let help = command()
        .args(["--export-source", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--resume"));
}

#[test]
fn cli_export_resumes_a_durable_checkpoint_and_reports_integrity_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("export");
    let run = || {
        command()
            .arg("--export-source")
            .arg(fixture("navigation.apk"))
            .arg(&out)
            .output()
            .unwrap()
    };
    let first = summary(&run());
    let manifest_path = out.join("manifest.jsonl");
    let original = fs::read_to_string(&manifest_path).unwrap();
    let first_line = original.split_inclusive('\n').next().unwrap();
    assert!(original.lines().count() > 1);
    let status_path = out.join("export-status.json");
    let mut checkpoint = read_json(&status_path);
    checkpoint["next_class"] = json!(1);
    checkpoint["manifest_bytes"] = json!(first_line.len());
    checkpoint["exported"] = json!(1);
    checkpoint["failed"] = json!(0);
    checkpoint["complete"] = json!(false);
    checkpoint["status"] = json!("running");
    fs::write(&status_path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    // Simulate interruption after durable source files but before their journal checkpoint.
    fs::write(&manifest_path, format!("{first_line}{{partial record")).unwrap();
    let resumed = command()
        .arg("--export-source")
        .arg(fixture("navigation.apk"))
        .arg(&out)
        .arg("--resume")
        .output()
        .unwrap();
    let result = summary(&resumed);
    assert_eq!(result["exported"], first["exported"]);
    assert_eq!(fs::read_to_string(&manifest_path).unwrap(), original);
    // An existing file changed by a user must survive, and the CLI must report failure.
    let mut checkpoint = read_json(&status_path);
    checkpoint["status"] = json!("incomplete");
    checkpoint["complete"] = json!(false);
    fs::write(&status_path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    fs::write(out.join("class-00000000.txt"), "changed by user").unwrap();
    let failed = command()
        .arg("--export-source")
        .arg(fixture("navigation.apk"))
        .arg(&out)
        .arg("--resume")
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(result["status"], "incomplete");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("source file changed")
    );
    assert!(result["manifest_sha256"].is_null());
    assert_eq!(
        fs::read_to_string(out.join("class-00000000.txt")).unwrap(),
        "changed by user"
    );
}
