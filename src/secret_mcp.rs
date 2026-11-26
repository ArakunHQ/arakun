//! `arakun mcp`: Arakun's MCP server for the agents it starts, on stdio. Its
//! one tool, `request_secret`, goes to the app over the control socket with the
//! token of the chat the server was started for, and waits for the card's
//! outcome there. The protocol is `agent::secret::mcp_reply`.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::agent::secret;
use crate::control::{self, Request};

/// How agents start this server: the `arakun` shim when there is one, so it
/// works in a sandbox too, or this program.
pub fn command() -> Option<(PathBuf, Vec<String>)> {
    // A headless agent has no window to show the card in.
    if std::env::args().nth(1).as_deref() == Some("agent-host") {
        return None;
    }
    let name = if cfg!(windows) {
        "arakun.exe"
    } else {
        "arakun"
    };
    let program = control::shim_dir()
        .map(|d| d.join(name))
        .filter(|p| p.exists())
        .or_else(|| std::env::current_exe().ok())?;
    Some((program, vec!["mcp".into()]))
}

/// Serves MCP on stdin and stdout until stdin closes.
pub fn serve() -> i32 {
    let token = std::env::var(secret::TOKEN_VAR).unwrap_or_default();
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let write = |out: &Mutex<std::io::Stdout>, v: Value| {
        let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        // A call waits for the user; pings and lists must not wait behind it.
        let is_call = serde_json::from_str::<Value>(&line)
            .is_ok_and(|v| v["method"] == "tools/call" && v.get("id").is_some());
        if is_call {
            let (out, token) = (out.clone(), token.clone());
            std::thread::spawn(move || {
                if let Some(reply) = secret::mcp_reply(&line, |args| forward(&token, args)) {
                    write(&out, reply);
                }
            });
        } else if let Some(reply) = secret::mcp_reply(&line, |_| Err(String::new())) {
            write(&out, reply);
        }
    }
    0
}

/// Sends one call to the app and waits for the outcome, a little longer than
/// the card stays open.
fn forward(token: &str, args: &Value) -> Result<Value, String> {
    if token.is_empty() {
        return Err(
            "Arakun did not start this server for a session, so it has no card to show".into(),
        );
    }
    let timeout = secret::SecretRequest::parse(args)?.timeout;
    let req = Request {
        cmd: "secret".into(),
        args: vec![token.to_string(), args.to_string()],
        cwd: std::env::current_dir().unwrap_or_default(),
        side: false,
    };
    let reply = control::send_within(
        &control::socket_path(),
        &req,
        timeout + Duration::from_secs(60),
    )
    .map_err(|e| format!("could not reach Arakun: {e}"))?;
    if !reply.ok {
        return Err(reply.message);
    }
    serde_json::from_str(&reply.message).map_err(|e| format!("Arakun answered oddly: {e}"))
}
