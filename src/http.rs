//! The HTTP clients Arakun talks to servers with, and the JSON call every
//! provider makes. Providers keep only what their answers mean.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use ureq::config::ConfigBuilder;
use ureq::http::{HeaderMap, request::Builder};
use ureq::typestate::AgentScope;

/// What Arakun calls itself, so a server's logs can tell versions apart.
pub const USER_AGENT: &str = concat!("arakun/", env!("ARAKUN_VERSION"));

/// The settings every client starts from. Only for clients that need more
/// than [`agent`] gives, such as no redirects or no proxy.
pub fn builder() -> ConfigBuilder<AgentScope> {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
}

/// The shared client that gives up on a whole call after `timeout`. Every
/// status comes back as an answer: callers know what theirs mean.
///
/// One client per timeout for the life of the app, so connections (and
/// their TLS) are reused across providers.
pub fn agent(timeout: Duration) -> &'static ureq::Agent {
    static AGENTS: OnceLock<Mutex<Vec<(Duration, &'static ureq::Agent)>>> = OnceLock::new();
    // A panic report goes out through here: a poisoned lock must not stop it.
    let mut agents = AGENTS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some((_, a)) = agents.iter().find(|(t, _)| *t == timeout) {
        return a;
    }
    // Leaked on purpose: the timeouts are constants, so there are only a few.
    let a: &'static ureq::Agent = Box::leak(Box::new(
        builder().timeout_global(Some(timeout)).build().into(),
    ));
    agents.push((timeout, a));
    a
}

/// What came back: the status, the headers and up to the limit of the body.
pub struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
}

impl Reply {
    /// The body as text. Bytes that are not UTF-8 become U+FFFD.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    /// A header's value, when it is there and is text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|h| h.to_str().ok())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Sends `request` with `body` on `agent` and reads up to `limit` bytes of
/// the answer. Err only when nothing usable came back; a cut-short body is an
/// error too, as half a JSON answer would only fail later with a worse message.
pub fn run(
    agent: &ureq::Agent,
    request: Builder,
    body: Option<String>,
    limit: u64,
) -> Result<Reply, String> {
    let sent = match body {
        Some(b) => request
            .body(b)
            .map_err(|e| e.to_string())
            .and_then(|r| agent.run(r).map_err(|e| e.to_string())),
        None => request
            .body(())
            .map_err(|e| e.to_string())
            .and_then(|r| agent.run(r).map_err(|e| e.to_string())),
    }?;
    let (parts, mut body) = sent.into_parts();
    let bytes = body
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("the answer was cut short: {e}"))?;
    Ok(Reply {
        status: parts.status.as_u16(),
        headers: parts.headers,
        bytes,
    })
}

/// A JSON request: `body` goes as JSON and `token`, where there is one, as a
/// bearer token in the header only, never in the address.
pub fn request(method: &str, url: &str, token: Option<&str>) -> Builder {
    let request = ureq::http::Request::builder()
        .method(method)
        .uri(url)
        .header("Content-Type", "application/json");
    match token {
        Some(t) => request.header("Authorization", format!("Bearer {t}")),
        None => request,
    }
}

/// [`run`] with a [`request`]: the call most providers make.
pub fn json_call(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<String>,
    limit: u64,
) -> Result<Reply, String> {
    run(agent, request(method, url, token), body, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A server for one request: answers `answer` and hands back what it got.
    fn serve(answer: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            // Headers, then the body its Content-Length names.
            loop {
                let n = stream.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if got.len() >= end + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&got).to_string()
        });
        (url, handle)
    }

    #[test]
    fn sends_token_and_body_and_reads_any_status() {
        let (url, server) = serve(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 13\r\nRetry-After: 7\r\nConnection: close\r\n\r\n{\"error\":\"x\"}",
        );
        let reply = json_call(
            agent(Duration::from_secs(5)),
            "POST",
            &format!("{url}/path"),
            Some("secret"),
            Some("{\"a\":1}".into()),
            1024,
        )
        .unwrap();
        let got = server.join().unwrap().to_ascii_lowercase();
        assert_eq!(reply.status, 404);
        assert!(!reply.ok());
        assert_eq!(reply.header("retry-after"), Some("7"));
        assert_eq!(reply.text(), "{\"error\":\"x\"}");
        assert!(got.starts_with("post /path "));
        assert!(got.contains("authorization: bearer secret"));
        assert!(got.contains("content-type: application/json"));
        assert!(got.contains(&format!("user-agent: {}", USER_AGENT.to_ascii_lowercase())));
        assert!(got.ends_with("{\"a\":1}"));
    }

    #[test]
    fn a_body_over_the_limit_is_an_error() {
        let (url, server) =
            serve("HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789");
        let reply = json_call(agent(Duration::from_secs(5)), "GET", &url, None, None, 4);
        server.join().unwrap();
        assert!(reply.is_err());
    }

    #[test]
    fn one_client_per_timeout() {
        let a = agent(Duration::from_secs(41));
        assert!(std::ptr::eq(a, agent(Duration::from_secs(41))));
        assert!(!std::ptr::eq(a, agent(Duration::from_secs(42))));
    }
}
