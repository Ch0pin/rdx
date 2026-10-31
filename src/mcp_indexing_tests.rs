use super::*;

struct Fixture {
    directory: PathBuf,
    server: Option<Server>,
    registration: Registration,
    args: Value,
}
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("rdx-mcp-index-{}", random_id().unwrap()));
        fs::create_dir_all(&directory).unwrap();
        let server = Server::start_in(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"),
            directory.clone(),
            random_id().unwrap(),
        )
        .unwrap();
        let registration =
            serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
        until(|| server.status().state == "Running");
        let args = json!({"instance_id":server.instance_id,"project_id":server.status().project_id,"query":"sample","limit":10});
        Self {
            directory,
            server: Some(server),
            registration,
            args,
        }
    }
    fn controller(&self) -> literals::Controller {
        self.server
            .as_ref()
            .unwrap()
            .literals
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone()
    }
    fn query(&self) -> TcpStream {
        let mut stream = TcpStream::connect(self.registration.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write_frame(&mut stream, &json!({"token":self.registration.token,"instance_id":self.registration.instance_id,"name":"find_string_references","arguments":self.args})).unwrap();
        stream
    }
    fn gate(&self) -> (literals::Controller, Arc<AtomicBool>, Arc<AtomicBool>) {
        let controller = self.controller();
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        controller.gate_for_test(entered.clone(), release.clone());
        (controller, entered, release)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.take();
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < deadline, "indexing state did not converge");
        thread::sleep(Duration::from_millis(2));
    }
}
fn reply(stream: TcpStream) -> Value {
    read_frame(&mut BufReader::new(stream)).unwrap().unwrap()
}

#[test]
fn literal_index_build_keeps_health_and_other_requests_responsive() {
    let fixture = Fixture::new();
    let (controller, entered, release) = fixture.gate();
    let request = fixture.query();
    until(|| entered.load(Ordering::Relaxed));
    let started = Instant::now();
    let info = remote(&fixture.registration, "get_instance_info", &json!({})).unwrap();
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(info["literal_index"]["state"], "indexing");
    assert!(info["literal_index"]["completed_classes"].as_u64().unwrap() > 0);
    let mut args = fixture.args.clone();
    args.as_object_mut().unwrap().remove("query");
    let classes = remote(&fixture.registration, "get_all_classes", &args).unwrap();
    assert!(!classes["data"]["items"].as_array().unwrap().is_empty());
    release.store(true, Ordering::Relaxed);
    let indexed = reply(request);
    assert_eq!(
        indexed["result"]["data"]["index_complete"], true,
        "{indexed}"
    );
    assert_eq!(controller.status()["attempt"], 1);
    let cached = remote(
        &fixture.registration,
        "find_string_references",
        &fixture.args,
    )
    .unwrap();
    assert_eq!(cached["data"], indexed["result"]["data"]);
}

#[test]
fn literal_index_is_single_flight_and_health_has_reserved_admission() {
    let fixture = Fixture::new();
    let (controller, entered, release) = fixture.gate();
    let requests: Vec<_> = (0..8).map(|_| fixture.query()).collect();
    until(|| entered.load(Ordering::Relaxed) && controller.status()["active_waiters"] == 8);
    let started = Instant::now();
    let info = remote(&fixture.registration, "get_instance_info", &json!({})).unwrap();
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(info["active_requests"], 8);
    assert_eq!(info["literal_index"]["attempt"], 1);
    let busy = remote(
        &fixture.registration,
        "find_string_references",
        &fixture.args,
    )
    .unwrap_err();
    assert!(busy.to_string().contains("INSTANCE_BUSY"));
    release.store(true, Ordering::Relaxed);
    for request in requests {
        let result = reply(request);
        assert_eq!(result["result"]["data"]["index_complete"], true, "{result}");
    }
    assert_eq!(controller.status()["attempt"], 1);
}

#[test]
fn partial_request_frame_does_not_block_health() {
    let fixture = Fixture::new();
    let mut partial = TcpStream::connect(fixture.registration.address).unwrap();
    partial.write_all(b"{\"token\":").unwrap();
    let started = Instant::now();
    let info = remote(&fixture.registration, "get_instance_info", &json!({})).unwrap();
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(info["state"], "Running");
}

#[test]
fn disconnected_last_index_waiter_cancels_and_next_query_retries() {
    let fixture = Fixture::new();
    let (controller, entered, release) = fixture.gate();
    let request = fixture.query();
    until(|| entered.load(Ordering::Relaxed));
    drop(request);
    until(|| controller.status()["state"] == "cancelled");
    assert_eq!(controller.status()["index_complete"], false);
    release.store(true, Ordering::Relaxed);
    let retried = remote(
        &fixture.registration,
        "find_string_references",
        &fixture.args,
    )
    .unwrap();
    assert_eq!(retried["data"]["index_complete"], true);
    assert_eq!(controller.status()["attempt"], 2);
}

#[test]
fn request_deadline_and_server_stop_cancel_background_indexing() {
    let fixture = Fixture::new();
    let (controller, entered, _) = fixture.gate();
    let request = fixture.query();
    until(|| entered.load(Ordering::Relaxed));
    for request in fixture
        .server
        .as_ref()
        .unwrap()
        .requests
        .lock()
        .unwrap()
        .iter_mut()
    {
        if request.parsed.load(Ordering::Relaxed) && !request.done.load(Ordering::Relaxed) {
            request.started = Instant::now() - Duration::from_secs(121);
        }
    }
    let result = reply(request);
    assert!(
        result["error"].as_str().unwrap().contains("CANCELLED"),
        "{result}"
    );
    until(|| controller.status()["state"] == "cancelled");
    let mut fixture = Fixture::new();
    let (controller, entered, _) = fixture.gate();
    let request = fixture.query();
    until(|| entered.load(Ordering::Relaxed));
    fixture.server.take();
    let result = reply(request);
    assert!(
        result["error"].as_str().unwrap().contains("CANCELLED"),
        "{result}"
    );
    until(|| controller.status()["state"] == "cancelled");
    assert_eq!(controller.status()["index_complete"], false);
}

#[test]
fn changed_or_deleted_apk_during_indexing_invalidates_and_rejects_delivery() {
    for delete in [false, true] {
        let directory =
            std::env::temp_dir().join(format!("rdx-mcp-index-identity-{}", random_id().unwrap()));
        fs::create_dir_all(&directory).unwrap();
        let apk = directory.join("input.apk");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk"),
            &apk,
        )
        .unwrap();
        let server =
            Server::start_in(apk.clone(), directory.clone(), random_id().unwrap()).unwrap();
        let registration =
            serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
        until(|| server.status().state == "Running");
        let args = json!({"instance_id":server.instance_id,"project_id":server.status().project_id,"query":"sample","limit":10});
        let fixture = Fixture {
            directory,
            server: Some(server),
            registration,
            args,
        };
        let (controller, entered, release) = fixture.gate();
        let query = fixture.query();
        until(|| entered.load(Ordering::Relaxed));
        if delete {
            fs::remove_file(&apk).unwrap();
        } else {
            OpenOptions::new()
                .append(true)
                .open(&apk)
                .unwrap()
                .write_all(&[0])
                .unwrap();
        }
        release.store(true, Ordering::Relaxed);
        let result = reply(query);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("PROJECT_CHANGED"),
            "{result}"
        );
        assert_eq!(controller.status()["state"], "invalidated");
        assert_eq!(controller.status()["index_complete"], false);
        let retry = remote(
            &fixture.registration,
            "find_string_references",
            &fixture.args,
        )
        .unwrap_err();
        assert!(retry.to_string().contains("PROJECT_CHANGED"));
        assert_eq!(controller.status()["state"], "invalidated");
    }
}

#[test]
#[ignore = "requires an actual APK through RDX_LITERAL_APK"]
fn actual_apk_literal_index_keeps_health_responsive_and_reuses_complete_results() {
    let path = PathBuf::from(std::env::var("RDX_LITERAL_APK").expect("RDX_LITERAL_APK"));
    let directory =
        std::env::temp_dir().join(format!("rdx-mcp-literal-apk-{}", random_id().unwrap()));
    fs::create_dir_all(&directory).unwrap();
    let server = Server::start_in(
        path.canonicalize().unwrap(),
        directory.clone(),
        random_id().unwrap(),
    )
    .unwrap();
    let registration: Registration =
        serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
    let loading = Instant::now();
    loop {
        let status = server.status();
        assert_ne!(status.state, "Failed", "{}", status.last_error);
        if status.state == "Running" {
            break;
        }
        assert!(
            loading.elapsed() < Duration::from_secs(180),
            "APK loading deadline"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let identity = server.status();
    let args = json!({"instance_id":server.instance_id,"project_id":identity.project_id,"query":"mResources","limit":10});
    let (sender, receiver) = std::sync::mpsc::channel();
    let (entry, query) = (registration.clone(), args.clone());
    let worker = thread::spawn(move || {
        sender
            .send(remote(&entry, "find_string_references", &query))
            .unwrap();
    });
    let started = Instant::now();
    let mut max_health_latency = Duration::ZERO;
    let mut samples = 0;
    let mut max_completed = 0;
    let first = loop {
        let health_start = Instant::now();
        let info = remote(&registration, "get_instance_info", &json!({})).unwrap();
        let latency = health_start.elapsed();
        max_health_latency = max_health_latency.max(latency);
        assert!(
            latency < Duration::from_secs(2),
            "health latency {latency:?}"
        );
        assert_eq!(info["state"], "Running");
        assert_eq!(info["project_id"], identity.project_id);
        max_completed = max_completed.max(
            info["literal_index"]["completed_classes"]
                .as_u64()
                .unwrap_or(0),
        );
        samples += 1;
        match receiver.try_recv() {
            Ok(result) => break result.unwrap(),
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(error) => panic!("index request worker: {error}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(150),
            "index response deadline"
        );
        thread::sleep(Duration::from_millis(20));
    };
    worker.join().unwrap();
    let first_ms = started.elapsed().as_millis();
    assert_eq!(first["data"]["index_complete"], true);
    assert_eq!(first["identity"]["project_id"], identity.project_id);
    assert_eq!(first["identity"]["apk_sha256"], identity.apk_sha256);
    let cached_start = Instant::now();
    let cached = remote(&registration, "find_string_references", &args).unwrap();
    assert_eq!(cached, first);
    println!(
        "{}",
        json!({"apk_path":path,"project_id":identity.project_id,"apk_sha256":identity.apk_sha256,
        "first_query_ms":first_ms,"cached_query_ms":cached_start.elapsed().as_millis(),
        "health_samples":samples,"max_health_latency_ms":max_health_latency.as_millis(),"max_progress_classes":max_completed,
        "indexed_classes":first["data"]["indexed_classes"],"indexed_dexes":first["data"]["indexed_dexes"],
        "estimated_index_bytes":first["data"]["estimated_index_bytes"],"query_total":first["data"]["total"]})
    );
    drop(server);
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn idle_parsed_requests_do_not_wait_on_socket_timeouts_or_hide_disconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut clients = Vec::new();
    let mut watched = Vec::new();
    for _ in 0..8 {
        clients.push(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        let (stream, _) = listener.accept().unwrap();
        // Readiness must remain immediate even when the stream deliberately
        // has a long blocking-read timeout and its writer shares the socket.
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        watched.push(RequestWatch {
            stream,
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(false)),
            parsed: Arc::new(AtomicBool::new(true)),
            started: Instant::now(),
        });
    }
    let requests = Mutex::new(watched);
    let started = Instant::now();
    poll_requests(&requests);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "idle request polling waited for a blocking read"
    );
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request.cancel.load(Ordering::Relaxed))
    );
    // Readiness and the following peek preserve queued data for its owner.
    clients[0].write_all(b"x").unwrap();
    until(|| socket::readable(&requests.lock().unwrap()[0].stream).unwrap());
    poll_requests(&requests);
    let mut byte = [0];
    assert_eq!(
        requests.lock().unwrap()[0].stream.peek(&mut byte).unwrap(),
        1
    );
    assert_eq!(byte, [b'x']);
    assert!(!requests.lock().unwrap()[0].cancel.load(Ordering::Relaxed));
    // Closing a separate idle peer still cancels its outstanding work.
    clients[1].shutdown(std::net::Shutdown::Both).unwrap();
    until(|| {
        poll_requests(&requests);
        requests.lock().unwrap()[1].cancel.load(Ordering::Relaxed)
    });
    requests.lock().unwrap()[2].started = Instant::now() - Duration::from_secs(121);
    poll_requests(&requests);
    assert!(requests.lock().unwrap()[2].cancel.load(Ordering::Relaxed));
    requests.lock().unwrap()[3]
        .done
        .store(true, Ordering::Relaxed);
    poll_requests(&requests);
    assert_eq!(requests.lock().unwrap().len(), 7);
}

#[test]
fn headless_owned_service_is_queryable_and_unregisters_on_drop() {
    let fixture = Fixture::new();
    let info = remote(&fixture.registration, "get_instance_info", &json!({})).unwrap();
    assert_eq!(info["state"], "Running");
    let record = fixture.registration.clone();
    drop(fixture);
    assert!(remote(&record, "get_instance_info", &json!({})).is_err());
}

#[test]
fn headless_open_request_deduplicates_mode_and_path_and_releases_registration() {
    let directory =
        std::env::temp_dir().join(format!("rdx-headless-open-{}", random_id().unwrap()));
    fs::create_dir_all(&directory).unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/navigation.apk");
    let args = json!({"path":path,"headless":true,"request_key":"recover"});
    let mut opened = std::collections::HashMap::new();
    let first = open_apk_request(&args, &mut opened, directory.clone()).unwrap();
    let second = open_apk_request(&args, &mut opened, directory.clone()).unwrap();
    assert_eq!(first["instance_id"], second["instance_id"]);
    assert_eq!(opened.len(), 1);
    assert!(
        open_apk_request(
            &json!({"path":path,"headless":false,"request_key":"recover"}),
            &mut opened,
            directory.clone()
        )
        .is_err()
    );
    let server = opened["recover"].server.as_ref().unwrap();
    until(|| server.status().state == "Running");
    let registration: Registration =
        serde_json::from_slice(&fs::read(&server.registration).unwrap()).unwrap();
    let info = remote(&registration, "get_instance_info", &json!({})).unwrap();
    assert_eq!(info["state"], "Running");
    let selected = discover(
        std::slice::from_ref(&registration),
        &json!({"apk_path":path}),
    )
    .unwrap();
    assert_eq!(selected["selection"]["status"], "unique_healthy_match");
    let later = discover(
        std::slice::from_ref(&registration),
        &json!({"apk_path":path,"offset":1}),
    )
    .unwrap();
    // Selection uses the fully checked registration batch, independently of
    // the requested result slice.
    assert_eq!(later["items"], json!([]));
    assert_eq!(
        later["selection"]["selected"],
        selected["selection"]["selected"]
    );
    assert_eq!(later["selection"]["exact_match_unique"], true);
    let record = server.registration.clone();
    drop(opened);
    assert!(!record.exists());
    fs::remove_dir_all(directory).unwrap();
}
