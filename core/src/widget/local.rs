//! The real [`Host`]: requests over the internet, files in one project, and
//! what the app passes in for sessions, notices and tokens.
//!
//! The limits here stand even if the sandbox's checks had a hole: every name
//! a request goes to is resolved here and refused when it points at this
//! computer or a private network (so a public name can't be turned toward
//! the router), redirects stay on the same host, there are no cookies or
//! proxies, and answers stop at 1 MB.

use std::io::Read as _;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use super::sandbox::{
    GitInfo, Host, MAX_ANSWER, MAX_READ, Request, Response, SessionInfo, Token, https_host,
};

const TIMEOUT: Duration = Duration::from_secs(10);
const REDIRECTS: usize = 3;

/// The callbacks the app answers through.
pub type Sessions = Box<dyn FnMut() -> Vec<SessionInfo>>;
pub type Notice = Box<dyn FnMut(&str, &str)>;
pub type Tokens = Box<dyn FnMut(&str) -> Option<String>>;

/// What the app answers for one widget.
pub struct Local {
    /// The project the widget is placed in: `read_file` and `git` look here.
    pub project: PathBuf,
    pub sessions: Sessions,
    pub notice: Notice,
    /// The token for a secret need, from the keyring or a password manager.
    pub token: Tokens,
}

impl Host for Local {
    fn fetch(&mut self, req: &Request) -> Result<Response, String> {
        fetch(req)
    }

    fn read_file(&mut self, path: &str) -> Result<String, String> {
        read_in(&self.project, path)
    }

    fn git(&mut self) -> Option<GitInfo> {
        let branch = crate::git::current_branch(&self.project).unwrap_or_default();
        let head = crate::git::read(&self.project, &["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let (owner, repo) = crate::git::origin_url(&self.project)
            .and_then(|u| owner_repo(&u))
            .unwrap_or_default();
        Some(GitInfo {
            branch,
            head,
            owner,
            repo,
        })
    }

    fn sessions(&mut self) -> Vec<SessionInfo> {
        (self.sessions)()
    }

    fn notice(&mut self, title: &str, text: &str) {
        (self.notice)(title, text)
    }

    fn token(&mut self, secret: &str) -> Token {
        (self.token)(secret).map_or(Token::Missing, Token::Found)
    }
}

/// `owner` and `repo` from a remote URL: `git@github.com:o/r.git`,
/// `https://host/o/r`.
fn owner_repo(url: &str) -> Option<(String, String)> {
    let path = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit([':', '/'])
        .take(2)
        .collect::<Vec<_>>();
    match path[..] {
        [repo, owner] if !repo.is_empty() && !owner.is_empty() => {
            Some((owner.to_string(), repo.to_string()))
        }
        _ => None,
    }
}

/// Reads `rel` in `project`, after the sandbox checked it is in the grant:
/// still never through a link out of the project, never a secret file, and
/// 256 KB at most.
pub fn read_in(project: &Path, rel: &str) -> Result<String, String> {
    super::manifest::check_file(rel)?;
    let root = dunce::canonicalize(project).map_err(|e| format!("The project: {e}"))?;
    let path = dunce::canonicalize(root.join(rel)).map_err(|_| format!("{rel} is not there"))?;
    let inside = path
        .strip_prefix(&root)
        .map_err(|_| format!("{rel} leads out of the project"))?;
    if crate::secrets::secret_path(inside) {
        return Err(format!("{rel} leads to a secret file"));
    }
    let file = std::fs::File::open(&path).map_err(|e| format!("{rel}: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{rel} is not a file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_READ + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{rel}: {e}"))?;
    if bytes.len() as u64 > MAX_READ {
        return Err(format!("{rel} is larger than 256 KB"));
    }
    String::from_utf8(bytes).map_err(|_| format!("{rel} is not text"))
}

/// Resolves as the default resolver does, then refuses any name with an
/// address that is not on the public internet. ureq connects to what this
/// returns, so the check and the connection see the same addresses.
#[derive(Debug)]
struct PublicOnly;

impl Resolver for PublicOnly {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let host = uri.host().ok_or(ureq::Error::HostNotFound)?;
        let port = uri.port_u16().unwrap_or(443);
        let addrs: Vec<SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(ureq::Error::Io)?
            .collect();
        if addrs.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        if let Some(a) = addrs.iter().find(|a| !public(a.ip())) {
            return Err(ureq::Error::Io(std::io::Error::other(format!(
                "{host} points at {}, which is not on the public internet",
                a.ip()
            ))));
        }
        let mut out = self.empty();
        for a in addrs.into_iter().take(16) {
            out.push(a);
        }
        Ok(out)
    }
}

/// Whether `ip` is a public internet address: not this computer, a private
/// or shared network, link-local, multicast or reserved.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                // shared address space (carrier NAT)
                || (a == 100 && (64..128).contains(&b))
                // benchmarking
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // unique local fc00::/7
                || (first & 0xfe00) == 0xfc00
                // link-local fe80::/10
                || (first & 0xffc0) == 0xfe80
                // documentation 2001:db8::/32
                || (first == 0x2001 && v6.segments()[1] == 0x0db8)
                // NAT64 and 6to4 can lead to IPv4 private addresses
                || first == 0x0064
                || first == 0x2002)
        }
    }
}

fn agent() -> ureq::Agent {
    // A new agent per request: no cookie jar or connection is shared
    // between widgets, or kept from one refresh to the next.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .https_only(true)
        .max_redirects(0)
        .proxy(None)
        .user_agent(concat!(
            "Arakun-Widget/",
            env!("ARAKUN_VERSION"),
            " (+https://github.com/ArakunHQ/arakun)"
        ))
        .build();
    ureq::Agent::with_parts(config, DefaultConnector::new(), PublicOnly)
}

/// Sends a checked request. A redirect is followed only to the same host.
pub fn fetch(req: &Request) -> Result<Response, String> {
    let agent = agent();
    let mut url = req.url.clone();
    for _ in 0..=REDIRECTS {
        let mut resp = send(&agent, req, &url)?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("{} redirected nowhere", req.host))?;
            let next = if location.starts_with('/') && !location.starts_with("//") {
                format!("https://{}{location}", req.host)
            } else {
                location.to_string()
            };
            if https_host(&next).ok().as_deref() != Some(&req.host) {
                return Err(format!("{} redirected to another host", req.host));
            }
            url = next;
            continue;
        }
        let body = resp
            .body_mut()
            .with_config()
            .limit(MAX_ANSWER as u64)
            .read_to_vec()
            .map_err(|e| format!("{}: {e}", req.host))?;
        return Ok(Response { status, body });
    }
    Err(format!("{} redirected too many times", req.host))
}

fn send(
    agent: &ureq::Agent,
    req: &Request,
    url: &str,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let fail = |e: ureq::Error| format!("{}: {e}", req.host);
    macro_rules! with_headers {
        ($b:expr) => {{
            let mut b = $b;
            for (k, v) in &req.headers {
                b = b.header(k, v);
            }
            b
        }};
    }
    match req.method.as_str() {
        "POST" => with_headers!(agent.post(url))
            .send(req.body.clone().unwrap_or_default())
            .map_err(fail),
        "HEAD" => with_headers!(agent.head(url)).call().map_err(fail),
        _ => with_headers!(agent.get(url)).call().map_err(fail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_refused() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "140.82.112.3", "2606:4700::1111"] {
            assert!(public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn a_name_for_this_computer_is_refused() {
        let err = PublicOnly
            .resolve(
                &"https://localhost/".parse().unwrap(),
                &ureq::Agent::config_builder().build(),
                NextTimeout::default(),
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("not on the public internet"),
            "{err}"
        );
    }

    #[test]
    fn files_stay_in_the_project() {
        let dir = std::env::temp_dir().join(format!("arakun-widget-read-{}", std::process::id()));
        let project = dir.join("p");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("vercel.json"), "{}").unwrap();
        std::fs::write(dir.join("outside.txt"), "no").unwrap();
        std::fs::write(project.join("big.txt"), vec![b'a'; MAX_READ as usize + 1]).unwrap();
        assert_eq!(read_in(&project, "vercel.json").unwrap(), "{}");
        assert!(read_in(&project, "../outside.txt").is_err());
        assert!(read_in(&project, "big.txt").is_err());
        assert!(read_in(&project, "missing.json").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("outside.txt"), project.join("link.txt")).unwrap();
            assert!(
                read_in(&project, "link.txt")
                    .unwrap_err()
                    .contains("out of")
            );
            std::fs::write(project.join(".env"), "SECRET=1").unwrap();
            std::os::unix::fs::symlink(project.join(".env"), project.join("env.txt")).unwrap();
            assert!(read_in(&project, "env.txt").unwrap_err().contains("secret"));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remotes_give_owner_and_repo() {
        for url in [
            "git@github.com:ArakunHQ/arakun.git",
            "https://github.com/ArakunHQ/arakun",
            "https://github.com/ArakunHQ/arakun.git/",
        ] {
            assert_eq!(
                owner_repo(url),
                Some(("ArakunHQ".into(), "arakun".into())),
                "{url}"
            );
        }
    }
}
