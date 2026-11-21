//! `arakun-server`: Arakun's server alone, for a machine with no display. See
//! `core/src/server.rs`, which is all of it; this file only starts it.
//!
//!   cargo build --release --locked -p arakun-server
//!   arakun-server help

use arakun_core::{host, server};

fn main() {
    host::set(host::Host {
        github_read_only: "GitHub is read-only on this server (ARAKUN_GITHUB=readonly): nothing was sent.",
        ..host::Host::default()
    });
    server::set_name("arakun-server");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("arakun-server {}", env!("ARAKUN_VERSION"));
        return;
    }
    std::process::exit(server::cli(&args));
}
