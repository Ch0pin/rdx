//! A headless gateway must finish spool cleanup before returning from stdin EOF.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};
struct Gateway {
    child: Child,
    input: Option<ChildStdin>,
    replies: Receiver<Value>,
    sequence: u64,
}
impl Gateway {
    fn start(home: &Path, tmp: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rdx"))
            .arg("mcp")
            .env("LOCALAPPDATA", home)
            .env("TMPDIR", tmp)
            .env("TMP", tmp)
            .env("TEMP", tmp)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str(&line) else {
                    break;
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        let mut gateway = Self {
            child,
            input,
            replies,
            sequence: 0,
        };
        gateway.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"source-shutdown","version":"1"}}}));
        let initialized = gateway
            .replies
            .recv_timeout(Duration::from_secs(120))
            .unwrap();
        assert_eq!(initialized["id"], 0);
        gateway.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        gateway
    }
    fn send(&mut self, message: Value) {
        writeln!(self.input.as_mut().unwrap(), "{message}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }
    fn call(&mut self, name: &str, args: Value) -> Value {
        self.sequence += 1;
        let id = self.sequence;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}}));
        let reply = self.replies.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        reply["result"]["structuredContent"].clone()
    }
    fn eof(&mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                break;
            }
            assert!(Instant::now() < deadline, "headless shutdown did not drain");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn spool_count(tmp: &Path) -> usize {
    fs::read_dir(tmp)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("rdx-source-search-")
        })
        .count()
}
#[test]
fn headless_eof_removes_completed_and_active_source_job_spools() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let tmp = root.path().join("tmp");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&tmp).unwrap();
    let apk = root.path().join("source-jobs.apk");
    let mut zip = zip::ZipWriter::new(fs::File::create(&apk).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("classes.dex", options).unwrap();
    zip.write_all(include_bytes!("fixtures/hello.dex")).unwrap();
    zip.start_file("AndroidManifest.xml", options).unwrap();
    zip.write_all(b"<manifest package=\"sample.sourcejobs\" />")
        .unwrap();
    zip.start_file("assets/large.txt", options).unwrap();
    zip.write_all("api ".repeat(250_000).as_bytes()).unwrap();
    zip.finish().unwrap();
    let mut gateway = Gateway::start(&home, &tmp);
    let opened = gateway.call(
        "open_apk",
        json!({"path":apk,"headless":true,"request_key":"shutdown"}),
    );
    let instance = opened["instance_id"].clone();
    let deadline = Instant::now() + Duration::from_secs(10);
    let info = loop {
        let info = gateway.call("get_instance_info", json!({"instance_id":instance}));
        assert!(Instant::now() < deadline);
        if info["state"] == "Running" {
            break info;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let pin = json!({"instance_id":instance,"project_id":info["project_id"]});
    let mut query = pin.clone();
    query["query"] = json!("return");
    let completed = gateway.call("start_source_search", query)["data"]["job_id"].clone();
    let mut status_args = pin.clone();
    status_args["job_id"] = completed;
    loop {
        let state = gateway.call("get_source_search_status", status_args.clone());
        assert!(Instant::now() < deadline);
        if state["data"]["worker_finished"] == true {
            assert_eq!(state["data"]["scan_complete"], true);
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        spool_count(&tmp),
        1,
        "completed retained spool should still exist"
    );
    let mut query = pin.clone();
    query["query"] = json!("api");
    query["scope"] = json!("resources");
    query["resource_prefix"] = json!("assets/");
    query["limit"] = json!(1);
    query["disk_budget_bytes"] = json!(64 * 1024 * 1024);
    let active = gateway.call("start_source_search", query)["data"]["job_id"].clone();
    status_args["job_id"] = active;
    assert_eq!(
        gateway.call("get_source_search_status", status_args)["data"]["worker_finished"],
        false,
        "fixture must cover an active worker at EOF"
    );
    assert_eq!(spool_count(&tmp), 2);
    gateway.eof();
    assert_eq!(
        spool_count(&tmp),
        0,
        "gateway exited before source-result cleanup"
    );
}
