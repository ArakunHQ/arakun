//! Plan limits through CLIProxyAPI: when Claude Code runs on that proxy (or
//! the user typed its address in the Usage tab), the claude.ai and ChatGPT
//! accounts behind it are the ones that hit their 5-hour and weekly limits,
//! not this machine's sign-in. The proxy's management API lists them
//! (`GET /v0/management/auth-files`) with their health, and its `api-call`
//! reads each provider's usage endpoint with the account's own token, so the
//! token never leaves the proxy.
//!
//! The management API needs the proxy's management key. Every wrong or missing
//! key counts toward a 30-minute ban of this address (5 tries), which would
//! also lock out the proxy's own web panel, so nothing is sent without a key
//! and a key the proxy refused is not tried again. The key is saved with the
//! proxy it was typed for and only ever sent there, never in the clear to
//! another machine: switch to another proxy and the tab asks again. The key
//! is kept in the keyring (`secret_store`); its file names only that proxy.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::plan::{self, PlanUsage, Window};

/// Refetch at most this often; `refresh` serves the cache in between.
const STALE: u64 = 5 * 60;

/// ChatGPT's usage endpoint, the one the Codex CLI reads its limits from.
const CODEX_USAGE: &str = "https://chatgpt.com/backend-api/wham/usage";

/// What the proxy's accounts have left.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Proxy {
    /// The management API's base, e.g. "http://127.0.0.1:8317/v0/management".
    pub base: String,
    /// The address was typed in the Usage tab, not read from Claude Code's
    /// `ANTHROPIC_BASE_URL`.
    #[serde(skip)]
    pub typed: bool,
    /// Unix seconds of the last read.
    pub at: u64,
    pub accounts: Vec<Account>,
    /// Why the last read failed as a whole: a refused key, the proxy is down.
    pub error: Option<String>,
    /// Whether a management key is set; without one nothing is read.
    #[serde(skip)]
    pub has_key: bool,
    /// The fingerprint of a key the proxy refused, so it is not sent again.
    #[serde(default)]
    refused: Option<u64>,
}

impl Proxy {
    /// "127.0.0.1:8317": the proxy's address without the management path.
    pub fn host(&self) -> &str {
        self.base
            .split_once("://")
            .map_or(self.base.as_str(), |(_, r)| r)
            .trim_end_matches("/v0/management")
    }

    /// The last read was refused, so only a new key reads again.
    pub fn key_refused(&self) -> bool {
        self.refused.is_some()
    }
}

/// Whose account it is: the providers whose plans have usage windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Claude,
    Codex,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
        }
    }

    fn from_auth(p: &str) -> Option<Self> {
        if p.eq_ignore_ascii_case("claude") {
            Some(Provider::Claude)
        } else if p.eq_ignore_ascii_case("codex") {
            Some(Provider::Codex)
        } else {
            None
        }
    }
}

/// Whether the proxy routes requests to the account now, from `auth-files`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum State {
    #[default]
    Ready,
    /// The proxy holds the account back after a limit or errors, until
    /// `until` (Unix seconds) when it says.
    Cooling { until: Option<u64>, why: String },
    /// Its sign-in on the proxy has run out; only a new sign-in there helps.
    SignIn,
    /// The proxy reports an error but still routes to it.
    Failing { why: String },
}

/// One account the proxy routes to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// The account's email, or its auth file's name.
    pub name: String,
    #[serde(default)]
    pub provider: Provider,
    /// "plus", "pro": the plan the proxy knows of; empty when unknown.
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub state: State,
    /// Its windows; `None` when the read failed or was skipped.
    pub usage: Option<PlanUsage>,
    pub error: Option<String>,
}

impl Account {
    /// Whether it can take requests at `now`: its windows were read, none of
    /// the account-wide ones is full, and the proxy is not holding it back.
    /// A model's own full window leaves room for the other models.
    pub fn has_room(&self, now: u64) -> bool {
        match &self.state {
            State::SignIn => return false,
            State::Cooling { until, .. } if until.is_none_or(|t| t > now) => return false,
            _ => {}
        }
        self.usage
            .as_ref()
            .is_some_and(|u| u.windows.iter().all(|w| !full(w, now)))
    }

    /// When it can take requests again, if it can't now and that is known.
    pub fn back_at(&self, now: u64) -> Option<u64> {
        if self.has_room(now) {
            return None;
        }
        let cooling = match &self.state {
            State::SignIn => return None,
            // Held back with no end the proxy gave: unknown.
            State::Cooling { until: None, .. } => return None,
            State::Cooling { until, .. } => *until,
            _ => None,
        };
        // Every full window must start over before it takes requests again.
        let windows = self.usage.as_ref().map(|u| {
            u.windows
                .iter()
                .filter(|w| full(w, now))
                .map(|w| w.resets_at)
                .collect::<Option<Vec<_>>>()
        });
        let windows = match windows {
            Some(Some(times)) => times.into_iter().max(),
            // A full window without a reset time: unknown.
            Some(None) => return None,
            None => None,
        };
        match (cooling, windows) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
        .filter(|t| *t > now)
    }
}

/// An account-wide window that is at its limit and has not started over
/// since it was read. A model's own window ("Weekly · Opus") is left out:
/// the account still takes requests for the other models.
fn full(w: &Window, now: u64) -> bool {
    !w.one_model() && w.active && w.percent >= 100 && !w.reset_by(now)
}

/// "3 of 4 Claude accounts have room · one is back in 1h 12m": the pool at a
/// glance, for a provider with more than one account.
pub fn summary(accounts: &[Account], provider: Provider, now: u64) -> Option<String> {
    let mine: Vec<&Account> = accounts.iter().filter(|a| a.provider == provider).collect();
    let total = mine.len();
    if total < 2 {
        return None;
    }
    let room = mine.iter().filter(|a| a.has_room(now)).count();
    let name = provider.name();
    if room == total {
        return Some(format!("All {total} {name} accounts have room"));
    }
    let mut text = format!("{room} of {total} {name} accounts have room");
    if let Some(when) = mine
        .iter()
        .filter_map(|a| a.back_at(now))
        .min()
        .and_then(|at| plan::when_text(at, now))
    {
        text.push_str(&format!(" · one is back {when}"));
    }
    Some(text)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The proxy the key was saved for. The name is from when the file held the
/// key too, and stays so a file from before is still found.
fn key_path() -> PathBuf {
    crate::platform::app_dir().join("cliproxy-management-key")
}

/// The keyring entry of the key. Not per test Arakun: its file is shared too.
const KEY_ACCOUNT: &str = "cliproxy-management-key";
const KEY_LABEL: &str = "arakun: CLIProxyAPI management key";

fn cache_path() -> PathBuf {
    crate::platform::app_dir().join("cliproxy-plan.json")
}

fn address_path() -> PathBuf {
    crate::platform::app_dir().join("cliproxy-address")
}

/// The management base the key saved from the Usage tab was typed for. The
/// key is in the keyring; `key` holds it only as a file from before had it,
/// until it is moved there. Never logged or shown.
#[derive(Serialize, Deserialize)]
struct SavedKey {
    base: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    key: String,
}

fn saved_key() -> Option<SavedKey> {
    crate::durable::load_json(key_path())
}

/// Where the key is kept: the keyring, or a fake one in the tests.
trait Keep {
    fn get(&self) -> Result<Option<String>, String>;
    fn set(&self, key: &str) -> Result<(), String>;
    fn forget(&self);
}

struct Keyring;

impl Keep for Keyring {
    fn get(&self) -> Result<Option<String>, String> {
        crate::secret_store::get(KEY_ACCOUNT)
    }

    fn set(&self, key: &str) -> Result<(), String> {
        crate::secret_store::set(KEY_ACCOUNT, KEY_LABEL, key)
    }

    fn forget(&self) {
        crate::secret_store::forget(KEY_ACCOUNT)
    }
}

/// The saved key, only when it was typed for `base`. A key still in the file
/// at `file` is moved to the keyring. `held` is what the keyring gave last
/// time, so the 30-second poll does not ask it again: a locked keyring would
/// show its unlock prompt each time. `force` (Refresh, a saved key) asks it
/// anew. An error means the keyring could not say.
fn key_for(
    saved: Option<SavedKey>,
    base: &str,
    file: &Path,
    held: &mut Held,
    force: bool,
    keep: &impl Keep,
) -> Result<Option<String>, String> {
    let Some(saved) = saved else {
        return Ok(None);
    };
    let in_file = saved.key.trim();
    if !in_file.is_empty() {
        let key = in_file.to_string();
        // A keyring that refused it is asked again only on Refresh, not
        // with each poll's prompt.
        if force || !matches!(held, Some(Err(_))) {
            *held =
                Some(move_to_keyring(&saved.base, &key, file, keep).map(|()| Some(key.clone())));
        }
        return Ok((saved.base == base).then_some(key));
    }
    if saved.base != base {
        return Ok(None);
    }
    if force {
        *held = None;
    }
    held.get_or_insert_with(|| {
        keep.get()
            .map(|k| k.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()))
    })
    .clone()
}

/// Moves the key from the file to the keyring and rewrites the file without
/// it. When the keyring can't take it, it stays in the file and is still used.
fn move_to_keyring(base: &str, key: &str, file: &Path, keep: &impl Keep) -> Result<(), String> {
    if let Err(e) = keep.set(key) {
        eprintln!("arakun: could not move the CLIProxyAPI management key to the keyring: {e}");
        return Err(e);
    }
    if let Err(e) = write_base(file, base) {
        eprintln!("arakun: {e:#}");
    }
    Ok(())
}

/// Writes the file that says which proxy the key is for, without the key.
fn write_base(file: &Path, base: &str) -> Result<()> {
    let saved = SavedKey {
        base: base.to_string(),
        key: String::new(),
    };
    crate::durable::write_private(file, &serde_json::to_vec(&saved)?)?;
    Ok(())
}

/// The management base the address typed in the Usage tab points to.
fn typed_base() -> Option<String> {
    let typed = std::fs::read_to_string(address_path()).ok()?;
    address_base(&typed).ok()
}

/// The proxy to read and whether its address was typed: the typed one over
/// Claude Code's endpoint, since typing it is the more deliberate choice.
fn current_base() -> Option<(String, bool)> {
    if let Some(base) = typed_base() {
        return Some((base, true));
    }
    let base = management_base(&super::claude_auth::endpoint_url()?)?;
    Some((base, false))
}

/// The management base for an address as typed: "127.0.0.1:8317",
/// "https://proxy.example.com/v1". Without a scheme, http only for this
/// machine, so the key is never sent in the clear by default.
fn address_base(typed: &str) -> Result<String> {
    let typed = typed.trim();
    if typed.is_empty() {
        bail!("Type the proxy's address.");
    }
    let url = if typed.contains("://") {
        typed.to_string()
    } else if loopback(typed) {
        format!("http://{typed}")
    } else {
        format!("https://{typed}")
    };
    let scheme = url.split("://").next().unwrap_or_default().to_lowercase();
    if scheme != "http" && scheme != "https" {
        bail!("Use an http or https address.");
    }
    management_base(&url).context("That is not an address.")
}

/// Saves the address typed in the Usage tab and returns its management base.
pub fn save_address(typed: &str) -> Result<String> {
    let base = address_base(typed)?;
    crate::durable::write(&address_path(), typed.trim().as_bytes())?;
    Ok(base)
}

/// What this process last read from the keyring for the key: `None` before
/// the first read, then the key, none, or why the keyring could not say.
type Held = Option<Result<Option<String>, String>>;

/// Held while the key or the cache is read or written, so a read that
/// started with a key cannot write its cache after the key is forgotten.
/// It guards the key as last read from the keyring too.
static FILES: Mutex<Held> = Mutex::new(None);

fn files() -> MutexGuard<'static, Held> {
    FILES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forgets the typed address, the key saved for it and its last read, so the
/// tab goes back to Claude Code's endpoint.
pub fn forget_address() -> Result<()> {
    let mut held = files();
    let base = typed_base();
    remove(&address_path())?;
    if base.is_some_and(|b| saved_key().is_some_and(|k| k.base == b)) {
        remove_key(&key_path(), &cache_path(), &mut held, &Keyring)?;
    }
    Ok(())
}

/// Forgets the management key and what was read with it.
pub fn forget_key() -> Result<()> {
    remove_key(&key_path(), &cache_path(), &mut files(), &Keyring)
}

fn remove_key(file: &Path, cache: &Path, held: &mut Held, keep: &impl Keep) -> Result<()> {
    remove(file)?;
    remove(cache)?;
    keep.forget();
    *held = None;
    Ok(())
}

fn remove(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("couldn't remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Saves the management key for `base`, the proxy the tab showed when the key
/// was typed (not whatever Claude Code runs on by the time this runs): the
/// key in the keyring, the proxy in the file. Off the UI thread: the keyring
/// may wait for the person to unlock it.
pub fn save_key(key: &str, base: &str) -> Result<()> {
    save_key_in(key, base, &key_path(), &mut files(), &Keyring)
}

fn save_key_in(
    key: &str,
    base: &str,
    file: &Path,
    held: &mut Held,
    keep: &impl Keep,
) -> Result<()> {
    let key = key.trim();
    keep.set(key)
        .map_err(|e| anyhow!("The keyring could not keep the management key: {e}"))?;
    write_base(file, base)?;
    *held = Some(Ok(Some(key.to_string())));
    Ok(())
}

/// Whether the key may travel to `base`: over https, or plain http to this
/// machine. Anything else would send it in the clear.
fn safe_to_send(base: &str) -> bool {
    base.starts_with("https://") || (base.starts_with("http://") && loopback(base))
}

/// Whether `base` is on this machine: "localhost", "127.0.0.1:8317", "[::1]".
fn loopback(base: &str) -> bool {
    let rest = base.split_once("://").map_or(base, |(_, r)| r);
    let host = rest.split('/').next().unwrap_or_default();
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.rsplit_once(':').map_or(host, |(h, _)| h),
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// At most `n` characters of `s`, cut on a character boundary.
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The management API's base for Claude Code's `ANTHROPIC_BASE_URL`: the
/// proxy serves both from one origin.
fn management_base(base_url: &str) -> Option<String> {
    let (scheme, rest) = base_url.trim().split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?.trim();
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    (!host.is_empty()).then(|| format!("{}://{host}/v0/management", scheme.to_lowercase()))
}

/// A stable fingerprint of the key (FNV-1a), so the cache can say "this key
/// was refused" without holding the key.
fn fingerprint(key: &str) -> u64 {
    key.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn cached() -> Option<Proxy> {
    crate::durable::load_json(cache_path())
}

fn store(p: &Proxy) {
    if let Ok(json) = serde_json::to_string_pretty(p) {
        let _ = crate::durable::write_private(&cache_path(), json.as_bytes());
    }
}

/// What the views show: `None` without a proxy (no typed address, and Claude
/// Code on its own sign-in). The cache while it is fresh, a read when it is
/// not, or now with `force` (a key was just saved, or Refresh). Reads files
/// and the network; keep it off the UI thread.
pub fn refresh(force: bool) -> Option<Proxy> {
    let mut held = files();
    let (base, typed) = current_base()?;
    let key = key_for(saved_key(), &base, &key_path(), &mut held, force, &Keyring);
    let cache = cached().filter(|c| c.base == base);
    let (mut out, read_now) = match key {
        Ok(key) => decide(base, key.as_deref(), cache, now(), force, read),
        Err(e) => (keyring_failed(base, cache, &e), false),
    };
    if read_now {
        store(&out);
    }
    out.typed = typed;
    Some(out)
}

/// What to show when the keyring could not give the key: the last read, with
/// why. The key counts as set, so the tab offers Refresh, not a new key.
fn keyring_failed(base: String, cache: Option<Proxy>, why: &str) -> Proxy {
    let mut out = cache.unwrap_or_else(|| Proxy {
        base,
        ..Proxy::default()
    });
    out.has_key = true;
    out.error = Some(format!(
        "The keyring could not give the management key: {why}"
    ));
    out
}

/// [`refresh`] without the files: whether to call `read` and what to show.
/// The second value says `read` ran and the cache should be written.
fn decide(
    base: String,
    key: Option<&str>,
    cache: Option<Proxy>,
    at: u64,
    force: bool,
    read: impl FnOnce(&str, &str) -> Result<Vec<Account>, Failure>,
) -> (Proxy, bool) {
    let mut out = cache.unwrap_or_else(|| Proxy {
        base: base.clone(),
        ..Proxy::default()
    });
    out.has_key = key.is_some();
    let Some(key) = key else {
        return (out, false);
    };
    if !safe_to_send(&base) {
        out.error = Some(
            "The management key is only sent over https or to this machine; use an https address for the proxy."
                .into(),
        );
        return (out, false);
    }
    let print = fingerprint(key);
    if out.refused == Some(print) {
        return (out, false);
    }
    let fresh = out.refused.is_none() && at.saturating_sub(out.at) < STALE && out.at > 0;
    if fresh && !force {
        return (out, false);
    }
    out.at = at;
    out.refused = None;
    match read(&base, key) {
        Ok(accounts) => {
            out.accounts = accounts;
            out.error = None;
        }
        Err(Failure::Refused(msg)) => {
            out.refused = Some(print);
            out.error = Some(msg);
            out.accounts.clear();
        }
        // The proxy is down or restarting: keep the last windows on screen.
        Err(Failure::Other(e)) => out.error = Some(format!("{e:#}")),
    }
    (out, true)
}

enum Failure {
    /// The proxy said no to the key; trying it again only brings the ban nearer.
    Refused(String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Failure::Other(e)
    }
}

/// The proxy's accounts and each one's windows.
fn read(base: &str, key: &str) -> Result<Vec<Account>, Failure> {
    read_with(|path, body| management(base, key, path, body))
}

/// [`read`] with the management calls passed in: `call(path, body)`.
fn read_with(
    mut call: impl FnMut(&str, Option<&str>) -> Result<Value, Failure>,
) -> Result<Vec<Account>, Failure> {
    let files = call("/auth-files", None)?;
    let mut accounts = Vec::new();
    for found in listed_accounts(&files) {
        let mut account = found.account;
        // A dead sign-in can't read its usage either; don't spend a call on it.
        if account.state != State::SignIn {
            let body = usage_call(account.provider, &found.index, found.chatgpt.as_deref());
            let got = match call("/api-call", Some(&body)) {
                Ok(answer) => account_usage(account.provider, &answer),
                // The key stopped working: stop, rather than send it once per account.
                Err(refused @ Failure::Refused(_)) => return Err(refused),
                Err(Failure::Other(e)) => Err(e),
            };
            match got {
                Ok(u) => account.usage = Some(u),
                Err(e) => account.error = Some(format!("{e:#}")),
            }
        }
        accounts.push(account);
    }
    Ok(accounts)
}

/// The `api-call` body that reads one account's usage with its own token.
fn usage_call(provider: Provider, index: &str, chatgpt: Option<&str>) -> String {
    let (url, header) = match provider {
        Provider::Claude => (
            plan::ENDPOINT,
            serde_json::json!({
                "Authorization": "Bearer $TOKEN$",
                "anthropic-beta": "oauth-2025-04-20",
            }),
        ),
        Provider::Codex => {
            let mut h = serde_json::json!({ "Authorization": "Bearer $TOKEN$" });
            if let Some(id) = chatgpt {
                h["ChatGPT-Account-Id"] = id.into();
            }
            (CODEX_USAGE, h)
        }
    };
    serde_json::json!({
        "auth_index": index,
        "method": "GET",
        "url": url,
        "header": header,
    })
    .to_string()
}

/// One management call with the key in a header (curl reads it from stdin,
/// so it stays out of the process list).
/// A body makes it a POST.
fn management(base: &str, key: &str, path: &str, body: Option<&str>) -> Result<Value, Failure> {
    let mut headers = vec![("Authorization", format!("Bearer {key}"))];
    if body.is_some() {
        headers.push(("Content-Type", "application/json".into()));
    }
    let url = format!("{base}{path}");
    // A proxy in the environment would see the key in the clear on its way here.
    let (out, status) = plan::http_status(&url, body, &headers, loopback(base))?;
    let json: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    let msg = json["error"].as_str().unwrap_or_default();
    failure(base, status, msg, &out).map_or(Ok(json), Err)
}

/// What a management answer's status and `error` mean, worded for the tab;
/// `None` on success.
fn failure(base: &str, status: u32, msg: &str, body: &str) -> Option<Failure> {
    let lower = msg.to_lowercase();
    Some(match status {
        200..=299 => return None,
        401 => Failure::Refused(
            "CLIProxyAPI refused the management key. Check it and save it again.".into(),
        ),
        403 if lower.contains("banned") => Failure::Other(anyhow!(
            "CLIProxyAPI blocked this address after too many wrong keys. {}",
            msg.split_once(". ").map_or("", |(_, rest)| rest)
        )),
        403 if lower.contains("remote management disabled") => Failure::Other(anyhow!(
            "CLIProxyAPI only takes management calls from its own machine. Set remote-management.allow-remote to true in its config."
        )),
        403 if lower.contains("key not set") => Failure::Other(anyhow!(
            "CLIProxyAPI has no management key. Set remote-management.secret-key in its config."
        )),
        403 => Failure::Other(anyhow!("CLIProxyAPI: {msg}")),
        404 => Failure::Other(anyhow!(
            "No CLIProxyAPI management API at {base}. Is a management key set in its config, and does the address forward /v0/management?"
        )),
        0 => Failure::Other(anyhow!("CLIProxyAPI did not answer at {base}.")),
        s => Failure::Other(anyhow!(
            "CLIProxyAPI: HTTP {s} {}",
            if msg.is_empty() {
                clip(body, 200)
            } else {
                msg.to_string()
            }
        )),
    })
}

/// An account from `auth-files`, with what its usage call needs.
struct Listed {
    account: Account,
    index: String,
    /// The ChatGPT workspace a Codex account reads its usage in.
    chatgpt: Option<String>,
}

/// Every enabled Claude and Codex sign-in in `auth-files`, Claude first.
/// API keys have no plan windows, so they are left out.
fn listed_accounts(files: &Value) -> Vec<Listed> {
    let Some(files) = files["files"].as_array() else {
        return Vec::new();
    };
    let at = now();
    let mut out: Vec<Listed> = files
        .iter()
        .filter(|f| {
            f["disabled"].as_bool() != Some(true) && f["account_type"].as_str() != Some("api_key")
        })
        .filter_map(|f| {
            let provider = f["provider"].as_str().or_else(|| f["type"].as_str())?;
            let provider = Provider::from_auth(provider)?;
            let index = f["auth_index"].as_str()?.to_string();
            let name = ["email", "label", "name"]
                .iter()
                .find_map(|k| f[*k].as_str().filter(|s| !s.is_empty()))
                .unwrap_or(&index)
                .to_string();
            let token = &f["id_token"];
            let text = |v: &Value| {
                v.as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            };
            Some(Listed {
                account: Account {
                    name,
                    provider,
                    plan: text(&token["plan_type"]).unwrap_or_default(),
                    state: state(f, at),
                    usage: None,
                    error: None,
                },
                index,
                chatgpt: text(&token["chatgpt_account_id"]),
            })
        })
        .collect();
    out.sort_by_key(|l| (l.account.provider, l.account.name.to_lowercase()));
    out
}

/// An `auth-files` entry's health: held back, signed out, or failing.
fn state(f: &Value, now: u64) -> State {
    let why = clip(f["status_message"].as_str().unwrap_or_default().trim(), 160);
    if f["requires_sign_in"].as_bool() == Some(true) {
        return State::SignIn;
    }
    if f["unavailable"].as_bool() == Some(true) || f["quota_blocked"].as_bool() == Some(true) {
        let until = f["next_retry_after"]
            .as_str()
            .and_then(plan::from_iso)
            .filter(|t| *t > now);
        return State::Cooling { until, why };
    }
    if f["status"].as_str() == Some("error") && !why.is_empty() {
        return State::Failing { why };
    }
    State::Ready
}

/// `api-call`'s answer (`status_code`, `body`) into the account's windows.
fn account_usage(provider: Provider, answer: &Value) -> Result<PlanUsage> {
    let status = answer["status_code"].as_u64().unwrap_or(0);
    let body = answer["body"].as_str().unwrap_or_default();
    let json: Value =
        serde_json::from_str(body).context("the usage endpoint did not answer JSON")?;
    if let Some(msg) = json["error"]["message"]
        .as_str()
        .or_else(|| json["error"].as_str())
        .or_else(|| json["detail"].as_str())
    {
        bail!("HTTP {status}: {msg}");
    }
    if !(200..300).contains(&status) {
        bail!("HTTP {status}");
    }
    Ok(match provider {
        Provider::Claude => plan::parse_usage(&json, "", ""),
        Provider::Codex => codex_usage(&json, now()),
    })
}

/// ChatGPT's `wham/usage` answer into windows: the plan's own limits, then
/// any model's separate ones ("5-hour · GPT-5.3-Codex-Spark").
fn codex_usage(json: &Value, now: u64) -> PlanUsage {
    let mut windows = Vec::new();
    codex_windows(&json["rate_limit"], "", now, &mut windows);
    for extra in json["additional_rate_limits"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let name = extra["limit_name"].as_str().unwrap_or_default();
        codex_windows(&extra["rate_limit"], name, now, &mut windows);
    }
    PlanUsage {
        at: now,
        subscription: json["plan_type"].as_str().unwrap_or_default().to_string(),
        windows,
        ..PlanUsage::default()
    }
}

fn codex_windows(limit: &Value, name: &str, now: u64, out: &mut Vec<Window>) {
    for key in ["primary_window", "secondary_window"] {
        let w = &limit[key];
        if !w.is_object() {
            continue;
        }
        let length = window_name(w["limit_window_seconds"].as_u64().unwrap_or(0));
        let label = if name.is_empty() {
            length
        } else {
            format!("{} · {name}", length.trim_end_matches(" limit"))
        };
        let percent = plan::whole(&w["used_percent"]);
        let resets_at = w["reset_at"]
            .as_u64()
            .filter(|t| *t > 0)
            .or_else(|| w["reset_after_seconds"].as_u64().map(|s| now + s));
        out.push(Window {
            label,
            percent,
            severity: if percent >= 100 { "exceeded" } else { "normal" }.into(),
            resets_at,
            active: true,
        });
    }
}

/// "5-hour limit", "Weekly limit": a window by its length, named the way
/// Claude's are so the same rows and checks apply.
fn window_name(secs: u64) -> String {
    const HOUR: u64 = 3600;
    match secs {
        18_000 => plan::FIVE_HOUR.into(),
        604_800 => "Weekly limit".into(),
        86_400 => "Daily limit".into(),
        s if s >= 2_419_200 => "Monthly limit".into(),
        s if s > 0 && s % 86_400 == 0 => format!("{}-day limit", s / 86_400),
        s if s > 0 && s % HOUR == 0 => format!("{}-hour limit", s / HOUR),
        _ => "Limit".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_management_api_shares_the_proxys_origin() {
        assert_eq!(
            management_base("http://127.0.0.1:8317/v1").as_deref(),
            Some("http://127.0.0.1:8317/v0/management")
        );
        assert_eq!(
            management_base("HTTPS://user:pw@proxy.example.com/anthropic?x=1").as_deref(),
            Some("https://proxy.example.com/v0/management")
        );
        assert_eq!(management_base("localhost:8317"), None);
    }

    fn names(files: &Value) -> Vec<(Provider, String, String)> {
        listed_accounts(files)
            .into_iter()
            .map(|l| (l.account.provider, l.index, l.account.name))
            .collect()
    }

    #[test]
    fn only_enabled_claude_and_codex_sign_ins_are_read() {
        let files = json!({"files": [
            {"auth_index": "b", "provider": "claude", "email": "zoe@example.com"},
            {"auth_index": "a", "provider": "claude", "email": "amy@example.com"},
            {"auth_index": "c", "provider": "claude", "email": "off@example.com", "disabled": true},
            {"auth_index": "d", "provider": "claude", "account_type": "api_key", "label": "key"},
            {"auth_index": "e", "provider": "codex", "email": "gpt@example.com",
             "id_token": {"chatgpt_account_id": "ws-1", "plan_type": "pro"}},
            {"auth_index": "g", "provider": "gemini", "email": "gem@example.com"},
            {"auth_index": "f", "type": "claude", "name": "claude-team.json"},
        ]});
        let c = |i: &str, n: &str| (Provider::Claude, i.to_string(), n.to_string());
        assert_eq!(
            names(&files),
            [
                c("a", "amy@example.com"),
                c("f", "claude-team.json"),
                c("b", "zoe@example.com"),
                (Provider::Codex, "e".into(), "gpt@example.com".into()),
            ]
        );
        let codex = listed_accounts(&files).pop().unwrap();
        assert_eq!(codex.chatgpt.as_deref(), Some("ws-1"));
        assert_eq!(codex.account.plan, "pro");
        assert!(listed_accounts(&json!({"error": "x"})).is_empty());
    }

    #[test]
    fn auth_files_health_becomes_a_state() {
        let now = 1_000;
        assert_eq!(state(&json!({"status": "active"}), now), State::Ready);
        assert_eq!(
            state(&json!({"requires_sign_in": true, "unavailable": true}), now),
            State::SignIn
        );
        assert_eq!(
            state(
                &json!({"unavailable": true, "status_message": "quota exhausted",
                        "next_retry_after": "1970-01-01T00:20:00.5Z"}),
                now
            ),
            State::Cooling {
                until: Some(1_200),
                why: "quota exhausted".into()
            }
        );
        // A retry time already past says nothing about when it is back.
        assert_eq!(
            state(
                &json!({"quota_blocked": true, "next_retry_after": "1970-01-01T00:10:00Z"}),
                now
            ),
            State::Cooling {
                until: None,
                why: String::new()
            }
        );
        assert_eq!(
            state(
                &json!({"status": "error", "status_message": "refresh failed"}),
                now
            ),
            State::Failing {
                why: "refresh failed".into()
            }
        );
    }

    #[test]
    fn a_codex_usage_answer_becomes_windows() {
        let body = json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true, "limit_reached": false,
                "primary_window": {"used_percent": 12, "limit_window_seconds": 18000,
                                   "reset_after_seconds": 600, "reset_at": 5_000},
                "secondary_window": {"used_percent": 100, "limit_window_seconds": 604800,
                                     "reset_after_seconds": 9_000, "reset_at": 0}
            },
            "additional_rate_limits": [{
                "limit_name": "GPT-5.3-Codex-Spark", "metered_feature": "x",
                "rate_limit": {"allowed": true, "limit_reached": false,
                    "primary_window": {"used_percent": 3, "limit_window_seconds": 18000,
                                       "reset_after_seconds": 60, "reset_at": 1_060}}
            }]
        });
        let u = codex_usage(&body, 1_000);
        assert_eq!(u.subscription, "plus");
        assert_eq!(
            u.windows
                .iter()
                .map(|w| (w.label.as_str(), w.percent, w.resets_at))
                .collect::<Vec<_>>(),
            [
                ("5-hour limit", 12, Some(5_000)),
                ("Weekly limit", 100, Some(10_000)),
                ("5-hour · GPT-5.3-Codex-Spark", 3, Some(1_060)),
            ]
        );
        assert_eq!(u.five_hour(), Some(12));
        assert_eq!(u.weekly().map(|w| w.severity.as_str()), Some("exceeded"));
        let answer = json!({"status_code": 200, "body": body.to_string()});
        assert!(account_usage(Provider::Codex, &answer).is_ok());
        let denied = json!({"status_code": 401, "body": r#"{"detail":"Unauthorized"}"#});
        assert_eq!(
            account_usage(Provider::Codex, &denied)
                .unwrap_err()
                .to_string(),
            "HTTP 401: Unauthorized"
        );
    }

    #[test]
    fn windows_are_named_by_their_length() {
        assert_eq!(window_name(18_000), "5-hour limit");
        assert_eq!(window_name(604_800), "Weekly limit");
        assert_eq!(window_name(86_400), "Daily limit");
        assert_eq!(window_name(3 * 3600), "3-hour limit");
        assert_eq!(window_name(2 * 86_400), "2-day limit");
        assert_eq!(window_name(0), "Limit");
    }

    #[test]
    fn the_usage_call_reads_each_providers_endpoint() {
        let claude: Value = serde_json::from_str(&usage_call(Provider::Claude, "a", None)).unwrap();
        assert_eq!(claude["url"], plan::ENDPOINT);
        assert_eq!(claude["header"]["Authorization"], "Bearer $TOKEN$");
        let codex: Value =
            serde_json::from_str(&usage_call(Provider::Codex, "e", Some("ws-1"))).unwrap();
        assert_eq!(codex["url"], CODEX_USAGE);
        assert_eq!(codex["auth_index"], "e");
        assert_eq!(codex["header"]["ChatGPT-Account-Id"], "ws-1");
    }

    #[test]
    fn a_signed_out_account_costs_no_call() {
        let mut calls = Vec::new();
        let got = read_with(|path, _| {
            calls.push(path.to_string());
            Ok(json!({"files": [
                {"auth_index": "a", "provider": "claude", "email": "a@example.com",
                 "requires_sign_in": true},
            ]}))
        })
        .ok()
        .unwrap();
        assert_eq!(calls, ["/auth-files"]);
        assert_eq!(got[0].state, State::SignIn);
    }

    fn window(label: &str, percent: u64, resets_at: Option<u64>) -> Window {
        Window {
            label: label.into(),
            percent,
            severity: "normal".into(),
            resets_at,
            active: true,
        }
    }

    fn account(provider: Provider, state: State, windows: Option<Vec<Window>>) -> Account {
        Account {
            name: "a@example.com".into(),
            provider,
            plan: String::new(),
            state,
            usage: windows.map(|windows| PlanUsage {
                windows,
                ..PlanUsage::default()
            }),
            error: None,
        }
    }

    #[test]
    fn room_and_when_an_account_is_back() {
        let now = 1_000;
        let ok = account(
            Provider::Claude,
            State::Ready,
            Some(vec![window("5-hour limit", 40, Some(2_000))]),
        );
        assert!(ok.has_room(now) && ok.back_at(now).is_none());
        // Full 5-hour and weekly windows: back when both have started over.
        let full = account(
            Provider::Claude,
            State::Ready,
            Some(vec![
                window("5-hour limit", 100, Some(2_000)),
                window("Weekly limit", 100, Some(9_000)),
            ]),
        );
        assert!(!full.has_room(now));
        assert_eq!(full.back_at(now), Some(9_000));
        // A window that has started over since it was read is empty again.
        assert!(full.has_room(9_000));
        let cooling = account(
            Provider::Claude,
            State::Cooling {
                until: Some(1_500),
                why: String::new(),
            },
            Some(vec![window("5-hour limit", 10, Some(2_000))]),
        );
        assert_eq!(cooling.back_at(now), Some(1_500));
        assert!(cooling.has_room(1_500));
        // A model's own full window leaves room for the other models.
        let opus = account(
            Provider::Claude,
            State::Ready,
            Some(vec![
                window("5-hour limit", 20, Some(2_000)),
                window("Weekly · Opus", 100, Some(9_000)),
            ]),
        );
        assert!(opus.has_room(now) && opus.back_at(now).is_none());
        // Held back with no end given: when it is back is unknown, whatever
        // its windows say.
        let held = account(
            Provider::Claude,
            State::Cooling {
                until: None,
                why: String::new(),
            },
            Some(vec![window("5-hour limit", 100, Some(2_000))]),
        );
        assert!(!held.has_room(2_000) && held.back_at(now).is_none());
        let signed_out = account(Provider::Claude, State::SignIn, None);
        assert!(!signed_out.has_room(now) && signed_out.back_at(now).is_none());
        // Windows that were not read: not known to have room.
        assert!(!account(Provider::Claude, State::Ready, None).has_room(now));
    }

    #[test]
    fn the_pool_summary_counts_room_per_provider() {
        let now = 1_000;
        let room = || {
            account(
                Provider::Claude,
                State::Ready,
                Some(vec![window("5-hour limit", 20, Some(5_000))]),
            )
        };
        let out = account(
            Provider::Claude,
            State::Ready,
            Some(vec![window(
                "5-hour limit",
                100,
                Some(1_000 + 2 * 3600 + 12 * 60),
            )]),
        );
        let codex = account(Provider::Codex, State::SignIn, None);
        let all = [room(), room(), out, codex];
        assert_eq!(
            summary(&all, Provider::Claude, now).as_deref(),
            Some("2 of 3 Claude accounts have room · one is back in 2h 12m")
        );
        // One account needs no summary.
        assert_eq!(summary(&all, Provider::Codex, now), None);
        assert_eq!(
            summary(&[room(), room()], Provider::Claude, now).as_deref(),
            Some("All 2 Claude accounts have room")
        );
    }

    #[test]
    fn a_typed_address_becomes_a_management_base() {
        assert_eq!(
            address_base("127.0.0.1:8317").unwrap(),
            "http://127.0.0.1:8317/v0/management"
        );
        assert_eq!(
            address_base(" proxy.example.com/v1 ").unwrap(),
            "https://proxy.example.com/v0/management"
        );
        assert_eq!(
            address_base("http://proxy.example.com:8317").unwrap(),
            "http://proxy.example.com:8317/v0/management"
        );
        assert!(address_base("").is_err());
        assert!(address_base("ftp://proxy.example.com").is_err());
    }

    #[test]
    fn the_proxys_refusals_are_worded_for_the_tab() {
        let msg = |status, error: &str| match failure(LOCAL, status, error, "") {
            Some(Failure::Refused(m)) => format!("refused: {m}"),
            Some(Failure::Other(e)) => e.to_string(),
            None => "ok".into(),
        };
        assert_eq!(msg(200, ""), "ok");
        assert!(msg(401, "invalid management key").starts_with("refused: "));
        assert_eq!(
            msg(
                403,
                "IP banned due to too many failed attempts. Try again in 29m12s"
            ),
            "CLIProxyAPI blocked this address after too many wrong keys. Try again in 29m12s"
        );
        assert!(msg(403, "remote management disabled").contains("allow-remote"));
        assert!(msg(403, "remote management key not set").contains("secret-key"));
    }

    #[test]
    fn an_api_call_answer_becomes_windows() {
        let body = json!({"limits": [
            {"kind": "session", "percent": 37, "severity": "normal", "is_active": true},
            {"kind": "weekly", "percent": 91, "severity": "warning", "is_active": true}
        ]})
        .to_string();
        let u =
            account_usage(Provider::Claude, &json!({"status_code": 200, "body": body})).unwrap();
        assert_eq!(
            u.windows
                .iter()
                .map(|w| (w.label.as_str(), w.percent))
                .collect::<Vec<_>>(),
            [("5-hour limit", 37), ("Weekly limit", 91)]
        );
        let expired = json!({"status_code": 401, "body": r#"{"type":"error","error":{"type":"authentication_error","message":"OAuth token has expired."}}"#});
        let err = account_usage(Provider::Claude, &expired)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "HTTP 401: OAuth token has expired.");
    }

    const LOCAL: &str = "http://127.0.0.1:8317/v0/management";

    fn never(_: &str, _: &str) -> Result<Vec<Account>, Failure> {
        panic!("nothing may reach the proxy here")
    }

    fn refused(_: &str, _: &str) -> Result<Vec<Account>, Failure> {
        Err(Failure::Refused(
            "CLIProxyAPI refused the management key".into(),
        ))
    }

    fn one_account(_: &str, _: &str) -> Result<Vec<Account>, Failure> {
        Ok(vec![account(Provider::Claude, State::Ready, None)])
    }

    #[test]
    fn nothing_is_sent_without_a_key() {
        let (p, read) = decide(LOCAL.into(), None, None, 1_000, true, never);
        assert!(!read && !p.has_key && p.error.is_none());
    }

    #[test]
    fn a_refused_key_is_sent_once() {
        let (p, read) = decide(LOCAL.into(), Some("bad"), None, 1_000, false, refused);
        assert!(read && p.refused.is_some() && p.error.is_some());
        // Not again, even when forced or long after.
        let (p, read) = decide(LOCAL.into(), Some("bad"), Some(p), 99_000, true, never);
        assert!(!read && p.error.is_some());
        // A new key is tried.
        let (p, read) = decide(
            LOCAL.into(),
            Some("good"),
            Some(p),
            99_000,
            false,
            one_account,
        );
        assert!(read && p.refused.is_none() && p.error.is_none());
        assert_eq!(p.accounts.len(), 1);
    }

    #[test]
    fn a_fresh_read_is_served_from_the_cache() {
        let (p, _) = decide(LOCAL.into(), Some("k"), None, 1_000, false, one_account);
        let (p, read) = decide(
            LOCAL.into(),
            Some("k"),
            Some(p),
            1_000 + STALE - 1,
            false,
            never,
        );
        assert!(!read);
        let (_, read) = decide(LOCAL.into(), Some("k"), Some(p), 1_001, true, one_account);
        assert!(read, "a saved key reads at once");
    }

    /// A keyring in memory. `locked` makes it refuse, as one left locked does.
    #[derive(Default)]
    struct Fake {
        key: std::cell::RefCell<Option<String>>,
        locked: bool,
        asked: std::cell::Cell<usize>,
    }

    impl Keep for Fake {
        fn get(&self) -> Result<Option<String>, String> {
            self.asked.set(self.asked.get() + 1);
            if self.locked {
                return Err("locked".into());
            }
            Ok(self.key.borrow().clone())
        }

        fn set(&self, key: &str) -> Result<(), String> {
            self.asked.set(self.asked.get() + 1);
            if self.locked {
                return Err("locked".into());
            }
            *self.key.borrow_mut() = Some(key.to_string());
            Ok(())
        }

        fn forget(&self) {
            *self.key.borrow_mut() = None;
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("arakun-cliproxy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("cliproxy-management-key")
    }

    fn saved(base: &str, key: &str) -> Option<SavedKey> {
        Some(SavedKey {
            base: base.into(),
            key: key.into(),
        })
    }

    #[test]
    fn the_key_only_goes_to_the_proxy_it_was_typed_for() {
        let (file, ring) = (scratch("typed-for"), Fake::default());
        *ring.key.borrow_mut() = Some("k".into());
        let other = "https://gw.example.com/v0/management";
        assert_eq!(
            key_for(saved(LOCAL, ""), LOCAL, &file, &mut None, false, &ring),
            Ok(Some("k".into()))
        );
        assert_eq!(
            key_for(saved(LOCAL, ""), other, &file, &mut None, false, &ring),
            Ok(None)
        );
        assert_eq!(
            key_for(None, LOCAL, &file, &mut None, false, &ring),
            Ok(None)
        );
        assert_eq!(ring.asked.get(), 1, "only the proxy it was typed for asks");
    }

    #[test]
    fn a_key_saved_goes_to_the_keyring_and_the_file_keeps_only_the_proxy() {
        let (file, ring) = (scratch("save"), Fake::default());
        let mut held = None;
        save_key_in(" k1 ", LOCAL, &file, &mut held, &ring).unwrap();
        assert_eq!(ring.key.borrow().as_deref(), Some("k1"));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("k1"), "{text}");
        let left: SavedKey = crate::durable::load_json(&file).unwrap();
        assert_eq!((left.base.as_str(), left.key.as_str()), (LOCAL, ""));
        // The poll reads what was saved without asking the keyring.
        let asked = ring.asked.get();
        assert_eq!(
            key_for(Some(left), LOCAL, &file, &mut held, false, &ring),
            Ok(Some("k1".into()))
        );
        assert_eq!(ring.asked.get(), asked);
    }

    #[test]
    fn a_key_from_before_moves_to_the_keyring_once() {
        let (file, ring) = (scratch("move"), Fake::default());
        let mut held = None;
        // A key typed for another proxy moves too.
        let got = key_for(
            saved("https://gw.example.com/v0/management", "old"),
            LOCAL,
            &file,
            &mut held,
            false,
            &ring,
        );
        assert_eq!(got, Ok(None));
        assert_eq!(ring.key.borrow().as_deref(), Some("old"));
        let left: SavedKey = crate::durable::load_json(&file).unwrap();
        assert!(left.key.is_empty());
        assert_eq!(left.base, "https://gw.example.com/v0/management");
        let mut held = None;
        assert_eq!(
            key_for(saved(LOCAL, "k"), LOCAL, &file, &mut held, false, &ring),
            Ok(Some("k".into()))
        );
        assert_eq!(ring.key.borrow().as_deref(), Some("k"));
    }

    #[test]
    fn a_locked_keyring_keeps_the_files_key_and_is_not_asked_each_poll() {
        let (file, ring) = (
            scratch("locked"),
            Fake {
                locked: true,
                ..Fake::default()
            },
        );
        crate::durable::write_private(&file, br#"{"base":"x","key":"k"}"#).unwrap();
        let mut held = None;
        for _ in 0..3 {
            assert_eq!(
                key_for(saved(LOCAL, "k"), LOCAL, &file, &mut held, false, &ring),
                Ok(Some("k".into()))
            );
        }
        assert_eq!(ring.asked.get(), 1);
        let left: SavedKey = crate::durable::load_json(&file).unwrap();
        assert_eq!(
            left.key, "k",
            "the file keeps it until the keyring takes it"
        );
        // Refresh asks again.
        key_for(saved(LOCAL, "k"), LOCAL, &file, &mut held, true, &ring).unwrap();
        assert_eq!(ring.asked.get(), 2);
        // A key only in the keyring: the error shows, not a first run.
        let mut held = None;
        assert!(key_for(saved(LOCAL, ""), LOCAL, &file, &mut held, false, &ring).is_err());
        assert!(key_for(saved(LOCAL, ""), LOCAL, &file, &mut held, false, &ring).is_err());
        assert_eq!(ring.asked.get(), 3);
        assert!(save_key_in("k2", LOCAL, &file, &mut held, &ring).is_err());
    }

    #[test]
    fn a_keyring_that_cannot_say_keeps_the_tab_on_the_key() {
        let p = keyring_failed(LOCAL.into(), None, "locked");
        assert!(p.has_key && !p.key_refused());
        assert!(p.error.unwrap().contains("keyring"));
    }

    #[test]
    fn a_forgotten_key_leaves_the_keyring() {
        let (file, ring) = (scratch("forget"), Fake::default());
        let mut held = None;
        save_key_in("k", LOCAL, &file, &mut held, &ring).unwrap();
        remove_key(&file, &file.with_extension("cache"), &mut held, &ring).unwrap();
        assert!(!file.exists() && ring.key.borrow().is_none() && held.is_none());
    }

    #[test]
    fn the_key_never_travels_in_the_clear() {
        for base in [
            LOCAL,
            "http://localhost:8317/v0/management",
            "http://[::1]:8317/v0/management",
            "https://proxy.example.com/v0/management",
        ] {
            assert!(safe_to_send(base), "{base}");
        }
        for base in [
            "http://proxy.example.com:8317/v0/management",
            "http://192.168.1.5:8317/v0/management",
            "http://127.0.0.1.example.com/v0/management",
        ] {
            assert!(!safe_to_send(base), "{base}");
            let (p, read) = decide(base.into(), Some("k"), None, 1_000, true, never);
            assert!(!read && p.error.is_some(), "{base}");
        }
    }

    #[test]
    fn a_key_refused_midway_is_not_sent_to_the_other_accounts() {
        let mut calls = Vec::new();
        let got = read_with(|path, _| {
            calls.push(path.to_string());
            match path {
                "/auth-files" => Ok(json!({"files": [
                    {"auth_index": "a", "provider": "claude", "email": "a@example.com"},
                    {"auth_index": "b", "provider": "claude", "email": "b@example.com"},
                ]})),
                _ => Err(Failure::Refused("refused".into())),
            }
        });
        assert!(matches!(got, Err(Failure::Refused(_))));
        assert_eq!(calls, ["/auth-files", "/api-call"]);
    }

    #[test]
    fn only_this_machine_skips_the_environments_proxy() {
        assert!(loopback(LOCAL));
        assert!(loopback("http://localhost:8317/v0/management"));
        assert!(loopback("http://[::1]:8317/v0/management"));
        assert!(!loopback("https://proxy.example.com/v0/management"));
        assert!(!loopback("http://127.0.0.1.example.com/v0/management"));
    }

    #[test]
    fn long_error_bodies_are_cut_on_a_character() {
        let body = "é".repeat(150);
        assert_eq!(clip(&body, 200).chars().count(), 150);
        assert_eq!(clip(&"€".repeat(300), 200).chars().count(), 200);
    }

    #[test]
    fn the_cache_keeps_a_refused_keys_fingerprint_not_the_key() {
        let p = Proxy {
            base: "http://127.0.0.1:8317/v0/management".into(),
            refused: Some(fingerprint("secret")),
            has_key: true,
            ..Proxy::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("secret"));
        let back: Proxy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.refused, Some(fingerprint("secret")));
        assert_ne!(fingerprint("secret"), fingerprint("secret2"));
    }
}
