//! Exercise the actual stdio process while a registered GUI delays its index.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const INSTANCE: &str = "stdio-index-fixture";
const PROJECT: &str = "fixture-project";
const TOKEN: &str = "fixture-loopback-token";
const TIMEOUT: Duration = Duration::from_secs(10);
// Debug builds hash the executable before the gateway can initialize.
// Measure responsiveness only after that startup work has finished.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

fn fixture_home() -> PathBuf {
    static NEXT_HOME: AtomicUsize = AtomicUsize::new(0);
    loop {
        let sequence = NEXT_HOME.fetch_add(1, Ordering::Relaxed);
        let home =
            std::env::temp_dir().join(format!("rdx-stdio-index-{}-{sequence}", std::process::id()));
        // Reserve the directory atomically. Concurrent tests must never share
        // registrations, and stale directories from a reused PID stay intact.
        match fs::create_dir(&home) {
            Ok(()) => return home,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("reserve fixture directory {}: {error}", home.display()),
        }
    }
}

struct Gui {
    home: PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    requests: Receiver<(TcpStream, Value)>,
}
impl Gui {
    fn start() -> Self {
        let home = fixture_home();
        let registry = home.join(".rdx/mcp");
        fs::create_dir_all(&registry).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        fs::write(
            registry.join(format!("{INSTANCE}.json")),
            serde_json::to_vec(&json!({
                "instance_id":INSTANCE, "address":listener.local_addr().unwrap(), "token":TOKEN,
            }))
            .unwrap(),
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (tx, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fake GUI accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(TIMEOUT)).unwrap();
                let mut line = String::new();
                BufReader::new(&mut stream).read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["token"], TOKEN);
                assert_eq!(request["instance_id"], INSTANCE);
                match request["name"].as_str().unwrap() {
                    "get_instance_info" => reply(
                        &mut stream,
                        json!({
                            "instance_id":INSTANCE, "project_id":PROJECT, "state":"Running",
                            "literal_index":{"state":"indexing"}, "active_requests":1,
                        }),
                    ),
                    "find_string_references" => {
                        if tx.send((stream, request)).is_err() {
                            break;
                        }
                    }
                    name => panic!("unexpected GUI request: {name}"),
                }
            }
        });
        Self {
            home,
            stop,
            worker: Some(worker),
            requests,
        }
    }
    fn literal(&self, query: &str) -> TcpStream {
        let (stream, request) = self
            .requests
            .recv_timeout(TIMEOUT)
            .expect("gateway did not open GUI literal request");
        assert_eq!(request["arguments"]["query"], query);
        assert_eq!(request["arguments"]["project_id"], PROJECT);
        stream
    }
}
impl Drop for Gui {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn reply(stream: &mut TcpStream, result: Value) {
    writeln!(stream, "{}", json!({"result":result})).unwrap();
    stream.flush().unwrap();
}
fn assert_disconnected(stream: &mut TcpStream) {
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut byte = [0];
    match stream.read(&mut byte) {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) => {}
        other => panic!("gateway failed to cancel GUI socket: {other:?}"),
    }
}

struct Gateway {
    child: Child,
    input: Option<ChildStdin>,
    // Holding this pipe unread intentionally exercises blocked writer cleanup.
    unread: Option<ChildStdout>,
    replies: Receiver<Value>,
    reader: Option<JoinHandle<()>>,
}
impl Gateway {
    fn start(gui: &Gui, drain: bool) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rdx"))
            .arg("mcp")
            .env("HOME", &gui.home)
            .env_remove("LOCALAPPDATA")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::channel();
        let (stdout_tx, stdout_rx) = mpsc::channel();
        let reader = Some(thread::spawn(move || {
            let mut output = BufReader::new(output);
            loop {
                let mut line = String::new();
                if !matches!(output.read_line(&mut line), Ok(n) if n > 0) {
                    break;
                }
                let Ok(value) = serde_json::from_str(&line) else {
                    break;
                };
                if tx.send(value).is_err() {
                    break;
                }
                if !drain {
                    // Only initialize has been sent. Return the pipe before any
                    // operation responses so blocked-writer coverage stays intact.
                    let _ = stdout_tx.send(output.into_inner());
                    break;
                }
            }
        }));
        let mut gateway = Self {
            child,
            input,
            unread: None,
            replies,
            reader,
        };
        gateway.send(json!({"jsonrpc":"2.0", "id":0, "method":"initialize", "params":{
            "protocolVersion":"2025-06-18", "capabilities":{}, "clientInfo":{"name":"indexing-test","version":"1"},
        }}));
        let initialized = gateway
            .replies
            .recv_timeout(STARTUP_TIMEOUT)
            .expect("gateway initialization readiness timed out");
        assert_eq!(initialized["id"], 0);
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
        if !drain {
            gateway.unread = Some(stdout_rx.recv_timeout(TIMEOUT).unwrap());
            gateway.reader.take().unwrap().join().unwrap();
        }
        gateway.send(json!({"jsonrpc":"2.0", "method":"notifications/initialized"}));
        gateway
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input.as_mut().unwrap(), "{value}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }
    fn tool(&mut self, id: u64, name: &str, arguments: Value) {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}}));
    }
    fn literal(&mut self, id: u64, query: &str) {
        self.tool(
            id,
            "find_string_references",
            json!({"instance_id":INSTANCE,"project_id":PROJECT,"query":query}),
        );
    }
    fn receive(&self, id: u64) -> Value {
        let response = self.replies.recv_timeout(TIMEOUT).unwrap_or_else(|error| {
            panic!(
                "stdio response id={id} timed out after {}s while waiting for {}: {error}",
                TIMEOUT.as_secs(),
                match id {
                    11 => "instance health during a held literal request",
                    12 => "gateway discovery health during a held literal request",
                    13 => "ping during a held literal request",
                    10 => "cancelled literal worker response",
                    14 => "successful literal retry worker response",
                    _ => "gateway operation response",
                }
            )
        });
        assert_eq!(
            response["id"], id,
            "unexpected or premature response: {response}"
        );
        response
    }
    fn eof_and_wait(&mut self) {
        self.input.take();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "gateway EOF exit: {status}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "gateway failed to exit after stdin EOF"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.unread.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn fixture_homes_are_isolated_under_concurrent_allocation() {
    let barrier = Arc::new(std::sync::Barrier::new(16));
    let workers: Vec<_> = (0..16)
        .map(|index| {
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                let home = fixture_home();
                fs::write(home.join("owner"), index.to_string()).unwrap();
                (home, index)
            })
        })
        .collect();
    let homes: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    let distinct: std::collections::HashSet<_> = homes.iter().map(|(home, _)| home).collect();
    assert_eq!(distinct.len(), homes.len());
    for (home, index) in homes {
        assert_eq!(
            fs::read_to_string(home.join("owner")).unwrap(),
            index.to_string()
        );
        fs::remove_dir_all(home).unwrap();
    }
}

#[test]
fn waiting_literal_keeps_stdio_health_live_and_cancel_retry_works() {
    let gui = Gui::start();
    let mut gateway = Gateway::start(&gui, true);
    gateway.literal(10, "held");
    let mut held = gui.literal("held");

    gateway.tool(11, "get_instance_info", json!({"instance_id":INSTANCE}));
    let info = gateway.receive(11);
    assert_eq!(info["result"]["isError"], false);
    assert_eq!(
        info["result"]["structuredContent"]["literal_index"]["state"],
        "indexing"
    );
    gateway.tool(12, "get_health", json!({}));
    let health = gateway.receive(12);
    assert_eq!(health["result"]["isError"], false);
    assert_eq!(
        health["result"]["structuredContent"]["transport"],
        "connected"
    );
    let discovered = &health["result"]["structuredContent"];
    assert_eq!(discovered["diagnostics"], json!([]));
    assert_eq!(discovered["items"].as_array().unwrap().len(), 1);
    assert_eq!(discovered["items"][0]["instance_id"], INSTANCE);
    assert_eq!(discovered["items"][0]["literal_index"]["state"], "indexing");
    gateway.send(json!({"jsonrpc":"2.0","id":13,"method":"ping"}));
    assert_eq!(gateway.receive(13)["result"], json!({}));

    gateway.send(
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":10}}),
    );
    assert_disconnected(&mut held);
    let cancelled = gateway.receive(10);
    assert_eq!(cancelled["result"]["isError"], true);
    assert!(
        cancelled["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("CANCELLED")
    );

    gateway.literal(14, "retry");
    let mut retry = gui.literal("retry");
    reply(
        &mut retry,
        json!({"project_id":PROJECT,"items":[],"index_complete":true}),
    );
    let retried = gateway.receive(14);
    assert_eq!(retried["result"]["isError"], false);
    assert_eq!(
        retried["result"]["structuredContent"]["index_complete"],
        true
    );

    gateway.literal(15, "eof");
    let mut outstanding = gui.literal("eof");
    gateway.eof_and_wait();
    assert_disconnected(&mut outstanding);
}

#[test]
fn stdin_eof_cancels_pending_socket_even_with_unread_large_stdout() {
    let gui = Gui::start();
    let mut gateway = Gateway::start(&gui, false);
    gateway.literal(20, "large");
    let mut large = gui.literal("large");
    reply(&mut large, json!({"payload":"x".repeat(1024 * 1024)}));
    // EOF here proves the GUI result was consumed and its socket closed. The
    // doubled MCP text/structured response cannot fit in the unread pipe.
    assert_disconnected(&mut large);
    gateway.literal(21, "pending");
    let mut pending = gui.literal("pending");
    gateway.eof_and_wait();
    assert_disconnected(&mut pending);
}
