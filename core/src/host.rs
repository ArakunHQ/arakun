//! What this crate asks of the program it is built into. The app answers with
//! its own memory and control socket; `arakun-server` and the examples have
//! neither and keep the defaults.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

pub struct Host {
    /// What a session started in this folder is told it remembers (`arakun mem`).
    pub memory_prompt: fn(&Path) -> String,
    /// Where the memory is kept. A sandboxed session may read it.
    pub memory_root: fn() -> PathBuf,
    /// The control socket a sandboxed session reaches `arakun` through.
    pub sandbox_socket: fn() -> PathBuf,
    /// Where the `arakun` shim is, when there is one.
    pub shim_dir: fn() -> Option<PathBuf>,
    /// What a GitHub write says when `ARAKUN_GITHUB=readonly` stops it.
    pub github_read_only: &'static str,
    /// How long since the person last used the keyboard or mouse; `None` when the
    /// host can't tell (`arakun-server` has no display to ask).
    pub idle_for: fn() -> Option<Duration>,
    /// How agents start Arakun's MCP server (`request_secret`): the program and
    /// its arguments. `None` where there is none, and then agents are not told of it.
    pub secret_mcp: fn() -> Option<(PathBuf, Vec<String>)>,
}

impl Default for Host {
    fn default() -> Self {
        Host {
            memory_prompt: |_| String::new(),
            memory_root: || crate::paths::store(crate::paths::MEMORY),
            sandbox_socket: || crate::paths::shared(crate::paths::SANDBOX_SOCKET),
            shim_dir: || None,
            github_read_only: "GitHub is read-only here (ARAKUN_GITHUB=readonly): nothing was sent.",
            idle_for: || None,
            secret_mcp: || None,
        }
    }
}

static HOST: OnceLock<Host> = OnceLock::new();

/// Sets the answers once, first thing in `main`. A second call is ignored.
pub fn set(host: Host) {
    let _ = HOST.set(host);
}

pub fn get() -> &'static Host {
    HOST.get_or_init(Host::default)
}

/// Whether GitHub is read-only: nothing is pushed, opened or commented on.
pub fn github_read_only() -> bool {
    std::env::var("ARAKUN_GITHUB").is_ok_and(|v| v == "readonly")
}
