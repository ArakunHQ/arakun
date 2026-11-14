//! Newline-delimited JSON-RPC 2.0 over a child process's stdio.
//!
//! Two dialects share the plumbing: plain JSON-RPC for ACP, and Factory's
//! `stream-jsonrpc`, which tags every message with `type` and API versions and
//! uses string ids.
//!
//! Three plain threads do the blocking I/O (stdin writer, stdout reader, stderr
//! logger). Responses complete oneshot futures; everything the agent initiates
//! arrives on an unbounded channel that a GPUI task drains.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::channel::{mpsc as fmpsc, oneshot};
use serde_json::{Value, json};

use super::AgentKind;
use super::launch::{self, LaunchSpec};
use crate::platform::ProcessTree;

#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub const AUTH_REQUIRED: i64 = -32000;
    pub const DISCONNECTED: i64 = -32099;

    fn disconnected() -> Self {
        RpcError {
            code: Self::DISCONNECTED,
            message: "the agent process is gone".into(),
            data: None,
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(details) = self
            .data
            .as_ref()
            .and_then(|d| d.get("details").or(Some(d)))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != self.message)
        {
            write!(f, ": {details}")?;
        }
        Ok(())
    }
}

pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Closed {
        status: String,
        stderr_tail: String,
    },
}

/// Keyed by the id as text, so numeric and string ids share one table.
type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, RpcError>>>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Acp,
    /// `droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc`.
    Factory,
}

pub const FACTORY_API_VERSION: &str = "1.0.0";
/// What the `@factory/droid-sdk` this client was modeled on sends.
pub const FACTORY_PROTOCOL_VERSION: &str = "1.201.1";

pub struct Connection {
    writer: mpsc::Sender<String>,
    pending: Pending,
    next_id: AtomicU64,
    pid: i32,
    /// Taken by `drop`, which hands it to the reaper thread.
    tree: Option<ProcessTree>,
    exited: Arc<AtomicBool>,
    trace: bool,
    dialect: Dialect,
}

fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A random RFC 4122 v4 UUID.
pub fn uuid_v4() -> String {
    let mut b = crate::platform::random_bytes().unwrap_or_else(|| {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
            ^ (std::process::id() as u128) << 64;
        seed.to_le_bytes()
    });
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

fn trace_enabled() -> bool {
    std::env::var_os("ARAKUN_ACP_TRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

fn clip(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

impl Connection {
    pub fn spawn(
        kind: AgentKind,
        spec: &LaunchSpec,
        cwd: &Path,
        dialect: Dialect,
    ) -> Result<(Connection, fmpsc::UnboundedReceiver<Incoming>)> {
        let mut cmd = crate::platform::agent_command(&spec.program);
        cmd.args(&spec.args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        // So dropping the thread takes down the adapter's own children too (Claude's
        // adapter runs the CLI under node), and so does arakun dying. Its priority is
        // lowered (inherited by the builds and tests it runs) so the window stays
        // responsive while it works.
        ProcessTree::prepare(&mut cmd);
        crate::platform::lower_priority(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {}", spec.display()))?;
        let tree = ProcessTree::adopt(&child);
        let pid = child.id() as i32;
        let stdin = child.stdin.take().context("agent stdin")?;
        let stdout = child.stdout.take().context("agent stdout")?;
        let stderr = child.stderr.take().context("agent stderr")?;
        let trace = trace_enabled();

        let (writer, write_rx) = mpsc::channel::<String>();
        std::thread::Builder::new()
            .name(format!("acp-{}-w", kind.id()))
            .spawn(move || {
                let mut stdin = std::io::BufWriter::new(stdin);
                while let Ok(line) = write_rx.recv() {
                    if stdin.write_all(line.as_bytes()).is_err()
                        || stdin.write_all(b"\n").is_err()
                        || stdin.flush().is_err()
                    {
                        break;
                    }
                }
            })?;

        let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
        {
            let tail = tail.clone();
            let mut log = launch::open_log(kind);
            if let Some(log) = log.as_mut() {
                let _ = writeln!(
                    log,
                    "=== {} pid {pid} in {} ({})",
                    unix_now(),
                    cwd.display(),
                    spec.display()
                );
            }
            std::thread::Builder::new()
                .name(format!("acp-{}-e", kind.id()))
                .spawn(move || {
                    for line in BufReader::new(stderr).lines() {
                        let Ok(line) = line else { break };
                        if let Some(log) = log.as_mut() {
                            let _ = writeln!(log, "{line}");
                        }
                        let mut tail = tail.lock().unwrap();
                        tail.push_back(line);
                        if tail.len() > 30 {
                            tail.pop_front();
                        }
                    }
                })?;
        }

        let pending: Pending = Arc::default();
        let exited = Arc::new(AtomicBool::new(false));
        let (events, events_rx) = fmpsc::unbounded();
        {
            let pending = pending.clone();
            let exited = exited.clone();
            std::thread::Builder::new()
                .name(format!("acp-{}-r", kind.id()))
                .spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { break };
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if trace {
                            eprintln!("<- {}", clip(line, 4000));
                        }
                        let Ok(msg) = serde_json::from_str::<Value>(line) else {
                            continue;
                        };
                        dispatch(msg, &pending, &events);
                    }
                    let status = match child.wait() {
                        Ok(s) => s.to_string(),
                        Err(e) => e.to_string(),
                    };
                    exited.store(true, Ordering::SeqCst);
                    for (_, tx) in pending.lock().unwrap().drain() {
                        let _ = tx.send(Err(RpcError::disconnected()));
                    }
                    // Give the stderr thread a moment to drain the last lines.
                    std::thread::sleep(Duration::from_millis(50));
                    let stderr_tail = tail.lock().unwrap().iter().cloned().collect::<Vec<_>>();
                    let _ = events.unbounded_send(Incoming::Closed {
                        status,
                        stderr_tail: stderr_tail.join("\n"),
                    });
                })?;
        }

        Ok((
            Connection {
                writer,
                pending,
                next_id: AtomicU64::new(1),
                pid,
                tree: Some(tree),
                exited,
                trace,
                dialect,
            },
            events_rx,
        ))
    }

    /// An ACP agent that runs inside arakun (see `workers_ai`): `serve` gets
    /// each line arakun writes and answers through the sink, as over stdio.
    pub fn in_process(
        kind: AgentKind,
        serve: impl FnOnce(mpsc::Receiver<String>, Arc<dyn Fn(Value) + Send + Sync>) + Send + 'static,
    ) -> Result<(Connection, fmpsc::UnboundedReceiver<Incoming>)> {
        let (writer, write_rx) = mpsc::channel::<String>();
        let pending: Pending = Arc::default();
        let exited = Arc::new(AtomicBool::new(false));
        let (events, events_rx) = fmpsc::unbounded();
        let trace = trace_enabled();
        {
            let (pending, exited) = (pending.clone(), exited.clone());
            std::thread::Builder::new()
                .name(format!("acp-{}", kind.id()))
                .spawn(move || {
                    let sink = {
                        let (pending, events) = (pending.clone(), events.clone());
                        Arc::new(move |msg: Value| {
                            if trace {
                                eprintln!("<- {}", clip(&msg.to_string(), 4000));
                            }
                            dispatch(msg, &pending, &events);
                        })
                    };
                    serve(write_rx, sink);
                    exited.store(true, Ordering::SeqCst);
                    for (_, tx) in pending.lock().unwrap().drain() {
                        let _ = tx.send(Err(RpcError::disconnected()));
                    }
                    let _ = events.unbounded_send(Incoming::Closed {
                        status: "stopped".into(),
                        stderr_tail: String::new(),
                    });
                })?;
        }
        Ok((
            Connection {
                writer,
                pending,
                next_id: AtomicU64::new(1),
                pid: 0,
                tree: None,
                exited,
                trace,
                dialect: Dialect::Acp,
            },
            events_rx,
        ))
    }

    fn write(&self, kind: &str, mut msg: Value) -> bool {
        if self.dialect == Dialect::Factory {
            msg["type"] = json!(kind);
            msg["factoryApiVersion"] = json!(FACTORY_API_VERSION);
            msg["factoryProtocolVersion"] = json!(FACTORY_PROTOCOL_VERSION);
        }
        let line = msg.to_string();
        if self.trace {
            eprintln!("-> {}", clip(&line, 4000));
        }
        self.writer.send(line).is_ok()
    }

    pub fn request(
        &self,
        method: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, RpcError>> + 'static {
        let id = match self.dialect {
            Dialect::Acp => json!(self.next_id.fetch_add(1, Ordering::Relaxed)),
            Dialect::Factory => json!(uuid_v4()),
        };
        let key = id_key(&id);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(key.clone(), tx);
        if !self.write(
            "request",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        ) && let Some(tx) = self.pending.lock().unwrap().remove(&key)
        {
            let _ = tx.send(Err(RpcError::disconnected()));
        }
        async move { rx.await.unwrap_or_else(|_| Err(RpcError::disconnected())) }
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.write(
            "notification",
            json!({"jsonrpc": "2.0", "method": method, "params": params}),
        );
    }

    pub fn respond(&self, id: Value, result: Value) {
        self.write(
            "response",
            json!({"jsonrpc": "2.0", "id": id, "result": result}),
        );
    }

    pub fn respond_error(&self, id: Value, code: i64, message: &str) {
        self.write(
            "response",
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        );
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }
}

fn dispatch(msg: Value, pending: &Pending, events: &fmpsc::UnboundedSender<Incoming>) {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    let id = msg.get("id").cloned().filter(|v| !v.is_null());
    match (method, id) {
        (Some(method), Some(id)) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let _ = events.unbounded_send(Incoming::Request { id, method, params });
        }
        (Some(method), None) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let _ = events.unbounded_send(Incoming::Notification { method, params });
        }
        (None, Some(id)) => {
            let Some(tx) = pending.lock().unwrap().remove(&id_key(&id)) else {
                return;
            };
            let result = match msg.get("error") {
                Some(err) if !err.is_null() => Err(RpcError {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string(),
                    data: err.get("data").cloned(),
                }),
                _ => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(result);
        }
        (None, None) => {}
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let Some(tree) = self.tree.take() else { return };
        let exited = self.exited.clone();
        tree.terminate();
        // Agents get a moment to flush their session files; then the group goes.
        let _ = std::thread::Builder::new()
            .name("acp-reaper".into())
            .spawn(move || {
                for _ in 0..20 {
                    if exited.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                tree.kill();
            });
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
