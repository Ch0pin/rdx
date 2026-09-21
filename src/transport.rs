use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

const MAX_RESPONSE: usize = 16 * 1024 * 1024;
const MAX_STDERR: usize = 8 * 1024;

pub struct Transport {
    pub child: Arc<Mutex<Child>>,
    requests: mpsc::SyncSender<Vec<u8>>,
    replies: mpsc::Receiver<Result<Value, String>>,
    next_id: u64,
    failed: bool,
    timeout: Duration,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    stderr_done: mpsc::Receiver<()>,
}

impl Transport {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Could not start worker")?;
        let mut input = child.stdin.take().context("Missing worker stdin")?;
        let output = child.stdout.take().context("Missing worker stdout")?;
        let mut stderr = child.stderr.take().context("Missing worker stderr")?;
        let stderr_tail = Arc::new(Mutex::new(Vec::with_capacity(MAX_STDERR)));
        let captured_stderr = Arc::clone(&stderr_tail);
        let (stderr_finished, stderr_done) = mpsc::sync_channel(1);
        // Drain continuously so a noisy worker cannot block on its stderr pipe.
        // Retain only a fixed-size tail, never a whole log or an unbounded line.
        thread::spawn(move || {
            let mut buffer = [0; 4096];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut tail) = captured_stderr.lock() {
                            let excess = (tail.len() + n).saturating_sub(MAX_STDERR);
                            tail.drain(..excess);
                            tail.extend_from_slice(&buffer[..n]);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = stderr_finished.send(());
        });
        let (tx, replies) = mpsc::sync_channel(1);
        let (requests, pending) = mpsc::sync_channel::<Vec<u8>>(1);
        let write_errors = tx.clone();
        // Writing a large request to a worker that never reads must also time out.
        thread::spawn(move || {
            while let Ok(bytes) = pending.recv() {
                if let Err(error) = input.write_all(&bytes).and_then(|_| input.flush()) {
                    let _ = write_errors.send(Err(format!("Worker stdin failed: {error}")));
                    return;
                }
            }
        });
        thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut line = Vec::new();
                loop {
                    let chunk = match reader.fill_buf() {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx.send(Err(e.to_string()));
                            return;
                        }
                    };
                    if chunk.is_empty() {
                        let _ = tx.send(Err("Worker stopped before completing a response".into()));
                        return;
                    }
                    let end = chunk.iter().position(|b| *b == b'\n').map(|p| p + 1);
                    let n = end.unwrap_or(chunk.len());
                    if line.len() + n > MAX_RESPONSE {
                        let _ = tx.send(Err("Worker response exceeds 16 MiB".into()));
                        return;
                    }
                    line.extend_from_slice(&chunk[..n]);
                    reader.consume(n);
                    if end.is_some() {
                        break;
                    }
                }
                if tx
                    .send(serde_json::from_slice(&line).map_err(|e| e.to_string()))
                    .is_err()
                {
                    return;
                }
            }
        });
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            requests,
            replies,
            next_id: 0,
            failed: false,
            timeout: Duration::from_secs(60),
            stderr_tail,
            stderr_done,
        })
    }

    fn terminate(&mut self) {
        self.failed = true;
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn request(&mut self, mut request: Value) -> Result<Value> {
        if self.failed {
            bail!("Worker transport is closed; reopen the project to retry");
        }
        if !request.is_object() {
            bail!("Worker request must be an object");
        }
        self.next_id += 1;
        request["protocol"] = json!(1);
        request["id"] = json!(self.next_id);
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        let response = (|| -> Result<Value> {
            self.requests
                .try_send(bytes)
                .context("Worker request queue unavailable")?;
            let response = self
                .replies
                .recv_timeout(self.timeout)
                .context("Worker stopped or exceeded request timeout; reopen the project to retry")?
                .map_err(anyhow::Error::msg)?;
            if response["protocol"] != 1 || response["id"] != self.next_id {
                bail!("Worker protocol or request ID mismatch");
            }
            if response.get("error").is_none() && response.get("result").is_none() {
                bail!("Missing worker result");
            }
            Ok(response)
        })();
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                self.terminate();
                // EOF on stdout can arrive before stderr has been drained. Wait
                // briefly for its reader, but never join it: a descendant may
                // retain the inherited pipe even after the worker exits.
                let _ = self.stderr_done.recv_timeout(Duration::from_millis(100));
                if let Ok(tail) = self.stderr_tail.lock() {
                    let detail = String::from_utf8_lossy(&tail);
                    let detail = detail.trim();
                    if !detail.is_empty() {
                        return Err(anyhow::anyhow!(
                            "{error}\nWorker stderr (last 8 KiB):\n{detail}"
                        ));
                    }
                }
                return Err(error);
            }
        };
        // A well-formed remote application error does not invalidate the transport.
        if let Some(error) = response.get("error") {
            bail!("{}", error);
        }
        Ok(response["result"].clone())
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noisy_stderr_is_drained_and_bounded() {
        let mut worker = Transport::spawn(Command::new("python3").args([
            "-c",
            "import sys; sys.stdin.readline(); sys.stderr.write('x' * 1000000 + 'END_MARKER'); sys.exit(1)",
        ]))
        .unwrap();
        let error = worker.request(json!({})).unwrap_err().to_string();
        assert!(error.ends_with("END_MARKER"));
        assert_eq!(worker.stderr_tail.lock().unwrap().len(), MAX_STDERR);
        assert!(error.len() < MAX_STDERR + 200);
    }

    #[test]
    fn timeout_includes_blocked_stdin_and_closes_transport() {
        let mut worker =
            Transport::spawn(Command::new("python3").args(["-c", "import time; time.sleep(30)"]))
                .unwrap();
        worker.timeout = Duration::from_millis(100);
        let start = std::time::Instant::now();
        let error = worker
            .request(json!({"source": "x".repeat(2 * 1024 * 1024)}))
            .unwrap_err();
        assert!(error.to_string().contains("timeout"));
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(worker.child.lock().unwrap().try_wait().unwrap().is_some());
        assert!(
            worker
                .request(json!({}))
                .unwrap_err()
                .to_string()
                .contains("closed")
        );
    }
}
