//! `arakun mcp`: Arakun's MCP server for the agents it starts, on stdio. Its
//! one tool, `request_secret`, goes to the app over the control socket with the
//! token of the chat the server was started for, and waits for the card's
//! outcome there. The protocol is `agent::secret::mcp_reply`. Under `arakun
//! server` it goes to that server's socket (`arakun_core::server::mcp`).

use std::path::PathBuf;

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

/// Serves MCP on stdin and stdout until stdin closes. Started by `arakun
/// server` for its sessions, it answers through that server instead.
pub fn serve() -> i32 {
    if std::env::var_os(secret::SERVER_SOCKET_VAR).is_some() {
        return arakun_core::server::mcp();
    }
    secret::serve_stdio(forward)
}

/// Sends one call to the app and waits for the outcome.
fn forward(token: &str, args: &Value) -> Result<Value, String> {
    let wait = secret::call_wait(args)?;
    let req = Request {
        cmd: "secret".into(),
        args: vec![token.to_string(), args.to_string()],
        cwd: std::env::current_dir().unwrap_or_default(),
        side: false,
    };
    let reply = control::send_within(&control::socket_path(), &req, wait)
        .map_err(|e| format!("could not reach Arakun: {e}"))?;
    if !reply.ok {
        return Err(reply.message);
    }
    serde_json::from_str(&reply.message).map_err(|e| format!("Arakun answered oddly: {e}"))
}
