//! `arakun-server`: Arakun's server alone, for a machine with no display. See
//! `src/server.rs`, which is all of it; this file only gathers the parts of
//! the app the server runs on, the way the examples do, and stands in for the
//! few it does not need.
//!
//!   cargo build --release --locked -p arakun-server
//!   arakun-server help

#[path = "../../src/accept.rs"]
mod accept;
#[allow(dead_code)]
#[path = "../../src/account.rs"]
mod account;
#[path = "../../src/agent/mod.rs"]
mod agent;
#[allow(dead_code)]
#[path = "../../src/headless/broker.rs"]
mod broker;
#[path = "../../src/durable.rs"]
mod durable;
#[allow(dead_code)]
#[path = "../../src/fetch.rs"]
mod fetch;
#[path = "../../src/guard.rs"]
mod guard;
#[allow(dead_code)]
#[path = "../../src/headless/sandbox.rs"]
mod sandbox;
#[allow(dead_code)]
mod headless {
    // The agent's sandbox and broker; sessions here run as the app's do, outside both.
    pub(crate) use crate::{broker, sandbox};
    pub fn root() -> std::path::PathBuf {
        crate::platform::app_dir().join("headless")
    }
}
mod memory {
    // `arakun mem` is the app's.
    pub fn prompt_for(_: &std::path::Path) -> String {
        String::new()
    }
}
/// What the phone's messages and the server use of the app's model.
mod model {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Status {
        Idle,
        Working,
        NeedsInput,
        Error,
    }

    pub fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}
mod state {
    pub fn data_dir() -> std::path::PathBuf {
        crate::platform::app_dir()
    }
}
mod phone_page {
    // The page a paired device opens, from this package's side of the repository.
    #[derive(rust_embed::RustEmbed)]
    #[folder = "../phone"]
    pub struct Page;
}
#[path = "../../src/caret.rs"]
mod caret;

// What the picker reads from a source (`shorthand`) goes unused here.
#[allow(dead_code)]
#[path = "../../src/clone_source.rs"]
mod clone_source;

#[allow(dead_code)]
#[path = "../../src/highlight.rs"]
mod highlight;
#[allow(dead_code)]
#[path = "../../src/input.rs"]
mod input;
#[allow(dead_code)]
#[path = "../../src/mascot.rs"]
mod mascot;
#[allow(dead_code)]
#[path = "../../src/phone/mod.rs"]
mod phone;
#[allow(dead_code)]
#[path = "../../src/platform.rs"]
mod platform;
#[allow(dead_code)]
#[path = "../../src/redact.rs"]
mod redact;
#[allow(dead_code)]
#[path = "../../src/schedule.rs"]
mod schedule;
#[path = "../../src/server.rs"]
mod server;
#[allow(dead_code)]
#[path = "../../src/signals.rs"]
mod signals;
#[allow(dead_code)]
#[path = "../../src/theme.rs"]
mod theme;
#[allow(dead_code)]
#[path = "../../src/ui.rs"]
mod ui;
#[allow(dead_code)]
#[path = "../../src/zoom.rs"]
mod zoom;

fn main() {
    server::set_name("arakun-server");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("arakun-server {}", env!("ARAKUN_VERSION"));
        return;
    }
    std::process::exit(server::cli(&args));
}
