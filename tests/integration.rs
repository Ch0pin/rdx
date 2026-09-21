use rdx::{
    engine::{DecompilerEngine, NativeEngine},
    plugin::Plugin,
    transport::Transport,
};
use serde_json::json;
use std::{path::PathBuf, process::Command};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn native_opens_dex_and_apk_and_reconstructs_method() {
    for extension in ["dex", "apk"] {
        let mut engine = NativeEngine::start().expect("start native engine");
        let project = engine
            .open(&root().join(format!("tests/fixtures/hello.{extension}")))
            .unwrap();
        assert_eq!(project.classes, ["sample.Hello"]);
        let source = engine.decompile("sample.Hello").unwrap();
        assert!(source.contains("class Hello"), "{source}");
        assert!(source.contains("int answer()"), "{source}");
        assert!(source.contains("return 42;"), "{source}");
        assert!(
            engine
                .decompile("sample.Missing")
                .unwrap_err()
                .to_string()
                .contains("Unknown class")
        );
        assert_eq!(
            engine.decompile("sample.Hello").unwrap(),
            source,
            "native engine survives unknown classes"
        );
    }
}

#[test]
fn source_stats_runs_over_real_transport() {
    let plugin = Plugin::load(&root().join("plugins/source-stats/plugin.json")).unwrap();
    assert_eq!(plugin.executable, "rdx-source-stats");
    assert!(PathBuf::from(env!("CARGO_BIN_EXE_rdx-source-stats")).is_file());
    let source = "class Hello { // α😀\n}\n";
    let result = plugin.analyze("Hello", source).unwrap();
    assert_eq!(
        result,
        json!({"class":"Hello", "lines":2, "characters":source.chars().count()})
    );
}

fn fake_worker(script: &str) -> Transport {
    Transport::spawn(Command::new("python3").args(["-u", "-c", script])).unwrap()
}

#[test]
fn rejects_wrong_response_id() {
    let mut worker = fake_worker(
        "import sys,json; r=json.loads(sys.stdin.readline()); print(json.dumps({'protocol':1,'id':r['id']+1,'result':{}}))",
    );
    assert!(
        worker
            .request(json!({"method":"test"}))
            .unwrap_err()
            .to_string()
            .contains("mismatch")
    );
}

#[test]
fn reports_worker_error() {
    let mut worker = fake_worker(
        "import sys,json; r=json.loads(sys.stdin.readline()); print(json.dumps({'protocol':1,'id':r['id'],'error':'fixture failure'}))",
    );
    assert!(
        worker
            .request(json!({"method":"test"}))
            .unwrap_err()
            .to_string()
            .contains("fixture failure")
    );
}

#[test]
fn reports_malformed_output() {
    let mut worker = fake_worker("import sys; sys.stdin.readline(); print('not json')");
    assert!(worker.request(json!({"method":"test"})).is_err());
}

#[test]
fn reports_worker_exit() {
    let mut worker = fake_worker("import sys; sys.stdin.readline(); sys.exit(1)");
    assert!(
        worker
            .request(json!({"method":"test"}))
            .unwrap_err()
            .to_string()
            .contains("Worker stopped")
    );
}

#[test]
fn oversized_response_terminates_worker_and_prevents_reuse() {
    let mut worker = fake_worker(
        "import sys,time; sys.stdin.readline(); sys.stdout.write('x'*(16*1024*1024+1)); sys.stdout.flush(); time.sleep(30)",
    );
    assert!(
        worker
            .request(json!({"method":"test"}))
            .unwrap_err()
            .to_string()
            .contains("exceeds 16 MiB")
    );
    assert!(worker.child.lock().unwrap().try_wait().unwrap().is_some());
    assert!(
        worker
            .request(json!({"method":"test"}))
            .unwrap_err()
            .to_string()
            .contains("closed")
    );
}

#[test]
fn malformed_response_terminates_worker_without_waiting_for_drop() {
    let mut worker = fake_worker(
        "import sys,time; sys.stdin.readline(); print('not json',flush=True); time.sleep(30)",
    );
    assert!(worker.request(json!({"method":"test"})).is_err());
    assert!(worker.child.lock().unwrap().try_wait().unwrap().is_some());
    assert!(
        worker
            .request(json!({"method":"test"}))
            .unwrap_err()
            .to_string()
            .contains("closed")
    );
}
