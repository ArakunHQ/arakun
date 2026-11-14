//! What sits in front of the sites: Tunnels and their connectors, DNS records,
//! each zone's SSL settings, registered domains and Pages projects, plus the
//! `cloudflared` on this machine against the latest release.

use std::process::Stdio;

use serde_json::Value;

/// A `cloudflared` instance connected to a Tunnel.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Connector {
    pub version: String,
    /// The public IP it connects from.
    pub origin_ip: String,
    /// Unix seconds.
    pub opened: u64,
    /// Cloudflare data centers it holds connections to ("MEX", "DFW").
    pub colos: Vec<String>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Tunnel {
    pub id: String,
    pub name: String,
    /// "healthy", "degraded", "down" or "inactive".
    pub status: String,
    /// Unix seconds its last connector left; 0 when it never had one.
    pub inactive_at: u64,
    /// Configured from the dashboard (true) or a local config file.
    pub remote: bool,
    pub connectors: Vec<Connector>,
    /// The hostnames whose DNS records route to it.
    pub hostnames: Vec<String>,
}

impl Tunnel {
    pub fn up(&self) -> bool {
        matches!(self.status.as_str(), "healthy" | "degraded")
    }
    /// The DNS name a record routes to this Tunnel through.
    pub fn target(&self) -> String {
        format!("{}.cfargotunnel.com", self.id)
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DnsRecord {
    #[serde(default)]
    pub id: String,
    pub kind: String,
    pub name: String,
    pub content: String,
    pub proxied: bool,
}

impl DnsRecord {
    /// The Tunnel id a CNAME routes to.
    pub fn tunnel(&self) -> Option<&str> {
        (self.kind == "CNAME").then(|| self.content.strip_suffix(".cfargotunnel.com")).flatten()
    }
    /// An address record that shows the server's IP to anyone who looks it up.
    pub fn exposes_origin(&self) -> bool {
        matches!(self.kind.as_str(), "A" | "AAAA") && !self.proxied && !private_ip(&self.content)
    }
}

/// Loopback, private and link-local addresses: nothing to hide behind a proxy.
fn private_ip(ip: &str) -> bool {
    match ip.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v)) => v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified(),
        Ok(std::net::IpAddr::V6(v)) => v.is_loopback() || v.is_unspecified() || (v.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// The zone settings that decide how safe and how cached a site is.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ZoneSettings {
    /// "off", "flexible", "full" or "strict".
    pub ssl: String,
    pub always_https: bool,
    /// "1.0", "1.1", "1.2", "1.3".
    pub min_tls: String,
    /// Caching is off for everyone while it's on.
    pub dev_mode: bool,
    /// HSTS sent (Security Headers).
    #[serde(default)]
    pub hsts: bool,
    pub read: bool,
}

impl ZoneSettings {
    /// "Full (strict)", "Flexible"...
    pub fn ssl_label(&self) -> &'static str {
        match self.ssl.as_str() {
            "strict" => "Full (strict)",
            "full" => "Full",
            "flexible" => "Flexible",
            "off" => "Off",
            _ => "Unknown",
        }
    }
    /// Visitors reach Cloudflare encrypted but Cloudflare reaches the origin in plain HTTP (or not at all).
    pub fn weak_ssl(&self) -> bool {
        matches!(self.ssl.as_str(), "off" | "flexible")
    }
}

/// A domain registered through Cloudflare Registrar.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Registration {
    pub name: String,
    /// Unix seconds.
    pub expires: u64,
    pub auto_renew: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PagesProject {
    pub name: String,
    pub domains: Vec<String>,
    /// "github", "gitlab" or "" for direct uploads.
    pub source: String,
    pub repo: String,
    /// The latest deployment: when, which branch, and how it ended.
    pub deployed: u64,
    pub branch: String,
    pub stage: String,
    /// "success", "failure", "active", "canceled", "idle"...
    pub status: String,
}

impl PagesProject {
    pub fn failed(&self) -> bool {
        self.status == "failure"
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct KvNamespace {
    pub title: String,
}

/// `cloudflared` on this machine.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct LocalCloudflared {
    /// The installed version; empty when it isn't installed.
    pub installed: String,
    /// The latest release on GitHub; empty when it couldn't be read.
    pub latest: String,
}

/// `2026.10.4` → (2026, 10, 4); anything after the numbers is ignored.
pub fn version(v: &str) -> (u32, u32, u32) {
    let mut n = v.trim().trim_start_matches('v').split(|c: char| !c.is_ascii_digit()).map(|p| p.parse().unwrap_or(0));
    (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0))
}

/// Whether `v` is older than `latest` (false when either is unknown).
pub fn outdated(v: &str, latest: &str) -> bool {
    !v.is_empty() && !latest.is_empty() && version(v) < version(latest)
}

pub(super) fn local_cloudflared() -> LocalCloudflared {
    let installed = crate::agent::which("cloudflared")
        .and_then(|bin| crate::platform::command(bin).arg("--version").stdin(Stdio::null()).output().ok())
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().nth(2).unwrap_or_default().to_string())
        .unwrap_or_default();
    let latest = crate::platform::command("curl")
        .args(["-sS", "--max-time", "10", "-H", "Accept: application/vnd.github+json", "https://api.github.com/repos/cloudflare/cloudflared/releases/latest"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v["tag_name"].as_str().map(str::to_string))
        .unwrap_or_default();
    LocalCloudflared { installed, latest }
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn time_of(v: &Value) -> u64 {
    v.as_str().and_then(crate::account::parse_rfc3339).unwrap_or(0)
}

fn items(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}

/// One connector per `client_id`: the list holds a row per connection (usually four).
pub(super) fn parse_tunnels(v: &Value) -> Vec<Tunnel> {
    let mut tunnels: Vec<Tunnel> = items(&v["result"])
        .filter(|t| t["deleted_at"].is_null())
        .map(|t| {
            let mut connectors: Vec<(String, Connector)> = Vec::new();
            for c in items(&t["connections"]) {
                let client = str_of(&c["client_id"]);
                let colo = str_of(&c["colo_name"]);
                let i = match connectors.iter().position(|(id, _)| *id == client) {
                    Some(i) => i,
                    None => {
                        connectors.push((
                            client,
                            Connector { version: str_of(&c["client_version"]), origin_ip: str_of(&c["origin_ip"]), opened: time_of(&c["opened_at"]), colos: Vec::new() },
                        ));
                        connectors.len() - 1
                    }
                };
                let conn = &mut connectors[i].1;
                if !colo.is_empty() && !conn.colos.contains(&colo) {
                    conn.colos.push(colo);
                }
            }
            Tunnel {
                id: str_of(&t["id"]),
                name: str_of(&t["name"]),
                status: str_of(&t["status"]),
                inactive_at: time_of(&t["conns_inactive_at"]),
                remote: t["remote_config"].as_bool().unwrap_or(false),
                connectors: connectors.into_iter().map(|(_, c)| c).collect(),
                hostnames: Vec::new(),
            }
        })
        .collect();
    tunnels.sort_by(|a, b| b.up().cmp(&a.up()).then(a.name.cmp(&b.name)));
    tunnels
}

pub(super) fn parse_dns(v: &Value) -> Vec<DnsRecord> {
    let mut records: Vec<DnsRecord> = items(&v["result"])
        .map(|r| DnsRecord { id: str_of(&r["id"]), kind: str_of(&r["type"]), name: str_of(&r["name"]), content: str_of(&r["content"]), proxied: r["proxied"].as_bool().unwrap_or(false) })
        .collect();
    records.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.name.cmp(&b.name)).then(a.kind.cmp(&b.kind)));
    records
}

pub(super) fn parse_settings(v: &Value) -> ZoneSettings {
    let mut s = ZoneSettings { read: true, ..Default::default() };
    for item in items(&v["result"]) {
        let value = item["value"].as_str().unwrap_or_default();
        match item["id"].as_str().unwrap_or_default() {
            "ssl" => s.ssl = value.to_string(),
            "always_use_https" => s.always_https = value == "on",
            "min_tls_version" => s.min_tls = value.to_string(),
            "development_mode" => s.dev_mode = value == "on",
            "security_header" => s.hsts = item["value"]["strict_transport_security"]["enabled"].as_bool().unwrap_or(false),
            _ => {}
        }
    }
    s
}

pub(super) fn parse_registrations(v: &Value) -> Vec<Registration> {
    let mut out: Vec<Registration> = items(&v["result"])
        .map(|d| Registration {
            name: str_of(if d["name"].is_string() { &d["name"] } else { &d["domain_name"] }),
            expires: time_of(&d["expires_at"]),
            auto_renew: d["auto_renew"].as_bool().unwrap_or(false),
        })
        .filter(|r| !r.name.is_empty())
        .collect();
    out.sort_by_key(|r| r.expires);
    out
}

pub(super) fn parse_pages(v: &Value) -> Vec<PagesProject> {
    let mut out: Vec<PagesProject> = items(&v["result"])
        .map(|p| {
            let d = &p["latest_deployment"];
            PagesProject {
                name: str_of(&p["name"]),
                domains: items(&p["domains"]).filter_map(|d| d.as_str().map(str::to_string)).collect(),
                source: str_of(&p["source"]["type"]),
                repo: str_of(&p["source"]["config"]["repo_name"]),
                deployed: time_of(&d["created_on"]),
                branch: str_of(&d["deployment_trigger"]["metadata"]["branch"]),
                stage: str_of(&d["latest_stage"]["name"]),
                status: str_of(&d["latest_stage"]["status"]),
            }
        })
        .collect();
    out.sort_by(|a, b| b.failed().cmp(&a.failed()).then(b.deployed.cmp(&a.deployed)));
    out
}

pub(super) fn parse_kv(v: &Value) -> Vec<KvNamespace> {
    let mut out: Vec<KvNamespace> = items(&v["result"]).map(|n| KvNamespace { title: str_of(&n["title"]) }).collect();
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions() {
        assert!(outdated("2026.9.3", "2026.10.4"));
        assert!(!outdated("2026.10.4", "2026.10.4"));
        assert!(!outdated("2026.11.0", "2026.10.4"));
        assert!(!outdated("", "2026.10.4") && !outdated("2026.9.3", ""));
        assert_eq!(version("v2026.10.2-rc1"), (2026, 10, 2));
    }

    #[test]
    fn tunnels_group_connections_by_connector() {
        let v = json!({ "result": [
            { "id": "t1", "name": "web", "status": "healthy", "connections": [
                { "client_id": "a", "client_version": "2026.9.3", "colo_name": "MEX", "origin_ip": "1.2.3.4" },
                { "client_id": "a", "client_version": "2026.9.3", "colo_name": "DFW", "origin_ip": "1.2.3.4" },
                { "client_id": "a", "client_version": "2026.9.3", "colo_name": "MEX", "origin_ip": "1.2.3.4" },
                { "client_id": "b", "client_version": "2026.10.4", "colo_name": "QRO", "origin_ip": "5.6.7.8" } ] },
            { "id": "t0", "name": "old", "status": "inactive", "connections": [] },
            { "id": "tx", "name": "gone", "status": "inactive", "deleted_at": "2026-01-01T00:00:00Z" } ] });
        let t = parse_tunnels(&v);
        assert_eq!(t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["web", "old"]);
        assert_eq!(t[0].connectors.len(), 2);
        assert_eq!(t[0].connectors[0].colos, vec!["MEX".to_string(), "DFW".to_string()]);
        assert_eq!(t[0].target(), "t1.cfargotunnel.com");
    }

    #[test]
    fn records_and_settings() {
        let tunnel = DnsRecord { id: String::new(), kind: "CNAME".into(), name: "dev.a.mx".into(), content: "abc.cfargotunnel.com".into(), proxied: true };
        assert_eq!(tunnel.tunnel(), Some("abc"));
        let bare = DnsRecord { id: String::new(), kind: "A".into(), name: "a.mx".into(), content: "203.0.113.9".into(), proxied: false };
        assert!(bare.exposes_origin());
        assert!(!DnsRecord { content: "192.168.1.2".into(), ..bare.clone() }.exposes_origin());
        assert!(!DnsRecord { proxied: true, ..bare }.exposes_origin());
        let s = parse_settings(&json!({ "result": [
            { "id": "ssl", "value": "flexible" }, { "id": "always_use_https", "value": "off" },
            { "id": "min_tls_version", "value": "1.0" }, { "id": "development_mode", "value": "on" } ] }));
        assert!(s.weak_ssl() && s.dev_mode && !s.always_https);
        assert_eq!((s.ssl_label(), s.min_tls.as_str()), ("Flexible", "1.0"));
    }

    #[test]
    fn pages_failures_first() {
        let v = json!({ "result": [
            { "name": "ok", "latest_deployment": { "created_on": "2026-10-20T00:00:00Z", "latest_stage": { "name": "deploy", "status": "success" } } },
            { "name": "broken", "domains": ["b.pages.dev"], "source": { "type": "github", "config": { "repo_name": "site" } },
              "latest_deployment": { "created_on": "2026-10-01T00:00:00Z", "deployment_trigger": { "metadata": { "branch": "main" } }, "latest_stage": { "name": "build", "status": "failure" } } } ] });
        let p = parse_pages(&v);
        assert_eq!(p[0].name, "broken");
        assert!(p[0].failed() && p[0].stage == "build" && p[0].repo == "site");
    }
}
