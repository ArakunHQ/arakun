//! Cloudflare, read through the `cf` CLI's sign-in, with the official REST v4
//! and GraphQL Analytics APIs (what Cloudflare's SDKs wrap). The token comes
//! straight from `cf`'s profile file; `cf auth whoami` (seconds: it starts Node)
//! only runs the first time, when the token has expired, or when Cloudflare
//! refuses it, since it refreshes the token. Calls share one HTTP client that
//! keeps its connections open. What the last load found is kept on disk, so the
//! tab shows it at once and reads the account again behind it.

mod advice;
mod extra;
pub mod fix;
mod local;
mod network;
mod parse;
pub mod tail;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Value, json};

pub use advice::*;
pub use extra::*;
pub use local::*;
pub use network::*;
use parse::*;

const API: &str = "https://api.cloudflare.com/client/v4";
pub const DASH: &str = "https://dash.cloudflare.com";
pub const MCP_URL: &str = "https://mcp.cloudflare.com/mcp";
/// Every read permission the tab uses, plus the writes granted before (WAF
/// rules, DNS, redirects, zone settings, email routing) so asking again doesn't take them away. `cf` takes
/// them as separate arguments and refuses names outside its own list.
pub const SCOPES: &str = "user:read account:read zone:read dns_records:read dns_settings:read dns_analytics:read \
workers:read workers_builds:read workers_deployments:read workers_observability:read workers_tail:read logpush:read \
pages:read auditlogs:read notification:read ai:read aig:read ai-search:read argotunnel.read teams-connector-cloudflared.read \
teams-connector-cloudflared.monitoring registrar:read registrar-domains.read ssl-and-certificates.read zone-settings.read \
d1.read d1.metadata_read containers.read secrets_store:read workers-kv-storage.read workers-kv-storage.metadata_read \
workers-r2.read workers-r2.metadata_read queues.read pipelines:read analytics.read account-analytics.read healthcheck.read \
page-rules.read cache-settings.read access.read teams:read memberships.read zone-security-center-insights.read \
account-security-center-insights.read firewall-services.read account-rulesets.read logs.read images:read stream.read \
vectorize.read workers-routes.read account-settings.read bot-management.read email-security-dmarcreports.read \
zone-waf.read zone-waf.write dns.write dynamic-redirect.read dynamic-redirect.write zone-settings.write \
email-routing-rule.read email-routing-rule.write email-routing-address.read email-routing-address.write";

/// The command that asks Cloudflare for [`SCOPES`] on the `cf` profile in use
/// (`cf auth login` would write a new default profile instead).
pub fn access_command(profile: &str) -> String {
    if profile.is_empty() {
        format!("cf auth login --scopes {SCOPES}")
    } else {
        format!("cf auth create {profile} --scopes {SCOPES}")
    }
}

/// Days of zone traffic the Domains page charts.
const DAYS: u64 = 7;

// The status queries keep to visitor traffic: a Worker's Cache API lookup that
// misses is logged as a 504 (requestSource edgeWorkerCacheAPI) though the Worker
// only sees "not cached".
const ZONE_QUERY: &str = "query($ids:[String!],$since:Date!,$from:Time!,$to:Time!){viewer{zones(filter:{zoneTag_in:$ids}){zoneTag \
httpRequests1dGroups(limit:10,filter:{date_geq:$since},orderBy:[date_ASC]){dimensions{date} sum{requests cachedRequests threats bytes} uniq{uniques}} \
statuses: httpRequestsAdaptiveGroups(limit:40,filter:{datetime_geq:$from,datetime_leq:$to,requestSource:\"eyeball\"},orderBy:[count_DESC]){count dimensions{edgeResponseStatus}} \
failing: httpRequestsAdaptiveGroups(limit:6,filter:{datetime_geq:$from,datetime_leq:$to,requestSource:\"eyeball\",edgeResponseStatus_geq:500},orderBy:[count_DESC]){count dimensions{clientRequestHTTPHost clientRequestPath edgeResponseStatus}} \
bad_hosts: httpRequestsAdaptiveGroups(limit:30,filter:{datetime_geq:$from,datetime_leq:$to,requestSource:\"eyeball\",edgeResponseStatus_geq:500},orderBy:[count_DESC]){count dimensions{clientRequestHTTPHost}}}}}";

const ACCOUNT_QUERY: &str = "query($a:String!,$from:Time!,$to:Time!,$week:Date!){viewer{accounts(filter:{accountTag:$a}){ \
workers: workersInvocationsAdaptive(limit:200,filter:{datetime_geq:$from,datetime_leq:$to},orderBy:[sum_requests_DESC]){sum{requests errors subrequests} quantiles{cpuTimeP50 cpuTimeP99} dimensions{scriptName}} \
r2: r2StorageAdaptiveGroups(limit:200,filter:{datetime_geq:$from,datetime_leq:$to},orderBy:[max_payloadSize_DESC]){max{payloadSize objectCount} dimensions{bucketName}} \
r2ops: r2OperationsAdaptiveGroups(limit:500,filter:{datetime_geq:$from,datetime_leq:$to}){sum{requests} dimensions{bucketName actionType}} \
d1: d1AnalyticsAdaptiveGroups(limit:100,filter:{date_geq:$week}){sum{readQueries writeQueries rowsRead rowsWritten} dimensions{databaseId}}}}}";

#[derive(Clone, Debug)]
pub enum LoadError {
    /// The `cf` CLI isn't installed.
    NoCli,
    /// `cf` has no sign-in (or it can't be refreshed).
    SignedOut(String),
    Failed(String),
}

impl From<crate::guard::Panicked> for LoadError {
    fn from(panicked: crate::guard::Panicked) -> Self {
        LoadError::Failed(panicked.to_string())
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Day {
    pub requests: u64,
    pub cached: u64,
    pub threats: u64,
    pub bytes: u64,
    pub visitors: u64,
}

/// A URL that answered 5xx in the last 24 hours.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Failing {
    pub host: String,
    pub path: String,
    pub status: u16,
    pub count: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Zone {
    pub id: String,
    pub name: String,
    pub plan: String,
    pub status: String,
    pub days: Vec<Day>,
    /// Requests of the last 24 hours by response status.
    pub statuses: Vec<(u16, u64)>,
    pub failing: Vec<Failing>,
    /// The last 24 hours' 5xx by hostname.
    pub bad_hosts: Vec<(String, u64)>,
    /// Shortest names first; empty when they couldn't be read.
    pub dns: Vec<DnsRecord>,
    pub settings: ZoneSettings,
    /// Edge certificates, the soonest to expire first.
    pub certs: Vec<Cert>,
    /// None when it couldn't be read.
    pub email_enabled: Option<bool>,
    pub email_rules: Vec<EmailRule>,
}

impl Zone {
    pub fn requests(&self) -> u64 {
        self.days.iter().map(|d| d.requests).sum()
    }
    pub fn threats(&self) -> u64 {
        self.days.iter().map(|d| d.threats).sum()
    }
    pub fn bytes(&self) -> u64 {
        self.days.iter().map(|d| d.bytes).sum()
    }
    /// Share of requests served from Cloudflare's cache, 0–100.
    pub fn cache_pct(&self) -> u64 {
        (self.days.iter().map(|d| d.cached).sum::<u64>() * 100)
            .checked_div(self.requests())
            .unwrap_or(0)
    }
    pub fn visitors_per_day(&self) -> u64 {
        if self.days.is_empty() {
            0
        } else {
            self.days.iter().map(|d| d.visitors).sum::<u64>() / self.days.len() as u64
        }
    }
    /// Requests of the last 24 hours.
    pub fn served(&self) -> u64 {
        self.statuses.iter().map(|(_, n)| n).sum()
    }
    /// The last 24 hours' requests by class: 1xx, 2xx, 3xx, 4xx, 5xx.
    pub fn classes(&self) -> [u64; 5] {
        let mut c = [0; 5];
        for &(status, n) in &self.statuses {
            if let 100..=599 = status {
                c[usize::from(status / 100 - 1)] += n;
            }
        }
        c
    }
    pub fn server_errors(&self) -> u64 {
        self.classes()[4]
    }
    /// 5xx statuses, most frequent first.
    pub fn server_statuses(&self) -> Vec<(u16, u64)> {
        let mut s: Vec<(u16, u64)> = self
            .statuses
            .iter()
            .copied()
            .filter(|(c, _)| *c >= 500)
            .collect();
        s.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        s
    }
    /// Whether a DNS record (or a wildcard) still answers for `host` (a port is ignored).
    /// True when the records weren't read, so nothing is taken for gone.
    pub fn resolves(&self, host: &str) -> bool {
        let host = host.split(':').next().unwrap_or(host);
        if self.dns.is_empty() {
            return true;
        }
        self.dns.iter().any(|r| {
            r.name == host
                || r.name.strip_prefix("*.").is_some_and(|base| {
                    host.strip_suffix(base).is_some_and(|rest| {
                        rest.ends_with('.') && !rest[..rest.len() - 1].contains('.')
                    })
                })
        })
    }

    /// 5xx of the last 24 hours from hostnames that no longer resolve: fixed
    /// already, still in the window.
    pub fn gone_errors(&self) -> u64 {
        self.bad_hosts
            .iter()
            .filter(|(h, _)| !self.resolves(h))
            .map(|(_, n)| n)
            .sum()
    }

    /// 5xx that still matter: those of hostnames that still answer.
    pub fn live_server_errors(&self) -> u64 {
        self.server_errors().saturating_sub(self.gone_errors())
    }

    /// Whether `host` belongs to this zone.
    pub fn covers(&self, host: &str) -> bool {
        host == self.name
            || host
                .strip_suffix(&self.name)
                .is_some_and(|rest| rest.ends_with('.'))
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ErrorSample {
    pub message: String,
    /// Unix seconds.
    pub when: u64,
    /// `GET /path`, or the cron/queue that ran.
    pub trigger: String,
    pub url: String,
}

/// A resource a Worker is bound to.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Binding {
    /// "D1", "R2", "KV", "Queue"...
    pub kind: String,
    /// The bucket, class, workflow... it points at; the binding name when there's nothing better.
    pub target: String,
    /// The D1 database id.
    pub id: Option<String>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Worker {
    pub name: String,
    /// Unix seconds of the last deploy.
    pub deployed: u64,
    /// "wrangler", "dash", ...
    pub deployed_from: String,
    pub domains: Vec<String>,
    pub errors: u64,
    /// Newest first.
    pub samples: Vec<ErrorSample>,
    /// Last 24 hours.
    pub requests: u64,
    pub subrequests: u64,
    /// CPU time per request in milliseconds, median and 99th percentile.
    pub cpu_p50: f64,
    pub cpu_p99: f64,
    pub bindings: Vec<Binding>,
    /// Who deployed it last.
    pub author: String,
    pub crons: Vec<String>,
}

impl ErrorSample {
    /// The request path, without query.
    pub fn path(&self) -> &str {
        let rest = match self.url.split_once("://") {
            Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
            None => self.trigger.split_once(' ').map_or("", |(_, p)| p),
        };
        rest.split(['?', '#']).next().unwrap_or_default()
    }

    /// Whether a scanner looking for secrets, git or WordPress files sent it.
    pub fn is_probe(&self) -> bool {
        probe_path(self.path())
    }
}

/// Paths scanners try on every site: dotfiles, PHP and WordPress, dumps and
/// config files with secrets.
pub fn probe_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    const ENDS: [&str; 7] = [".php", ".sql", ".env", ".bak", ".ini", ".yml", ".yaml"];
    const FILES: [&str; 12] = [
        "/credentials.json",
        "/secrets.json",
        "/auth.json",
        "/service-account.json",
        "/aws-exports.js",
        "/runtime-config.js",
        "/env-config.js",
        "/app.config.js",
        "/config.json",
        "/config.js",
        "/dockerfile",
        "/config.env",
    ];
    (p.starts_with("/.") && !p.starts_with("/.well-known/"))
        || p.contains("/.env")
        || p.contains("/.git")
        || p.starts_with("/wp-")
        || p.starts_with("/appsettings")
        || ENDS.iter().any(|e| p.ends_with(e))
        || FILES.contains(&p.as_str())
}

impl Worker {
    /// Whether nearly all its sampled errors come from scanners.
    pub fn bot_noise(&self) -> bool {
        let probes = self.samples.iter().filter(|s| s.is_probe()).count();
        probes > 0 && probes * 10 >= self.samples.len() * 8
    }

    /// The distinct paths scanners asked it for, newest first.
    pub fn probed(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in self.samples.iter().filter(|s| s.is_probe()) {
            let p = s.path().to_string();
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Alert {
    pub name: String,
    pub detail: String,
    pub sent: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Audit {
    pub when: u64,
    pub actor: String,
    pub action: String,
    pub resource: String,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Bucket {
    pub name: String,
    pub created: u64,
    pub size: u64,
    pub objects: u64,
    /// Operations of the last 24 hours: reads (get/head) and the rest (writes, lists).
    pub reads: u64,
    pub writes: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Database {
    pub id: String,
    /// From a wrangler config on this machine, else the binding that uses it.
    pub name: String,
    pub used_by: Vec<String>,
    /// Last 7 days.
    pub reads: u64,
    pub writes: u64,
    pub rows_read: u64,
    pub rows_written: u64,
    /// Bytes on disk and tables, from the D1 list.
    pub size: u64,
    pub tables: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Queue {
    pub name: String,
    pub producers: Vec<String>,
    pub consumers: Vec<String>,
    pub paused: bool,
    pub created: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Flow {
    pub name: String,
    pub script: String,
    pub running: u64,
    pub queued: u64,
    pub errored: u64,
    pub complete: u64,
    /// Unix seconds of the last run.
    pub triggered: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AiCall {
    pub when: u64,
    pub provider: String,
    pub model: String,
    pub status: u16,
    pub ok: bool,
    pub cached: bool,
    pub cost: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Gateway {
    pub id: String,
    /// Requests it has logged in total.
    pub logged: u64,
    /// Newest first.
    pub recent: Vec<AiCall>,
}

impl Gateway {
    pub fn failures(&self) -> usize {
        self.recent.iter().filter(|c| !c.ok).count()
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub email: String,
    /// The `cf` profile the sign-in comes from ("" for an environment token).
    pub profile: String,
    pub account_id: String,
    pub account_name: String,
    pub zones: Vec<Zone>,
    /// Most errors first, then the busiest.
    pub workers: Vec<Worker>,
    pub alerts: Vec<Alert>,
    pub audit: Vec<Audit>,
    /// Largest first.
    pub buckets: Vec<Bucket>,
    pub databases: Vec<Database>,
    pub queues: Vec<Queue>,
    pub flows: Vec<Flow>,
    pub gateways: Vec<Gateway>,
    pub tunnels: Vec<Tunnel>,
    pub registrations: Vec<Registration>,
    pub pages: Vec<PagesProject>,
    pub kv: Vec<KvNamespace>,
    pub cloudflared: LocalCloudflared,
    /// Whether the zones' traffic analytics came back.
    pub traffic: bool,
    /// Whether the Tunnels came back, so DNS records can be checked against them.
    pub tunnels_read: bool,
    /// Whether the account analytics (Worker requests, R2 sizes, D1 queries) came back.
    pub usage: bool,
    /// Security Center findings not dismissed, worst first; `insights_total` counts them all.
    pub insights: Vec<Insight>,
    pub insights_total: u64,
    pub containers: Vec<Container>,
    pub members: Vec<Member>,
    pub access_apps: Vec<AccessApp>,
    pub stream_videos: usize,
    /// Invoices and charges, newest first; None when the sign-in can't read billing.
    pub billing: Option<Vec<Charge>>,
    /// Parts that couldn't be read, to say so instead of showing them empty.
    pub problems: Vec<String>,
    pub fetched: u64,
}

impl Snapshot {
    pub fn errors(&self) -> u64 {
        self.workers.iter().map(|w| w.errors).sum()
    }
    pub fn server_errors(&self) -> u64 {
        self.zones.iter().map(Zone::server_errors).sum()
    }
    pub fn served(&self) -> u64 {
        self.zones.iter().map(Zone::served).sum()
    }
    pub fn r2_bytes(&self) -> u64 {
        self.buckets.iter().map(|b| b.size).sum()
    }
    /// Dollars a month R2 storage costs at Standard rates, after the 10 GB free.
    pub fn r2_monthly(&self) -> f64 {
        (self.r2_bytes() as f64 / 1e9 - 10.).max(0.) * 0.015
    }
    pub fn worker(&self, name: &str) -> Option<&Worker> {
        self.workers.iter().find(|w| w.name == name)
    }
    /// The Workers serving a domain of `zone`.
    pub fn workers_on(&self, zone: &Zone) -> Vec<&Worker> {
        self.workers
            .iter()
            .filter(|w| w.domains.iter().any(|d| zone.covers(d)))
            .collect()
    }
    /// The Workers bound to bucket `name`.
    pub fn bucket_users(&self, name: &str) -> Vec<&str> {
        self.workers
            .iter()
            .filter(|w| {
                w.bindings
                    .iter()
                    .any(|b| b.kind == "R2" && b.target == name)
            })
            .map(|w| w.name.as_str())
            .collect()
    }
    /// The newest `cloudflared` this snapshot knows of: the latest release, else
    /// the newest a connector runs.
    pub fn latest_cloudflared(&self) -> String {
        if !self.cloudflared.latest.is_empty() {
            return self.cloudflared.latest.clone();
        }
        self.tunnels
            .iter()
            .flat_map(|t| &t.connectors)
            .map(|c| c.version.clone())
            .max_by_key(|v| version(v))
            .unwrap_or_default()
    }
    /// The DNS records, across zones, that route to a Tunnel id no longer in the account.
    pub fn dangling_tunnel_records(&self) -> Vec<&DnsRecord> {
        self.zones
            .iter()
            .flat_map(|z| &z.dns)
            .filter(|r| {
                self.tunnels_read
                    && r.tunnel()
                        .is_some_and(|id| self.tunnels.iter().all(|t| t.id != id))
            })
            .collect()
    }
    pub fn tunnel(&self, name: &str) -> Option<&Tunnel> {
        self.tunnels.iter().find(|t| t.name == name)
    }
    pub fn zone(&self, name: &str) -> Option<&Zone> {
        self.zones.iter().find(|z| z.name == name)
    }
    pub fn dash(&self, path: &str) -> String {
        if path.is_empty() {
            format!("{DASH}/{}", self.account_id)
        } else {
            format!("{DASH}/{}/{path}", self.account_id)
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Session {
    #[serde(skip)]
    token: String,
    /// The `cf` profile file holding the token; None for `CLOUDFLARE_API_TOKEN`.
    file: Option<PathBuf>,
    profile: String,
    email: String,
    account_id: String,
    account_name: String,
}

fn cache_dir() -> PathBuf {
    crate::platform::app_dir().join("cache")
}

fn write_json(file: &std::path::Path, v: &impl serde::Serialize) {
    if let Ok(json) = serde_json::to_vec(v) {
        let _ = crate::durable::write(file, &json);
    }
}

/// What the last load found, to show while the next one runs.
pub fn cached() -> Option<Snapshot> {
    serde_json::from_slice(&std::fs::read(cache_dir().join("cloudflare.json")).ok()?).ok()
}

/// Asks `cf` who is signed in; it refreshes an expired token on the way.
fn whoami() -> Result<Session, LoadError> {
    let cf = crate::agent::which("cf").ok_or(LoadError::NoCli)?;
    let out = crate::platform::command(cf)
        .args(["auth", "whoami"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| LoadError::Failed(format!("cf: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = text
        .find('{')
        .and_then(|i| serde_json::from_str(&text[i..]).ok())
        .ok_or_else(|| {
            LoadError::Failed(first_line(&String::from_utf8_lossy(&out.stderr)).to_string())
        })?;
    if v["authenticated"].as_bool() != Some(true) {
        return Err(LoadError::SignedOut(
            v["error"].as_str().unwrap_or("Not logged in").to_string(),
        ));
    }
    let source = v["authSource"].as_str().unwrap_or_default();
    let file = token_file(source);
    let profile = file
        .as_ref()
        .and_then(|f| f.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let account = &v["accounts"][0];
    let mut s = Session {
        token: String::new(),
        file,
        profile,
        email: v["email"].as_str().unwrap_or_default().to_string(),
        account_id: account["id"].as_str().unwrap_or_default().to_string(),
        account_name: account["name"].as_str().unwrap_or_default().to_string(),
    };
    s.token = read_token(&s, true)
        .ok_or_else(|| LoadError::SignedOut(format!("Couldn't read the token ({source})")))?;
    write_json(&cache_dir().join("cloudflare-session.json"), &s);
    Ok(s)
}

/// The token in the session's file (or the environment); None when it has
/// expired or is about to, unless `any` (right after `cf` refreshed it).
fn read_token(s: &Session, any: bool) -> Option<String> {
    let token = match &s.file {
        Some(file) => {
            let c: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
            // RFC 3339 times compare as text; a minute of margin.
            let expires = c["expiration_time"].as_str().unwrap_or_default();
            if !any
                && !expires.is_empty()
                && expires[..expires.len().min(19)] <= iso(crate::model::unix_now() + 60)[..19]
            {
                return None;
            }
            c["oauth_token"].as_str().map(str::to_string)
        }
        None => std::env::var("CLOUDFLARE_API_TOKEN").ok(),
    };
    token.filter(|t| !t.trim().is_empty())
}

/// The session the last `whoami` found, with its token if still good: no Node.
fn quick_session() -> Option<Session> {
    let mut s: Session =
        serde_json::from_slice(&std::fs::read(cache_dir().join("cloudflare-session.json")).ok()?)
            .ok()?;
    s.token = read_token(&s, false)?;
    Some(s)
}

/// "OAuth token from /home/me/.config/cloudflare/config/me.json" → the file.
fn token_file(source: &str) -> Option<PathBuf> {
    let (_, path) = source.split_once(" from ")?;
    let path = PathBuf::from(path.trim());
    path.extension()
        .is_some_and(|e| e == "json")
        .then_some(path)
}

fn first_line(s: &str) -> &str {
    s.trim().lines().next().unwrap_or_default()
}

/// One client for every call, so connections (and their TLS) are reused.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(25)))
            .http_status_as_error(false)
            .user_agent("arakun")
            .build()
            .into()
    })
}

/// What Cloudflare answers 401/403 with when the token is no good.
const REFUSED: &str = "Cloudflare refused the sign-in";

fn call(token: &str, method: &str, url: &str, body: Option<&Value>) -> Result<Value, String> {
    let request = ureq::http::Request::builder()
        .method(method)
        .uri(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    let sent = match body {
        Some(b) => request
            .body(b.to_string())
            .map_err(|e| e.to_string())
            .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
        None => request
            .body(())
            .map_err(|e| e.to_string())
            .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
    };
    let mut response = sent?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|_| {
        if status == 401 {
            REFUSED.to_string()
        } else {
            format!("Cloudflare answered {status} with something unexpected")
        }
    })?;
    // REST and GraphQL both put failures in `errors`.
    if let Some(e) = v["errors"].as_array().and_then(|e| e.first()) {
        let message = e["message"]
            .as_str()
            .unwrap_or("Cloudflare refused the request");
        return Err(if status == 401 {
            format!("{REFUSED}: {message}")
        } else {
            message.to_string()
        });
    }
    if v["success"].as_bool() == Some(false) {
        return Err("Cloudflare refused the request".into());
    }
    Ok(v)
}

type Joined<T> = Result<T, String>;
/// A Worker's name with its settings, deployments and schedules.
type ScriptDetails = (String, [Value; 3]);

fn joined<T>(h: std::thread::ScopedJoinHandle<'_, Joined<T>>) -> Joined<T> {
    h.join().unwrap_or_else(|_| Err("crashed".into()))
}

/// Everything the Cloudflare tab shows, read in parallel. Parts that fail are
/// named in `problems`; only a missing sign-in fails the whole load. `roots` are
/// the projects on this machine, whose wrangler configs name the D1 databases.
/// `partial` gets the account as soon as the quick REST answers are in; the
/// analytics (seconds) come in the result. The result is kept for `cached`.
pub fn load(
    roots: &[PathBuf],
    partial: &(dyn Fn(&Snapshot) + Sync),
) -> Result<Snapshot, LoadError> {
    let quick = quick_session();
    let fresh = quick.is_none();
    let s = match quick {
        Some(s) => s,
        None => whoami()?,
    };
    let snap = match load_with(&s, roots, partial) {
        // The token was revoked or replaced since: ask `cf` once.
        Err(LoadError::SignedOut(e)) if !fresh && e.starts_with(REFUSED) => {
            load_with(&whoami()?, roots, partial)?
        }
        other => other?,
    };
    write_json(&cache_dir().join("cloudflare.json"), &snap);
    Ok(snap)
}

fn load_with(
    s: &Session,
    roots: &[PathBuf],
    partial: &(dyn Fn(&Snapshot) + Sync),
) -> Result<Snapshot, LoadError> {
    let now = crate::model::unix_now();
    let a = s.account_id.as_str();
    let t = s.token.as_str();
    let get = move |path: String| call(t, "GET", &format!("{API}{path}"), None);
    let graphql = move |query: &str, vars: Value| {
        call(
            t,
            "POST",
            &format!("{API}/graphql"),
            Some(&json!({ "query": query, "variables": vars })),
        )
    };
    let (from, to) = (iso(now - 86_400), iso(now));
    let errors_query = |view: &str| {
        let mut q = json!({
            "queryId": format!("arakun-errors-{view}"),
            "timeframe": { "from": (now - 86_400) * 1000, "to": now * 1000 },
            "view": view,
            "limit": 100,
            "parameters": {
                "datasets": ["cloudflare-workers"],
                "filters": [{ "key": "$metadata.error", "operation": "exists", "type": "string" }],
            },
        });
        if view == "calculations" {
            q["parameters"]["calculations"] = json!([{ "operator": "count" }]);
            q["parameters"]["groupBys"] =
                json!([{ "type": "string", "value": "$metadata.service" }]);
        }
        q
    };
    let telemetry = format!("{API}/accounts/{a}/workers/observability/telemetry/query");
    let mut snap = Snapshot {
        email: s.email.clone(),
        profile: s.profile.clone(),
        account_id: s.account_id.clone(),
        account_name: s.account_name.clone(),
        fetched: now,
        ..Default::default()
    };
    let (mut d1_list, mut tunnel_list) = (None, None);
    let mut refused = None;
    std::thread::scope(|sc| {
        // The zones first: their ids go to the traffic query.
        let zone_list = get("/zones?per_page=50".into());
        if let Err(e) = &zone_list
            && e.starts_with(REFUSED)
        {
            refused = Some(e.clone());
            return;
        }
        let ids: Vec<String> = zone_list
            .as_ref()
            .map(|v| parse_zones(v).into_iter().map(|z| z.id).collect())
            .unwrap_or_default();
        let traffic = {
            let ids = ids.clone();
            let (from, to) = (from.clone(), to.clone());
            sc.spawn(move || -> Joined<Value> {
                if ids.is_empty() {
                    return Ok(Value::Null);
                }
                graphql(ZONE_QUERY, json!({ "ids": ids, "since": date(now - (DAYS - 1) * 86_400), "from": from, "to": to }))
            })
        };
        let zones = sc.spawn(move || -> Joined<Vec<Zone>> {
            let mut zones = parse_zones(&zone_list?);
            // DNS records, settings, certificates and Email Routing: calls per zone, all at once.
            let details: Vec<[Joined<Value>; 5]> = std::thread::scope(|sc| {
                let handles: Vec<_> = zones
                    .iter()
                    .map(|z| {
                        [
                            "dns_records?per_page=500",
                            "settings",
                            "ssl/certificate_packs?status=all",
                            "email/routing",
                            "email/routing/rules",
                        ]
                        .map(|what| {
                            let id = z.id.clone();
                            sc.spawn(move || get(format!("/zones/{id}/{what}")))
                        })
                    })
                    .collect();
                handles.into_iter().map(|hs| hs.map(joined)).collect()
            });
            for (z, [dns, settings, certs, email, rules]) in zones.iter_mut().zip(details) {
                z.dns = dns.map(|v| parse_dns(&v)).unwrap_or_default();
                z.settings = settings.map(|v| parse_settings(&v)).unwrap_or_default();
                z.certs = certs.map(|v| parse_certs(&v)).unwrap_or_default();
                z.email_enabled = email.ok().and_then(|v| parse_email_enabled(&v));
                z.email_rules = rules.map(|v| parse_email_rules(&v)).unwrap_or_default();
            }
            Ok(zones)
        });
        // Each Worker's settings (bindings), last deploy and crons.
        let scripts = sc.spawn(|| -> Joined<(Value, Vec<ScriptDetails>)> {
            let scripts = get(format!("/accounts/{a}/workers/scripts"))?;
            let names: Vec<String> = scripts["result"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["id"].as_str().map(str::to_string))
                .collect();
            let details = std::thread::scope(|sc| {
                let handles: Vec<_> = names
                    .iter()
                    .map(|n| {
                        let hs = ["settings", "deployments", "schedules"].map(|what| {
                            let n = n.clone();
                            sc.spawn(move || {
                                get(format!("/accounts/{a}/workers/scripts/{n}/{what}"))
                            })
                        });
                        (n.clone(), hs)
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|(n, hs)| (n, hs.map(|h| joined(h).unwrap_or(Value::Null))))
                    .collect()
            });
            Ok((scripts, details))
        });
        let insights = sc.spawn(|| {
            get(format!(
                "/accounts/{a}/security-center/insights?per_page=100"
            ))
        });
        let containers = sc.spawn(|| get(format!("/accounts/{a}/containers/applications")));
        let members = sc.spawn(|| get(format!("/accounts/{a}/members")));
        let access = sc.spawn(|| get(format!("/accounts/{a}/access/apps")));
        let stream = sc.spawn(|| get(format!("/accounts/{a}/stream?limit=1000")));
        let billing = sc.spawn(|| {
            get("/user/billing/history?per_page=12&order=occurred_at&direction=desc".into())
        });
        let usage = sc.spawn(|| {
            graphql(
                ACCOUNT_QUERY,
                json!({ "a": a, "from": from, "to": to, "week": date(now - 7 * 86_400) }),
            )
        });
        let domains = sc.spawn(|| get(format!("/accounts/{a}/workers/domains")));
        let counts = sc.spawn(|| call(t, "POST", &telemetry, Some(&errors_query("calculations"))));
        let events = sc.spawn(|| call(t, "POST", &telemetry, Some(&errors_query("events"))));
        let alerts = sc.spawn(|| {
            get(format!(
                "/accounts/{a}/alerting/v3/history?per_page=25&since={}",
                iso(now - 30 * 86_400)
            ))
        });
        let audit = sc.spawn(|| {
            get(format!(
                "/accounts/{a}/audit_logs?per_page=40&direction=desc"
            ))
        });
        let buckets = sc.spawn(|| get(format!("/accounts/{a}/r2/buckets")));
        let queues = sc.spawn(|| get(format!("/accounts/{a}/queues")));
        let flows = sc.spawn(|| get(format!("/accounts/{a}/workflows?per_page=50")));
        let tunnels = sc.spawn(|| {
            get(format!(
                "/accounts/{a}/cfd_tunnel?is_deleted=false&per_page=100"
            ))
        });
        let registrar = sc.spawn(|| get(format!("/accounts/{a}/registrar/domains")));
        let pages = sc.spawn(|| get(format!("/accounts/{a}/pages/projects")));
        let kv = sc.spawn(|| get(format!("/accounts/{a}/storage/kv/namespaces?per_page=100")));
        let d1 = sc.spawn(|| get(format!("/accounts/{a}/d1/database?per_page=100")));
        let local = sc.spawn(local_cloudflared);
        let gateways = sc.spawn(|| -> Joined<Vec<Gateway>> {
            let list = get(format!("/accounts/{a}/ai-gateway/gateways"))?;
            let ids = list["result"].as_array().into_iter().flatten().filter_map(|g| g["id"].as_str().map(str::to_string));
            Ok(ids
                .map(|id| {
                    let logs = get(format!(
                        "/accounts/{a}/ai-gateway/gateways/{id}/logs?per_page=20&order_by=created_at&order_by_direction=desc"
                    ))
                    .unwrap_or(Value::Null);
                    parse_gateway(&id, &logs)
                })
                .collect())
        });

        let mut problems: Vec<String> = Vec::new();
        // The quick REST answers first, shown as soon as they're in.
        match joined(zones) {
            Ok(z) => snap.zones = z,
            Err(e) => problem(&mut problems, "Domains", e),
        }
        match joined(buckets) {
            Ok(v) => snap.buckets = parse_buckets(&v),
            Err(e) => problem(&mut problems, "R2", e),
        }
        match joined(scripts) {
            Ok((v, details)) => {
                let domains = joined(domains).unwrap_or_else(|e| {
                    problem(&mut problems, "Worker domains", e);
                    Value::Null
                });
                snap.workers = parse_workers(&v, &domains);
                for (name, [settings, deploys, schedules]) in details {
                    if let Some(w) = snap.workers.iter_mut().find(|w| w.name == name) {
                        w.bindings = parse_bindings(&settings);
                        w.author = parse_deploy(&deploys).0;
                        w.crons = parse_crons(&schedules);
                    }
                }
            }
            Err(e) => problem(&mut problems, "Workers", e),
        }
        match joined(alerts) {
            Ok(v) => snap.alerts = parse_alerts(&v),
            Err(e) => problem(&mut problems, "Alerts", e),
        }
        match joined(audit) {
            Ok(v) => snap.audit = parse_audit(&v),
            Err(e) => problem(&mut problems, "Audit log", e),
        }
        match joined(queues) {
            Ok(v) => snap.queues = parse_queues(&v),
            Err(e) => problem(&mut problems, "Queues", e),
        }
        match joined(flows) {
            Ok(v) => snap.flows = parse_flows(&v),
            Err(e) => problem(&mut problems, "Workflows", e),
        }
        match joined(gateways) {
            Ok(g) => snap.gateways = g,
            Err(e) => problem(&mut problems, "AI Gateway", e),
        }
        match joined(tunnels) {
            Ok(v) => tunnel_list = Some(parse_tunnels(&v)),
            Err(e) => problem(&mut problems, "Tunnels", e),
        }
        match joined(registrar) {
            Ok(v) => snap.registrations = parse_registrations(&v),
            Err(e) => problem(&mut problems, "Registrar", e),
        }
        match joined(pages) {
            Ok(v) => snap.pages = parse_pages(&v),
            Err(e) => problem(&mut problems, "Pages", e),
        }
        match joined(kv) {
            Ok(v) => snap.kv = parse_kv(&v),
            Err(e) => problem(&mut problems, "KV", e),
        }
        match joined(d1) {
            Ok(v) => d1_list = Some(v),
            Err(e) => problem(&mut problems, "D1", e),
        }
        match joined(insights) {
            Ok(v) => (snap.insights, snap.insights_total) = parse_insights(&v),
            Err(e) => problem(&mut problems, "Security Center", e),
        }
        match joined(containers) {
            Ok(v) => snap.containers = parse_containers(&v),
            Err(e) => problem(&mut problems, "Containers", e),
        }
        if let Ok(v) = joined(members) {
            snap.members = parse_members(&v);
        }
        if let Ok(v) = joined(access) {
            snap.access_apps = parse_access(&v);
        }
        // cf's OAuth client can't be granted billing:read: this only works for an API token that has it.
        snap.billing = joined(billing).ok().map(|v| parse_charges(&v));
        if let Ok(v) = joined(stream) {
            snap.stream_videos = v["result"].as_array().map_or(0, Vec::len);
        }
        snap.cloudflared = local.join().unwrap_or_default();
        {
            let mut early = snap.clone();
            early.problems = problems.clone();
            finish(&mut early, roots, &d1_list, &tunnel_list);
            partial(&early);
        }

        // Then the analytics, which take seconds.
        match joined(traffic) {
            Ok(v) if !v.is_null() => {
                parse_traffic(&v, &mut snap.zones);
                snap.traffic = true;
            }
            Ok(_) => {}
            Err(e) => problem(&mut problems, "Traffic", e),
        }
        match joined(counts) {
            Ok(v) => apply_error_counts(&v, &mut snap.workers),
            Err(e) => problem(&mut problems, "Worker errors", e),
        }
        if let Ok(v) = joined(events) {
            apply_error_samples(&v, &mut snap.workers);
        }
        match joined(usage) {
            Ok(v) => {
                snap.databases = apply_usage(&v, &mut snap.workers, &mut snap.buckets);
                snap.usage = true;
            }
            Err(e) => problem(&mut problems, "Usage", e),
        }
        snap.problems = problems;
    });
    if let Some(e) = refused {
        return Err(LoadError::SignedOut(e));
    }
    finish(&mut snap, roots, &d1_list, &tunnel_list);
    Ok(snap)
}

/// A zone's last 24 hours in detail: by hour, and its top paths, countries,
/// hostnames and browsers.
#[derive(Clone, Debug, Default)]
pub struct ZoneTraffic {
    /// (unix seconds of the hour, requests).
    pub hours: Vec<(u64, u64)>,
    pub paths: Vec<(String, u64)>,
    pub countries: Vec<(String, u64)>,
    pub hosts: Vec<(String, u64)>,
    pub browsers: Vec<(String, u64)>,
}

const DETAIL_QUERY: &str = "query($z:String!,$from:Time!,$to:Time!){viewer{zones(filter:{zoneTag:$z}){\
paths: httpRequestsAdaptiveGroups(limit:8,filter:{datetime_geq:$from,datetime_leq:$to,requestSource:\"eyeball\"},orderBy:[count_DESC]){count dimensions{v: clientRequestPath}} \
countries: httpRequestsAdaptiveGroups(limit:8,filter:{datetime_geq:$from,datetime_leq:$to,requestSource:\"eyeball\"},orderBy:[count_DESC]){count dimensions{v: clientCountryName}} \
hosts: httpRequestsAdaptiveGroups(limit:8,filter:{datetime_geq:$from,datetime_leq:$to,requestSource:\"eyeball\"},orderBy:[count_DESC]){count dimensions{v: clientRequestHTTPHost}} \
browsers: httpRequestsAdaptiveGroups(limit:8,filter:{datetime_geq:$from,datetime_leq:$to,requestSource:\"eyeball\"},orderBy:[count_DESC]){count dimensions{v: userAgentBrowser}} \
hours: httpRequestsAdaptiveGroups(limit:48,filter:{datetime_geq:$from,datetime_leq:$to,requestSource:\"eyeball\"},orderBy:[datetimeHour_ASC]){count dimensions{v: datetimeHour}}}}}";

/// Blocking: reads one zone's detail, when asked (it costs analytics budget).
pub fn zone_traffic(zone_id: &str) -> Result<ZoneTraffic, String> {
    let s = quick_session()
        .map(Ok)
        .unwrap_or_else(whoami)
        .map_err(|e| match e {
            LoadError::NoCli => "The cf CLI isn't installed".to_string(),
            LoadError::SignedOut(m) | LoadError::Failed(m) => m,
        })?;
    let now = crate::model::unix_now();
    let v = call(
        &s.token,
        "POST",
        &format!("{API}/graphql"),
        Some(
            &json!({ "query": DETAIL_QUERY, "variables": { "z": zone_id, "from": iso(now - 86_400), "to": iso(now) } }),
        ),
    )?;
    let z = &v["data"]["viewer"]["zones"][0];
    let list = |k: &str| -> Vec<(String, u64)> {
        z[k].as_array()
            .into_iter()
            .flatten()
            .map(|g| {
                (
                    g["dimensions"]["v"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    g["count"].as_u64().unwrap_or(0),
                )
            })
            .collect()
    };
    Ok(ZoneTraffic {
        hours: list("hours")
            .into_iter()
            .map(|(h, n)| (crate::account::parse_rfc3339(&h).unwrap_or(0), n))
            .collect(),
        paths: list("paths"),
        countries: list("countries"),
        hosts: list("hosts"),
        browsers: list("browsers"),
    })
}

/// Notes a part that couldn't be read, saying what Cloudflare's rate limit means.
fn problem(problems: &mut Vec<String>, what: &str, e: String) {
    let e = if e.contains("budget depleted") {
        "Cloudflare's analytics rate limit was hit; it resets in about 5 minutes".into()
    } else {
        e
    };
    problems.push(format!("{what}: {e}"))
}

/// What needs several parts together: D1 names, Tunnels against DNS, the order.
fn finish(
    snap: &mut Snapshot,
    roots: &[PathBuf],
    d1_list: &Option<Value>,
    tunnel_list: &Option<Vec<Tunnel>>,
) {
    name_databases(&mut snap.databases, &snap.workers, &d1_names(roots));
    if let Some(v) = d1_list {
        apply_d1_list(v, &mut snap.databases);
    }
    // Without the Tunnel permission Cloudflare answers an empty list, not an
    // error: when DNS routes to Tunnels the list can't be empty.
    match tunnel_list {
        Some(t)
            if t.is_empty()
                && snap
                    .zones
                    .iter()
                    .flat_map(|z| &z.dns)
                    .any(|r| r.tunnel().is_some()) =>
        {
            snap.problems
                .push("Tunnels: this sign-in can't list them; grant read access in Tools".into());
        }
        Some(t) => {
            snap.tunnels = t.clone();
            snap.tunnels_read = true;
        }
        None => {}
    }
    for t in &mut snap.tunnels {
        let target = t.target();
        t.hostnames = snap
            .zones
            .iter()
            .flat_map(|z| &z.dns)
            .filter(|r| r.content == target)
            .map(|r| r.name.clone())
            .collect();
    }
    let zones_without_dns: Vec<String> = snap
        .zones
        .iter()
        .filter(|z| !z.settings.read)
        .map(|z| z.name.clone())
        .collect();
    if !zones_without_dns.is_empty() {
        snap.problems.push(format!(
            "DNS and SSL settings: not readable for {}",
            zones_without_dns.join(", ")
        ));
    }
    snap.workers.sort_by(|x, y| {
        y.errors
            .cmp(&x.errors)
            .then(y.requests.cmp(&x.requests))
            .then(y.deployed.cmp(&x.deployed))
    });
    snap.buckets
        .sort_by(|x, y| y.size.cmp(&x.size).then(x.name.cmp(&y.name)));
}

/// Unix seconds → `2026-10-19`.
pub fn date(t: u64) -> String {
    let (y, m, d) = civil_from_days((t / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Unix seconds → `2026-10-19T03:05:21Z`.
fn iso(t: u64) -> String {
    let s = t % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        date(t),
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(iso(1_791_928_800), "2026-10-13T22:00:00Z");
        assert_eq!(
            crate::account::parse_rfc3339(&iso(1_792_462_588)),
            Some(1_792_462_588)
        );
    }

    #[test]
    fn token_file_comes_from_whoami() {
        assert_eq!(
            token_file("OAuth token from /home/k/.config/cloudflare/config/k.json"),
            Some(PathBuf::from("/home/k/.config/cloudflare/config/k.json"))
        );
        assert_eq!(
            token_file("CLOUDFLARE_API_TOKEN environment variable"),
            None
        );
    }

    #[test]
    fn removed_hosts_stop_counting() {
        let dns = |name: &str| DnsRecord {
            id: "r".into(),
            kind: "CNAME".into(),
            name: name.into(),
            content: "x".into(),
            proxied: true,
        };
        let z = Zone {
            name: "a.dev".into(),
            statuses: vec![(200, 100), (530, 309), (500, 4)],
            bad_hosts: vec![("support.a.dev".into(), 309), ("www.a.dev:443".into(), 4)],
            dns: vec![dns("www.a.dev"), dns("*.cdn.a.dev")],
            ..Default::default()
        };
        assert_eq!(
            (z.server_errors(), z.gone_errors(), z.live_server_errors()),
            (313, 309, 4)
        );
        assert!(z.resolves("img.cdn.a.dev") && !z.resolves("deep.img.cdn.a.dev"));
        assert!(
            Zone {
                dns: vec![],
                ..z.clone()
            }
            .resolves("support.a.dev")
        );
    }

    #[test]
    fn zone_classes_and_hosts() {
        let z = Zone {
            name: "example.com".into(),
            statuses: vec![(200, 10), (404, 5), (530, 7), (504, 2), (0, 1)],
            ..Default::default()
        };
        assert_eq!(z.classes(), [0, 10, 0, 5, 9]);
        assert_eq!(z.served(), 25);
        assert_eq!(z.server_statuses(), vec![(530, 7), (504, 2)]);
        assert!(z.covers("example.com") && z.covers("dev.example.com"));
        assert!(!z.covers("notexample.com"));
    }

    #[test]
    fn scanner_paths() {
        for p in [
            "/.env.bak",
            "/.git/config",
            "/xmlrpc.php",
            "/wp-login.php",
            "/database.sql",
            "/appsettings.Production.json",
            "/Dockerfile",
            "/config.yaml",
            "/api/.env",
        ] {
            assert!(probe_path(p), "{p}");
        }
        for p in [
            "/",
            "/about",
            "/rss.xml",
            "/.well-known/security.txt",
            "/_next/static/chunks/a.js",
            "/a-benchmark-changed-two-products",
        ] {
            assert!(!probe_path(p), "{p}");
        }
        let sample = |url: &str| ErrorSample {
            url: url.into(),
            trigger: "GET".into(),
            ..Default::default()
        };
        assert_eq!(sample("https://www.example.org/.env?x=1").path(), "/.env");
        assert_eq!(
            ErrorSample {
                trigger: "POST /xmlrpc.php".into(),
                ..Default::default()
            }
            .path(),
            "/xmlrpc.php"
        );
        let mut w = Worker {
            samples: vec![
                sample("https://a.dev/.env"),
                sample("https://a.dev/.env"),
                sample("https://a.dev/x.php"),
            ],
            ..Default::default()
        };
        assert!(w.bot_noise());
        assert_eq!(w.probed(), vec!["/.env".to_string(), "/x.php".to_string()]);
        w.samples.push(sample("https://a.dev/posts/1"));
        assert!(!w.bot_noise());
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "reaches Cloudflare with this machine's cf sign-in"]
    fn load_timing() {
        for round in 0..2 {
            let t = std::time::Instant::now();
            let early = std::sync::Mutex::new(None);
            let r = super::load(&[], &|_| {
                early.lock().unwrap().get_or_insert(t.elapsed());
            });
            let s = r.as_ref().map(|s| {
                (
                    s.zones.len(),
                    s.workers.len(),
                    s.traffic,
                    s.usage,
                    s.problems.clone(),
                )
            });
            eprintln!(
                "round {round}: quick part {:?}, all {:?} → {:?}",
                early.lock().unwrap(),
                t.elapsed(),
                s
            );
        }
        let t = std::time::Instant::now();
        eprintln!(
            "cached() {:?} in {:?}",
            super::cached().map(|s| s.zones.len()),
            t.elapsed()
        );
    }
}
