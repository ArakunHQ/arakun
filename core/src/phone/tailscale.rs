//! Tailscale as the way to the phone: when the computer and the phone are in
//! the same tailnet, `tailscale serve` takes HTTPS at the machine's name with
//! a certificate browsers already trust, and hands the bytes to the phone
//! server on the loopback. The phone then reaches arakun from anywhere, over
//! an encrypted link, and gets a secure origin (what a service worker and
//! push notifications need) without a certificate authority of ours on it.
//!
//! Everything here goes through the `tailscale` command line, which talks to
//! the daemon the user already runs; nothing is linked in.

use serde::Deserialize;

/// The port `serve` takes HTTPS on: the default, so the address carries none.
const HTTPS_PORT: u16 = 443;
/// Where to turn HTTPS certificates on for the tailnet.
pub const ADMIN_DNS_URL: &str = "https://login.tailscale.com/admin/dns";

/// What the daemon says about this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The command line is not here (or not where arakun looks).
    Missing,
    /// Installed but not connected to the tailnet: stopped, logged out, starting.
    Down,
    /// Connected, but the tailnet does not issue HTTPS certificates.
    NoHttps,
    /// Connected and able to serve `https://{name}/`.
    Ready { name: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct StatusJson {
    backend_state: String,
    #[serde(rename = "Self")]
    this: Option<Node>,
    #[serde(default)]
    cert_domains: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Node {
    #[serde(rename = "DNSName", default)]
    dns_name: String,
}

/// Asks the daemon. Runs a process: call it off the main thread.
pub fn status() -> Status {
    let Some(out) = run(&["status", "--json"]) else {
        return Status::Missing;
    };
    // `status` exits 1 while stopped or logged out, with the JSON still printed.
    parse_status(&String::from_utf8_lossy(&out.stdout))
}

fn parse_status(json: &str) -> Status {
    let Ok(s) = serde_json::from_str::<StatusJson>(json) else {
        return Status::Down;
    };
    if s.backend_state != "Running" {
        return Status::Down;
    }
    let name = s
        .this
        .map(|n| n.dns_name.trim_end_matches('.').to_string())
        .unwrap_or_default();
    let certs = s.cert_domains.unwrap_or_default();
    if name.is_empty() || !certs.iter().any(|d| d.eq_ignore_ascii_case(&name)) {
        return Status::NoHttps;
    }
    Status::Ready { name }
}

/// The address phones use once `serve` is on.
pub fn url(name: &str) -> String {
    format!("https://{name}/")
}

/// Points the tailnet's HTTPS at the phone server on the loopback. Kept by the
/// daemon across runs (`--bg`), so it is also how a run that finds it already
/// there leaves it as is. The TLS-terminated TCP mode hands the connection over
/// whole, so the WebSocket and the request headers arrive as the phone sent them.
pub fn serve(port: u16) -> Result<(), String> {
    let out = run(&[
        "serve",
        "--bg",
        &format!("--tls-terminated-tcp={HTTPS_PORT}"),
        &format!("tcp://127.0.0.1:{port}"),
    ])
    .ok_or("Tailscale is not installed")?;
    if out.status.success() {
        Ok(())
    } else {
        Err(tidy(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// Takes the HTTPS port back; other things the user serves stay.
pub fn stop_serving() -> Result<(), String> {
    let out = run(&[
        "serve",
        &format!("--tls-terminated-tcp={HTTPS_PORT}"),
        "off",
    ])
    .ok_or("Tailscale is not installed")?;
    if out.status.success() {
        Ok(())
    } else {
        Err(tidy(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// The CLI's error, one line, without its "error: " prefix.
fn tidy(stderr: &str) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Tailscale refused");
    line.trim_start_matches("error: ")
        .trim_start_matches("Error: ")
        .to_string()
}

/// Runs `tailscale` with `args`; `None` when it is not here.
fn run(args: &[&str]) -> Option<std::process::Output> {
    for program in programs() {
        match crate::platform::command(&program).args(args).output() {
            Ok(out) => return Some(out),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Where the CLI may be: on the PATH, or where the installers put it.
fn programs() -> Vec<String> {
    // Only Windows and macOS add to it.
    #[allow(unused_mut)]
    let mut v = vec!["tailscale".to_string()];
    #[cfg(windows)]
    if let Ok(pf) = std::env::var("ProgramFiles") {
        v.push(format!("{pf}\\Tailscale\\tailscale.exe"));
    }
    #[cfg(target_os = "macos")]
    v.push("/Applications/Tailscale.app/Contents/MacOS/Tailscale".to_string());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_needs_a_running_daemon_and_a_certificate_for_the_name() {
        let running = r#"{"BackendState":"Running","Self":{"DNSName":"desktop.tail1234.ts.net."},"CertDomains":["desktop.tail1234.ts.net"]}"#;
        assert_eq!(
            parse_status(running),
            Status::Ready {
                name: "desktop.tail1234.ts.net".into()
            }
        );
        assert_eq!(
            url("desktop.tail1234.ts.net"),
            "https://desktop.tail1234.ts.net/"
        );

        let stopped = r#"{"BackendState":"Stopped","Self":{"DNSName":"desktop.tail1234.ts.net."},"CertDomains":null}"#;
        assert_eq!(parse_status(stopped), Status::Down);
        let no_certs = r#"{"BackendState":"Running","Self":{"DNSName":"desktop.tail1234.ts.net."},"CertDomains":null}"#;
        assert_eq!(parse_status(no_certs), Status::NoHttps);
        let other_cert = r#"{"BackendState":"Running","Self":{"DNSName":"desktop.tail1234.ts.net."},"CertDomains":["other.ts.net"]}"#;
        assert_eq!(parse_status(other_cert), Status::NoHttps);
        assert_eq!(parse_status("not json"), Status::Down);
    }

    #[test]
    fn errors_come_as_one_plain_line() {
        assert_eq!(
            tidy("error: Tailscale HTTPS is not enabled\n  see https://...\n"),
            "Tailscale HTTPS is not enabled"
        );
        assert_eq!(tidy("\n"), "Tailscale refused");
    }
}
