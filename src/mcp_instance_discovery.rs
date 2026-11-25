//! Bounded discovery of local registrations and exact project identity.
use super::*;
use std::io::ErrorKind;

const MAX_RESOLVE_REGISTRATIONS: usize = 128;
const RESOLVE_CONCURRENCY: usize = 8;
const RESOLVE_TIME: Duration = Duration::from_secs(5);
const PROBE_TIME: Duration = Duration::from_millis(800);
const MAX_DIAGNOSTICS: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Relation {
    Related,
    Unrelated,
    Unknown,
}

fn relation(info: &Value, entry: &Registration, args: &Value) -> Relation {
    let mut unknown = false;
    for key in ["package_name", "apk_sha256"] {
        if let Some(expected) = args[key].as_str() {
            match info[key].as_str().filter(|value| !value.is_empty()) {
                Some(actual) if actual != expected => return Relation::Unrelated,
                None => unknown = true,
                _ => {}
            }
        }
    }
    if let Some(expected) = args["apk_path"].as_str() {
        let expected_path = Path::new(expected);
        let bare = !expected_path.is_absolute() && expected_path.components().count() == 1;
        let actual = info["apk_path"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(Path::new)
            .or(entry.apk_path.as_deref());
        match actual {
            Some(actual) if bare => {
                if !actual
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(expected))
                {
                    return Relation::Unrelated;
                }
            }
            Some(actual) if actual.is_absolute() && expected_path.is_absolute() => {
                if let (Ok(a), Ok(b)) = (actual.canonicalize(), expected_path.canonicalize()) {
                    if a != b {
                        return Relation::Unrelated;
                    }
                } else if actual != expected_path {
                    return Relation::Unrelated;
                }
            }
            _ => unknown = true,
        }
    }
    if unknown {
        Relation::Unknown
    } else {
        Relation::Related
    }
}

pub(super) struct Scan<'a> {
    args: &'a Value,
    filtered: bool,
    full_diagnostics: bool,
    pub(super) items: Vec<Value>,
    diagnostics: Vec<Value>,
    checked: usize,
    unrelated: usize,
    unknown: usize,
    suppressed: usize,
    capped: usize,
    timed_out: usize,
    uncertain: usize,
}
impl<'a> Scan<'a> {
    pub(super) fn new(args: &'a Value) -> Self {
        let filtered = ["package_name", "apk_sha256", "apk_path"]
            .iter()
            .any(|key| args.get(*key).is_some());
        Self {
            args,
            filtered,
            full_diagnostics: args["diagnostics"] == "all"
                || !filtered && args.get("diagnostics").is_none(),
            items: Vec::new(),
            diagnostics: Vec::new(),
            checked: 0,
            unrelated: 0,
            unknown: 0,
            suppressed: 0,
            capped: 0,
            timed_out: 0,
            uncertain: 0,
        }
    }

    pub(super) fn record(&mut self, entry: &Registration, probe: Result<Value>, resolve: bool) {
        self.checked += 1;
        match probe {
            Ok(mut info) if instance_matches(&info, self.args) => {
                info["reachability"] = json!("reachable");
                info["last_seen_unix_ms"] = json!(unix_ms());
                info["owner_pid"] = json!(entry.owner_pid);
                info["registered_at_unix_ms"] = json!(entry.registered_at_unix_ms);
                if resolve {
                    let ready = info["state"] == "Running"
                        && info["project_id"].as_str().is_some_and(|id| !id.is_empty());
                    if !ready {
                        if !non_running(&info) {
                            self.uncertain += 1;
                        }
                        self.diagnostic(entry, "project_not_ready", Relation::Related, &info, None);
                        return;
                    }
                    self.items.push(json!({"instance_id":entry.instance_id,"project_id":info["project_id"],
                        "state":"Running","package_name":info["package_name"],"apk_sha256":info["apk_sha256"],
                        "apk_path":info["apk_path"],"reachability":"reachable"}));
                } else {
                    self.items.push(info);
                }
            }
            Ok(info) => {
                let relation = relation(&info, entry, self.args);
                if resolve && relation != Relation::Unrelated && !non_running(&info) {
                    self.uncertain += 1;
                }
                let status = if relation == Relation::Unrelated {
                    "reachable_unrelated"
                } else {
                    "reachable_identity_unknown"
                };
                self.diagnostic(entry, status, relation, &info, None);
            }
            Err(error) => {
                let kind = error
                    .downcast_ref::<std::io::Error>()
                    .map(std::io::Error::kind);
                if matches!(kind, Some(ErrorKind::TimedOut | ErrorKind::WouldBlock)) {
                    self.timed_out += 1;
                }
                let relation = relation(&Value::Null, entry, self.args);
                if resolve
                    && relation != Relation::Unrelated
                    && kind != Some(ErrorKind::ConnectionRefused)
                {
                    self.uncertain += 1;
                }
                self.diagnostic(
                    entry,
                    "instance_unreachable",
                    relation,
                    &Value::Null,
                    Some(&error),
                );
            }
        }
    }

    fn diagnostic(
        &mut self,
        entry: &Registration,
        status: &str,
        relation: Relation,
        info: &Value,
        error: Option<&anyhow::Error>,
    ) {
        self.unrelated += usize::from(relation == Relation::Unrelated);
        self.unknown += usize::from(relation == Relation::Unknown);
        if !self.full_diagnostics && relation == Relation::Unrelated {
            self.suppressed += 1;
            return;
        }
        if self.diagnostics.len() == MAX_DIAGNOSTICS {
            self.capped += 1;
            return;
        }
        let mut record = json!({"instance_id":entry.instance_id,"status":status,"identity":
        match relation {
            Relation::Unknown => "unknown",
            Relation::Unrelated => "unrelated",
            Relation::Related if self.filtered => "related",
            Relation::Related => "unfiltered",
        }});
        if self.full_diagnostics {
            record["checked_at_unix_ms"] = json!(unix_ms());
            record["last_seen_unix_ms"] = json!(error.is_none().then(unix_ms));
            record["owner_pid"] = json!(entry.owner_pid);
            record["registered_at_unix_ms"] = json!(entry.registered_at_unix_ms);
            record["apk_path"] = info
                .get("apk_path")
                .cloned()
                .unwrap_or_else(|| json!(entry.apk_path));
            record["project_id"] = info["project_id"].clone();
            record["package_name"] = info["package_name"].clone();
            record["apk_sha256"] = info["apk_sha256"].clone();
            record["state"] = info["state"].clone();
            if let Some(error) = error {
                record["error"] =
                    json!(format!("{error:#}").chars().take(1024).collect::<String>());
                record["recovery"] = json!({"tool":"open_apk","arguments":{"path":entry.apk_path,
                    "request_key":format!("recovery-{}",entry.instance_id),"headless":true},"requires_explicit_call":true});
                record["action"] =
                    json!("Check the GUI MCP status and retry. Reopen the GUI if it has closed.");
            }
        }
        self.diagnostics.push(record);
    }

    pub(super) fn attach_diagnostics(self, result: &mut Value) {
        result["diagnostic_counts"] = json!({"unrelated":self.unrelated,"unknown_identity":self.unknown,
            "suppressed_unrelated":self.suppressed,"details_omitted_at_limit":self.capped,
            "returned":self.diagnostics.len(),"timed_out":self.timed_out});
        result["diagnostics"] = json!(self.diagnostics);
    }
}

fn non_running(info: &Value) -> bool {
    matches!(info["state"].as_str(), Some("Loading" | "Failed"))
}

fn probe(
    entry: &Registration,
    control: &gateway::RequestControl,
    deadline: Instant,
) -> Result<Value> {
    let timeout = deadline.saturating_duration_since(Instant::now());
    if timeout.is_zero() {
        return Err(
            std::io::Error::new(ErrorKind::TimedOut, "Discovery probe deadline elapsed").into(),
        );
    }
    let mut stream =
        TcpStream::connect_timeout(&entry.address, timeout.min(Duration::from_millis(300)))
            .context("INSTANCE_UNAVAILABLE: cannot connect to registered GUI")?;
    stream.set_write_timeout(Some(
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1)),
    ))?;
    write_frame(
        &mut stream,
        &json!({"token":entry.token,"instance_id":entry.instance_id,"name":"get_instance_info","arguments":{}}),
    )?;
    let reply = control
        .read_response_until(stream, deadline)?
        .context("INSTANCE_DISCONNECTED: GUI closed the discovery connection")?;
    let info = normalize_instance_info(reply["result"].clone());
    ensure!(
        info["instance_id"] == entry.instance_id && info["state"].is_string(),
        "Invalid discovery reply identity"
    );
    Ok(info)
}

pub(super) fn resolve(args: &Value, control: &gateway::RequestControl) -> Result<Value> {
    let directory = registry()?;
    let snapshot = registration_snapshot(&directory)?;
    let mut result = resolve_snapshot(&snapshot.0, args, control, snapshot.1, RESOLVE_TIME)?;
    let changed = registration_snapshot(&directory).map_or(true, |current| current != snapshot);
    result["registry_changed"] = json!(changed);
    if changed {
        result["discovery_complete"] = json!(false);
        if result["selection"]["status"] != "ambiguous" {
            result["selection"]["status"] = if result["selection"]["candidates"]
                .as_array()
                .is_some_and(Vec::is_empty)
            {
                json!("discovery_incomplete")
            } else {
                json!("matching_candidate_discovery_incomplete")
            };
            result["selection"]["selected"] = Value::Null;
            result["selection"]["exact_match_unique"] = Value::Null;
        }
    }
    Ok(result)
}

fn resolve_snapshot(
    entries: &[Registration],
    args: &Value,
    control: &gateway::RequestControl,
    registry_complete: bool,
    time: Duration,
) -> Result<Value> {
    let mut filters = args.clone();
    filters["apk_sha256"] = json!(required(args, "apk_sha256")?.to_ascii_lowercase());
    let deadline = Instant::now() + time;
    let mut scan = Scan::new(&filters);
    let candidates = &entries[..entries.len().min(MAX_RESOLVE_REGISTRATIONS)];
    for batch in candidates.chunks(RESOLVE_CONCURRENCY) {
        ensure!(!control.is_cancelled(), "CANCELLED: request cancelled");
        if Instant::now() >= deadline {
            break;
        }
        let replies = thread::scope(|scope| {
            let workers: Vec<_> = batch
                .iter()
                .map(|entry| {
                    scope.spawn(move || {
                        probe(entry, control, deadline.min(Instant::now() + PROBE_TIME))
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Discovery probe failed")))
                })
                .collect::<Vec<_>>()
        });
        for (entry, reply) in batch.iter().zip(replies) {
            scan.record(entry, reply, true);
        }
    }
    ensure!(!control.is_cancelled(), "CANCELLED: request cancelled");
    let duplicate_candidate = scan.items.iter().any(|candidate| {
        entries
            .iter()
            .filter(|entry| candidate["instance_id"] == entry.instance_id)
            .take(2)
            .count()
            > 1
    });
    let complete = registry_complete
        && scan.checked == entries.len()
        && scan.uncertain == 0
        && !duplicate_candidate;
    let mut selection = feedback::selection(&scan.items, &filters, complete);
    selection["hint"] = json!(
        "Selection requires exactly one healthy exact match across the inspected registry snapshot. Use the returned instance_id and project_id together."
    );
    let mut result = json!({"selection":selection,"items":scan.items,"total":scan.items.len(),
        "registered_instances":entries.len(),"checked_registrations":scan.checked,"discovery_complete":complete,
        "registry_scan_complete":registry_complete,"unchecked_registrations":entries.len()-scan.checked,
        "uncertain_reachability_or_identity":scan.uncertain,"duplicate_candidate_registration":duplicate_candidate,
        "registration_budget":MAX_RESOLVE_REGISTRATIONS,"deadline_ms":time.as_millis()});
    scan.attach_diagnostics(&mut result);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(index: usize, address: SocketAddr) -> Registration {
        Registration {
            instance_id: format!("instance-{index:02}"),
            address,
            token: "test-token".into(),
            owner_pid: None,
            apk_path: None,
            registered_at_unix_ms: None,
        }
    }

    #[test]
    fn resolver_checks_later_registrations_and_never_selects_ambiguous_or_partial_matches() {
        for (matches, registry_complete, expected) in [
            (vec![19], true, "unique_healthy_match"),
            (vec![1, 19], true, "ambiguous"),
            (vec![19], false, "matching_candidate_discovery_incomplete"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let entries: Vec<_> = (0..20).map(|index| entry(index, address)).collect();
            let hash = "a".repeat(64);
            let args = json!({"package_name":"p.target","apk_sha256":hash.to_ascii_uppercase()});
            validate("resolve_instance", &args).unwrap();
            let serving = thread::spawn(move || {
                for _ in 0..20 {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let request = read_frame(&mut BufReader::new(&mut stream))
                        .unwrap()
                        .unwrap();
                    assert_eq!(request["token"], "test-token");
                    let id = request["instance_id"].as_str().unwrap();
                    let index: usize = id.strip_prefix("instance-").unwrap().parse().unwrap();
                    if let Some(state) = match index {
                        0 => Some("Loading"),
                        2 => Some("Failed"),
                        _ => None,
                    } {
                        write_frame(
                            &mut stream,
                            &json!({"result":{"instance_id":id,"state":state}}),
                        )
                        .unwrap();
                        continue;
                    }
                    let package = if matches.contains(&index) {
                        "p.target"
                    } else {
                        "p.other"
                    };
                    write_frame(&mut stream, &json!({"result":{"instance_id":id,"state":"Running",
                        "project_id":format!("project-{index}"),"package_name":package,"apk_sha256":hash}})).unwrap();
                }
            });
            let result = resolve_snapshot(
                &entries,
                &args,
                &gateway::RequestControl::default(),
                registry_complete,
                Duration::from_secs(5),
            )
            .unwrap();
            serving.join().unwrap();
            assert_eq!(result["checked_registrations"], 20);
            assert_eq!(result["selection"]["status"], expected, "{result}");
            assert_eq!(
                result["diagnostic_counts"]["suppressed_unrelated"],
                18 - result["total"].as_u64().unwrap()
            );
            assert_eq!(result["diagnostic_counts"]["unknown_identity"], 2);
            assert_eq!(result["diagnostics"].as_array().unwrap().len(), 2);
            if expected == "unique_healthy_match" {
                assert_eq!(result["selection"]["selected"]["project_id"], "project-19");
            } else {
                assert!(result["selection"]["selected"].is_null());
            }
        }
    }

    #[test]
    fn resolver_times_out_and_releases_a_partial_reply_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let registration = entry(0, listener.local_addr().unwrap());
        let serving = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            assert!(
                read_frame(&mut BufReader::new(&mut stream))
                    .unwrap()
                    .is_some()
            );
            stream.write_all(b"{\"partial\":").unwrap();
            let mut byte = [0];
            assert_eq!(
                stream.read(&mut byte).unwrap(),
                0,
                "probe connection should close at deadline"
            );
        });
        let args = json!({"package_name":"p.target","apk_sha256":"a".repeat(64)});
        let result = resolve_snapshot(
            &[registration],
            &args,
            &gateway::RequestControl::default(),
            true,
            Duration::from_millis(400),
        )
        .unwrap();
        serving.join().unwrap();
        assert_eq!(result["discovery_complete"], false);
        assert_eq!(result["diagnostic_counts"]["timed_out"], 1);
        assert_eq!(result["diagnostic_counts"]["unknown_identity"], 1);
        assert!(result["selection"]["selected"].is_null());
        assert!(result["diagnostics"][0].get("recovery").is_none());
    }

    #[test]
    fn diagnostics_compact_known_unrelated_and_keep_unknown_explicit() {
        let root = tempfile::tempdir().unwrap();
        let target_path = root.path().join("target.apk");
        let unrelated_path = root.path().join("elsewhere.apk");
        assert!(target_path.is_absolute() && unrelated_path.is_absolute());
        let address = "127.0.0.1:1".parse().unwrap();
        let mut unrelated = entry(0, address);
        unrelated.apk_path = Some(unrelated_path);
        let unknown = entry(1, address);
        let mut args = json!({"apk_path":target_path,"package_name":"p.target"});
        for detail in ["related", "all"] {
            args["diagnostics"] = json!(detail);
            let mut scan = Scan::new(&args);
            for entry in [&unrelated, &unknown] {
                scan.record(entry, Err(anyhow::anyhow!("test disconnection")), false);
            }
            let mut result = json!({});
            scan.attach_diagnostics(&mut result);
            assert_eq!(result["diagnostic_counts"]["unrelated"], 1);
            assert_eq!(result["diagnostic_counts"]["unknown_identity"], 1);
            if detail == "all" {
                assert_eq!(result["diagnostic_counts"]["returned"], 2);
                assert_eq!(result["diagnostics"][0]["identity"], "unrelated");
                assert!(result["diagnostics"][1]["recovery"].is_object());
            } else {
                assert_eq!(result["diagnostic_counts"]["suppressed_unrelated"], 1);
                assert_eq!(
                    result["diagnostics"],
                    json!([{"instance_id":"instance-01","identity":"unknown","status":"instance_unreachable"}])
                );
            }
        }
    }
}
