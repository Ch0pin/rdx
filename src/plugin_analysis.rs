//! Session-local enabled plugins and bounded project analysis jobs shared by GUI/MCP.
use crate::{plugin::Plugin, project_snapshot::ProjectSnapshot, transport::Transport};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

#[derive(Clone)]
struct Job {
    plugin_id: String,
    apk_sha256: String,
    cancel: Arc<AtomicBool>,
    state: Arc<Mutex<Value>>,
}
#[derive(Default)]
pub struct Controller {
    enabled: Mutex<BTreeMap<String, Plugin>>,
    jobs: Mutex<BTreeMap<String, Job>>,
    serial: AtomicU64,
}
impl Controller {
    pub fn set_enabled(&self, plugin: &Plugin, enabled: bool) {
        let mut entries = self.enabled.lock().unwrap();
        if enabled && plugin.protocol == 2 {
            entries.insert(plugin.id.clone(), plugin.clone());
        } else {
            entries.remove(&plugin.id);
            for job in self
                .jobs
                .lock()
                .unwrap()
                .values()
                .filter(|j| j.plugin_id == plugin.id)
            {
                job.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
    pub fn list(&self) -> Value {
        let analyses: Vec<_> = self.jobs.lock().unwrap().iter().map(|(id,j)| json!({"analysis_id":id,"plugin_id":j.plugin_id,"status":j.state.lock().unwrap()["status"],"apk_sha256":j.apk_sha256})).collect();
        json!({"items":self.enabled.lock().unwrap().values().map(|p|json!({"id":p.id,"name":p.name,"protocol":p.protocol,"capability":p.capability})).collect::<Vec<_>>(),
            "analyses":analyses,"scope":"Only project plugins explicitly enabled in this GUI session; enabling executes no code"})
    }
    pub fn cancel_all(&self) {
        for job in self.jobs.lock().unwrap().values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn start(
        &self,
        snapshot: ProjectSnapshot,
        plugin_id: &str,
        arguments: Value,
    ) -> Result<Value> {
        ensure!(arguments.is_object(), "arguments must be an object");
        ensure!(
            serde_json::to_vec(&arguments)?.len() <= 16384,
            "Plugin arguments exceed 16 KiB"
        );
        let enabled = self.enabled.lock().unwrap();
        let plugin = enabled
            .get(plugin_id)
            .cloned()
            .context("PLUGIN_NOT_ENABLED: enable the manifest in the GUI first")?;
        snapshot.verify_current()?;
        let mut jobs = self.jobs.lock().unwrap();
        ensure!(
            !jobs
                .values()
                .any(|j| j.state.lock().unwrap()["status"] == "running"),
            "PLUGIN_BUSY: wait for or cancel the active analysis"
        );
        while jobs.len() >= 4 {
            let key = jobs.keys().next().unwrap().clone();
            jobs.remove(&key);
        }
        let id = format!(
            "analysis-{:016x}",
            self.serial.fetch_add(1, Ordering::Relaxed)
        );
        let state = Arc::new(Mutex::new(
            json!({"analysis_id":id,"plugin_id":plugin_id,"apk_sha256":snapshot.identity.hash,"status":"running","queries_completed":0}),
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let job = Job {
            plugin_id: plugin_id.into(),
            apk_sha256: snapshot.identity.hash.clone(),
            cancel: cancel.clone(),
            state: state.clone(),
        };
        let initial = state.lock().unwrap().clone();
        let worker = job.clone();
        thread::Builder::new().name("rdx-plugin-analysis".into()).spawn(move || {
            let (deadline_done, deadline_wait) = std::sync::mpsc::channel::<()>();
            let timeout_cancel = cancel.clone();
            thread::spawn(move || {
                if matches!(deadline_wait.recv_timeout(std::time::Duration::from_secs(300)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
                    timeout_cancel.store(true, Ordering::Relaxed);
                }
            });
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Value> {
                let executable = plugin.resolve_executable()?;
                let mut command = Command::new(executable);
                command.args(&plugin.args).current_dir(&plugin.directory);
                let mut transport = Transport::spawn(&mut command)?;
                let input = json!({"method":"project.analyze","project":{"apk_sha256":snapshot.identity.hash,"class_count":snapshot.project.classes.len()},"arguments":arguments});
                let result = transport.project_request(input, &cancel, |tool,args| {
                    let result = crate::mcp::plugin_query(&snapshot,tool,args,&cancel);
                    let mut state = state.lock().unwrap();
                    state["queries_completed"] = json!(state["queries_completed"].as_u64().unwrap()+1);
                    result
                })?;
                ensure!(serde_json::to_vec(&result)?.len() <= 4 * 1024 * 1024, "Plugin result exceeds 4 MiB; reduce analysis scope");
                ensure!(result["items"].is_array(), "Plugin result must contain items array");
                for item in result["items"].as_array().unwrap() {
                    ensure!(serde_json::to_vec(item)?.len() <= 512 * 1024, "Plugin row exceeds 512 KiB");
                }
                snapshot.verify_current()?;
                ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                Ok(result)
            })).unwrap_or_else(|_| Err(anyhow::anyhow!("Plugin analysis panicked")));
            drop(deadline_done);
            let mut state = worker.state.lock().unwrap();
            match outcome {
                Ok(result) => {state["status"] = json!("complete");state["result"] = result;}
                Err(error) => {state["status"] = json!(if worker.cancel.load(Ordering::Relaxed) {"cancelled"} else {"failed"});state["error"] = json!(format!("{error:#}"));}
            }
        })?;
        jobs.insert(id, job);
        Ok(initial)
    }
    pub fn get(
        &self,
        id: &str,
        hash: &str,
        offset: usize,
        limit: usize,
        cancel: bool,
    ) -> Result<Value> {
        ensure!((1..=100).contains(&limit), "limit must be 1..100");
        let jobs = self.jobs.lock().unwrap();
        let job = jobs
            .get(id)
            .context("ANALYSIS_NOT_FOUND: only the last four session results are retained")?;
        ensure!(job.apk_sha256 == hash, "PROJECT_CHANGED");
        if cancel {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let state = job.state.lock().unwrap();
        let mut page = state
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k != "result")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<serde_json::Map<_, _>>();
        if let Some(result) = state.get("result") {
            let items = result["items"].as_array().unwrap();
            ensure!(offset <= items.len(), "offset exceeds result count");
            let mut selected = Vec::new();
            let mut bytes = 0;
            for item in items.iter().skip(offset).take(limit) {
                let size = serde_json::to_vec(item)?.len();
                ensure!(size <= 512 * 1024, "Plugin row exceeds 512 KiB");
                if bytes + size > 512 * 1024 {
                    break;
                }
                bytes += size;
                selected.push(item.clone());
            }
            let end = offset + selected.len();
            let mut result_page = result
                .as_object()
                .context("Plugin result must be an object")?
                .iter()
                .filter(|(k, _)| *k != "items")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<serde_json::Map<_, _>>();
            result_page.insert("items".into(), json!(selected));
            page.insert("result".into(), json!(result_page));
            page.insert("total".into(), json!(items.len()));
            page.insert(
                "next_offset".into(),
                json!((end < items.len()).then_some(end)),
            );
        }
        page.insert("offset".into(), json!(offset));
        page.insert(
            "cancel_requested".into(),
            json!(job.cancel.load(Ordering::Relaxed)),
        );
        Ok(json!(page))
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::Path,
        time::{Duration, Instant},
    };
    fn fixture() -> ProjectSnapshot {
        ProjectSnapshot::open(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello.apk"),
        )
        .unwrap()
    }
    fn plugin(dir: &Path, script: &str) -> Plugin {
        std::fs::write(dir.join("worker.py"), script).unwrap();
        std::fs::write(dir.join("plugin.json"),serde_json::to_vec(&json!({"protocol":2,"id":"test","name":"test","executable":"python3","args":["worker.py"],"capability":"project.analyze"})).unwrap()).unwrap();
        Plugin::load(&dir.join("plugin.json")).unwrap()
    }
    fn done(controller: &Controller, id: &str, hash: &str) -> Value {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let result = controller.get(id, hash, 0, 1, false).unwrap();
            if result["status"] != "running" {
                return result;
            }
            assert!(Instant::now() < until, "{result}");
            thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    fn enabled_job_queries_same_project_and_pages_results() {
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(
            dir.path(),
            r#"import sys,json
r=json.loads(sys.stdin.readline())
print(json.dumps(dict(protocol=2,id=r['id'],method='host.query',query_id=1,tool='get_method_source',arguments={'method_id':'sample.Hello.answer()I'})),flush=True)
a=json.loads(sys.stdin.readline())
assert 'return' in a['result']['text']
print(json.dumps(dict(protocol=2,id=r['id'],result={'items':[{'value':1},{'value':2}]})),flush=True)
"#,
        );
        let c = Controller::default();
        let snapshot = fixture();
        let hash = snapshot.apk_sha256().to_owned();
        assert!(c.start(snapshot.clone(), "test", json!({})).is_err());
        c.set_enabled(&p, true);
        let job = c.start(snapshot, "test", json!({})).unwrap();
        let id = job["analysis_id"].as_str().unwrap();
        assert!(c.get(id, "wrong-project", 0, 1, false).is_err());
        let result = done(&c, id, &hash);
        assert_eq!(result["status"], "complete", "{result}");
        assert_eq!(result["queries_completed"], 1);
        assert_eq!(result["result"]["items"], json!([{"value":1}]));
        assert_eq!(result["next_offset"], 1);
        assert_eq!(
            c.get(id, &hash, 1, 1, false).unwrap()["result"]["items"],
            json!([{"value":2}])
        );
        c.set_enabled(&p, false);
        assert_eq!(c.list()["items"], json!([]));
    }
    #[test]
    fn cancellation_terminates_nonresponsive_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(
            dir.path(),
            "import sys,time;sys.stdin.readline();time.sleep(30)",
        );
        let c = Controller::default();
        c.set_enabled(&p, true);
        let snapshot = fixture();
        let hash = snapshot.apk_sha256().to_owned();
        let job = c.start(snapshot, "test", json!({})).unwrap();
        let id = job["analysis_id"].as_str().unwrap();
        c.get(id, &hash, 0, 1, true).unwrap();
        assert_eq!(done(&c, id, &hash)["status"], "cancelled");
    }
    #[test]
    fn host_queries_deny_execution_and_export() {
        let snapshot = fixture();
        let cancel = AtomicBool::new(false);
        for tool in [
            "run_plugin_analysis",
            "export_source",
            "open_apk",
            "forget_instance",
        ] {
            assert!(
                crate::mcp::plugin_query(&snapshot, tool, &json!({}), &cancel)
                    .unwrap_err()
                    .to_string()
                    .contains("PLUGIN_QUERY_DENIED")
            );
        }
    }
    #[test]
    fn project_plugin_rejects_wrong_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(
            dir.path(),
            "import sys,json;r=json.loads(sys.stdin.readline());print(json.dumps(dict(protocol=1,id=r['id'],result={'items':[]})),flush=True)",
        );
        let c = Controller::default();
        c.set_enabled(&p, true);
        let snapshot = fixture();
        let hash = snapshot.apk_sha256().to_owned();
        let job = c.start(snapshot, "test", json!({})).unwrap();
        assert_eq!(
            done(&c, job["analysis_id"].as_str().unwrap(), &hash)["status"],
            "failed"
        );
    }
    #[test]
    #[ignore = "requires a locally supplied APK and protocol-v2 plugin manifest"]
    fn supplied_plugin_inventory() {
        let manifest = std::env::var("RDX_PLUGIN_MANIFEST").unwrap();
        let apk = std::env::var("RDX_TEST_APK").unwrap();
        let p = Plugin::load(Path::new(&manifest)).unwrap();
        let snapshot = ProjectSnapshot::open(Path::new(&apk)).unwrap();
        let hash = snapshot.apk_sha256().to_owned();
        let c = Controller::default();
        c.set_enabled(&p, true);
        let job = c.start(snapshot, &p.id, json!({"max_sites":30})).unwrap();
        let id = job["analysis_id"].as_str().unwrap();
        let until = Instant::now() + Duration::from_secs(300);
        let result = loop {
            let result = c.get(id, &hash, 0, 100, false).unwrap();
            if result["status"] != "running" {
                break result;
            }
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(result["status"], "complete", "{result}");
        let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/plugin-inventory-result.json");
        std::fs::write(&out, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
        eprintln!(
            "PLUGIN_INVENTORY {} groups; {}",
            result["total"],
            out.display()
        );
    }
}
