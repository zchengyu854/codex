//! JSON-line client for the collaboration host.
//!
//! The terminal front-end never touches the database or the model adapters directly: it spawns
//! (or reuses) a `codex-collab-core` host and speaks the line protocol described in
//! `docs/core-api.md`. One line per request, one line per response or notification.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// A single message pushed from the host to this client.
#[derive(Debug)]
pub enum HostEvent {
    /// A response to a request, matched by id. `Err` carries the host's error message.
    Response {
        id: u64,
        result: Result<Value, String>,
    },
    /// A `collab/update` notification payload (agent deltas, task changes, resync, …).
    Update(Value),
    /// The host is gone; the reason is safe to show to the user.
    Closed(String),
}

/// A running collaboration host process and its line-protocol connection.
pub struct Host {
    child: Option<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    events: Receiver<HostEvent>,
    next_id: AtomicU64,
    reader: Option<JoinHandle<()>>,
    label: String,
}

impl Host {
    /// Start a host for `state` using the executable `core` and the Codex binary `codex`.
    pub fn spawn(core: &Path, state: &Path, codex: &Path) -> Result<Self> {
        let mut command = Command::new(core);
        command.arg(state).arg(codex);
        let label = format!("{} {}", core.display(), state.display());
        Self::from_command(command, label)
    }

    /// Wrap an arbitrary command as a line-protocol host (used by tests and for future transports).
    pub fn from_command(mut command: Command, label: String) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start collaboration host ({label})"))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, events) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("collab-host-reader".into())
            .spawn(move || {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    match lines.next() {
                        Some(Ok(line)) => {
                            if let Some(event) = classify(&line)
                                && tx.send(event).is_err()
                            {
                                break;
                            }
                        }
                        Some(Err(error)) => {
                            let _ = tx.send(HostEvent::Closed(format!(
                                "host stream read failed: {error}"
                            )));
                            break;
                        }
                        None => {
                            let _ = tx.send(HostEvent::Closed("host process ended".into()));
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            child: Some(child),
            stdin: Mutex::new(Some(stdin)),
            events,
            next_id: AtomicU64::new(1),
            reader: Some(reader),
            label,
        })
    }

    /// Send one request. Returns the id the response will carry.
    pub fn request(&self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        let mut guard = self.stdin.lock().unwrap();
        let stdin = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("collaboration host is closed"))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(id)
    }

    /// Non-blocking read of the next message, if any.
    pub fn try_recv(&self) -> Option<HostEvent> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(HostEvent::Closed("host connection closed".into()))
            }
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Close stdin and wait for the host to exit, killing it if it does not stop in time.
    pub fn shutdown(&mut self) {
        // Closing stdin tells the host to stop the scheduler and cancel running tasks.
        drop(self.stdin.lock().unwrap().take());
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Parse a host output line into an event. Unknown or malformed lines are ignored.
pub fn classify(line: &str) -> Option<HostEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value["method"] == "collab/update" {
        return Some(HostEvent::Update(value["params"].clone()));
    }
    let id = value.get("id")?.as_u64()?;
    if let Some(error) = value.get("error") {
        let message = error["message"]
            .as_str()
            .unwrap_or("unknown host error")
            .to_owned();
        return Some(HostEvent::Response {
            id,
            result: Err(message),
        });
    }
    Some(HostEvent::Response {
        id,
        result: Ok(value.get("result").cloned().unwrap_or(Value::Null)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_distinguishes_notifications_from_responses() {
        assert!(matches!(
            classify(r#"{"method":"collab/update","params":{"kind":"task_changed"}}"#),
            Some(HostEvent::Update(v)) if v["kind"] == "task_changed"
        ));
        assert!(matches!(
            classify(r#"{"id":7,"result":[1,2]}"#),
            Some(HostEvent::Response { id: 7, result: Ok(v) }) if v == serde_json::json!([1, 2])
        ));
        assert!(matches!(
            classify(r#"{"id":3,"error":{"message":"unknown method"}}"#),
            Some(HostEvent::Response { id: 3, result: Err(m) }) if m == "unknown method"
        ));
        assert!(classify("not json").is_none());
        assert!(classify(r#"{"result":true}"#).is_none());
    }

    #[test]
    fn request_and_response_round_trip_through_a_subprocess() {
        // A dependency-free fake host: echoes a fixed result for any id it receives.
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(r#"while IFS= read -r line; do printf '%s\n' '{"id":1,"result":["ok"]}'; done"#);
        let host = Host::from_command(command, "fake".into()).unwrap();
        let id = host.request("session/list", json!({})).unwrap();
        assert_eq!(id, 1);
        let event = wait_event(&host, Duration::from_secs(3)).expect("host replied");
        match event {
            HostEvent::Response { id: 1, result } => {
                assert_eq!(result.unwrap(), json!(["ok"]));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    fn wait_event(host: &Host, timeout: Duration) -> Option<HostEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(event) = host.try_recv() {
                return Some(event);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Opt-in smoke test against the real host. Build it first:
    /// `cargo build --manifest-path collab/Cargo.toml -p codex-collab-core`
    #[test]
    #[ignore = "requires a built codex-collab-core binary"]
    fn real_host_drives_session_and_agent_listing() {
        let core = std::env::var_os("CODEX_COLLAB_CORE")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                let exe = std::env::current_exe().ok()?;
                let debug_dir = exe.parent()?.parent()?;
                let candidate = debug_dir.join("codex-collab-core");
                candidate.exists().then_some(candidate)
            })
            .expect("build codex-collab-core or set CODEX_COLLAB_CORE");
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codex-collab-tui-smoke-{}-{unique}",
            std::process::id()
        ));
        let repo = root.join("repo");
        let state = root.join("state");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init"],
            vec![
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@localhost",
                "add",
                ".",
            ],
            vec![
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@localhost",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ],
        ] {
            let output = Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?} failed");
        }

        let mut host = Host::spawn(&core, &state, Path::new("codex")).unwrap();
        let response = |host: &Host, method: &str, params: Value| {
            let id = host.request(method, params).unwrap();
            match wait_event(host, Duration::from_secs(15)).expect("host replied") {
                HostEvent::Response { id: got, result } => {
                    assert_eq!(got, id);
                    result
                }
                other => panic!("unexpected event: {other:?}"),
            }
        };

        // Empty state: no sessions yet, and an unknown method fails closed.
        assert!(
            response(&host, "session/list", json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(response(&host, "no/such/method", json!({})).is_err());

        let session = response(
            &host,
            "session/create",
            json!({"repository": repo, "validation": [["/usr/bin/true"]]}),
        )
        .unwrap();
        let session_id = session["id"].as_str().unwrap().to_owned();

        let agent = json!({
            "id": "11111111-2222-4333-8444-555555555555",
            "sessionId": session_id,
            "role": "coordinator",
            "model": "planner-model",
            "baseUrl": "https://example.invalid/v1",
            "protocol": "responses",
            "contextBytes": 65536
        });
        response(&host, "agent/configure", agent).unwrap();
        let agents = response(&host, "agent/list", json!({"sessionId": session_id})).unwrap();
        assert_eq!(agents.as_array().unwrap().len(), 1);
        assert_eq!(agents[0]["model"], "planner-model");
        let tasks = response(&host, "task/list", json!({"sessionId": session_id})).unwrap();
        assert!(tasks.as_array().unwrap().is_empty());
        // The approval channel is reachable and fails closed for unknown ids.
        let approvals = response(&host, "approval/list", json!({})).unwrap();
        assert!(approvals.as_array().unwrap().is_empty());
        assert!(
            response(
                &host,
                "approval/decide",
                json!({"approvalId": "missing", "accept": true})
            )
            .is_err()
        );
        // Review/integration endpoints are reachable and fail closed for unknown ids.
        assert!(response(&host, "task/get", json!({"taskId": "missing"})).is_err());
        assert!(
            response(
                &host,
                "task/integrate",
                json!({"taskId": "missing", "expectedCommit": "deadbeef", "review": "ok"})
            )
            .is_err()
        );
        // Context compaction uses the exact session revision and rejects a stale one.
        let events = response(
            &host,
            "event/list",
            json!({"sessionId": session_id, "after": 0, "limit": 1000}),
        )
        .unwrap();
        let revision = events
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["revision"].as_i64())
            .max()
            .unwrap_or(0);
        assert!(
            response(
                &host,
                "context/compact",
                json!({"sessionId": session_id, "summary": "", "expectedRevision": revision})
            )
            .is_err(),
            "an empty summary must be rejected"
        );
        let compacted = response(
            &host,
            "context/compact",
            json!({"sessionId": session_id, "summary": "smoke checkpoint", "expectedRevision": revision}),
        )
        .unwrap();
        let new_revision = compacted["revision"].as_i64().unwrap();
        assert!(
            new_revision > revision,
            "compaction must append a checkpoint"
        );
        assert!(
            response(
                &host,
                "context/compact",
                json!({"sessionId": session_id, "summary": "stale", "expectedRevision": revision})
            )
            .is_err(),
            "a stale revision must be rejected"
        );

        host.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }
}
