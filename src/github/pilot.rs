//! arakun-pilot: the user's pull request agent on Cloudflare (~/arakun/pilot-cf).
//! It reviews other people's pull requests and fixes the user's own, then
//! leaves items (Spanish summaries) at `GET /items`. Arakun reads them every
//! minute (every five while its window is behind), shows them at the top of
//! the GitHub tab and tells the Worker
//! (`POST /items/<id>/done`) when the user dealt with one.
//!
//! Off unless `client.json` (`{"url": "..."}`) exists. Its read token is in
//! the keyring (`secret_store`). With an admin token there too, the palette
//! can hand the agent new work (`POST /tasks`). A file written by hand, or
//! from before the keyring, may hold `token` and `admin_token`: they move to
//! the keyring the first time the file is read.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, EventEmitter, Task};
use serde::{Deserialize, Deserializer, Serialize};

use super::pr::{ItemKind, ItemRef};

const POLL: Duration = Duration::from_secs(60);
/// The read's gap while the window is in the background, as the GitHub
/// inbox's: notices still come, only less often.
const POLL_IDLE: Duration = Duration::from_secs(300);
/// The only repository the Worker works on (SPEC.md): a task's `pr` is a
/// number in it.
pub const REPO: &str = "ArakunHQ/arakun";
/// The most ready answers a card or the board shows; the Worker sends 2 to 4.
pub(super) const OPCIONES_MAX: usize = 4;
/// Ids kept in the book: far more than the Worker ever has open.
const BOOK_MAX: usize = 1000;

// ---------------------------------------------------------------------------
// Where things are

/// The client's settings: `$XDG_CONFIG_HOME/arakun-pilot/client.json`
/// (`~/.config/…`). A test Arakun (`ARAKUN_STATE`, as `arakun dev` sets it)
/// reads `arakun-pilot/client.json` next to its state instead, so it never
/// talks to the real Worker unless pointed at it.
pub fn config_file() -> PathBuf {
    config_file_from(
        std::env::var_os("ARAKUN_STATE").map(PathBuf::from),
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        crate::platform::home(),
    )
}

fn config_file_from(state: Option<PathBuf>, xdg: Option<PathBuf>, home: PathBuf) -> PathBuf {
    let root = match state {
        Some(s) => s.parent().map(PathBuf::from).unwrap_or_default(),
        None => xdg
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config")),
    };
    root.join("arakun-pilot").join("client.json")
}

/// What was seen and handled, next to the app's other state.
fn book_file() -> PathBuf {
    match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name("pilot.json"),
        None => crate::state::data_dir().join("pilot.json"),
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Config {
    pub url: String,
    pub token: String,
    /// Only `POST /tasks` takes it: reading items never needs it.
    #[serde(default)]
    pub admin_token: Option<String>,
}

// The tokens never reach a log, not even through `{:?}`.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("url", &self.url)
            .field("token", &"<hidden>")
            .field(
                "admin_token",
                &self.admin_token.as_ref().map(|_| "<hidden>"),
            )
            .finish()
    }
}

/// The settings in `bytes`; None when they are missing a part.
pub fn parse_config(bytes: &[u8]) -> Option<Config> {
    let c: Config = serde_json::from_slice(bytes).ok()?;
    let url = c.url.trim().trim_end_matches('/').to_string();
    let token = c.token.trim().to_string();
    let admin_token = c
        .admin_token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let web = url.starts_with("https://") || local_http(&url);
    (web && !token.is_empty()).then_some(Config {
        url,
        token,
        admin_token,
    })
}

/// Plain `http://` only to this machine: the tokens must never cross a
/// network unencrypted, and `admin_token` can hand the agent work.
pub(crate) use crate::cloud::local_http;

pub fn load_config() -> Option<Config> {
    read_config().ok().flatten()
}

/// The settings: Ok(None) when there is no file (pilot is off), Err when
/// there is one that can't be used, so a typo in it says so instead of
/// looking like pilot was never set up. The tokens come from the keyring,
/// so never call this on the UI thread: the keyring may wait for the person
/// to unlock it.
pub fn read_config() -> Result<Option<Config>, String> {
    read_config_at(&config_file(), &Keyring, &Accounts::here())
}

const FIX: &str = "connect again with \u{201c}arakun-pilot settings\u{2026}\u{201d}";

/// What `client.json` holds. The tokens are there only in a file from
/// before the keyring, or one written by hand, until they are moved.
#[derive(Deserialize)]
struct Stored {
    #[serde(default)]
    url: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    admin_token: Option<String>,
}

fn read_config_at(
    path: &Path,
    vault: &dyn Vault,
    accounts: &Accounts,
) -> Result<Option<Config>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("couldn't read {}: {e}", path.display())),
    };
    let Ok(stored) = serde_json::from_slice::<Stored>(&bytes) else {
        return Err(config_problem(&bytes));
    };
    let url = stored.url.trim().trim_end_matches('/').to_string();
    if !(url.starts_with("https://") || local_http(&url)) {
        return Err(config_problem(&bytes));
    }
    let in_file = |t: Option<String>| t.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let (file_token, file_admin) = (in_file(stored.token), in_file(stored.admin_token));
    if file_token.is_some() || file_admin.is_some() {
        move_to_keyring(path, vault, accounts, &file_token, &file_admin);
    }
    let from_ring = |account: &str, what: &str| {
        vault
            .get(account)
            .map(|t| t.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()))
            .map_err(|e| format!("the keyring could not give its {what} ({e})"))
    };
    let token = match file_token {
        Some(t) => t,
        None => from_ring(&accounts.token, "token")?
            .ok_or_else(|| format!("its settings have no token; {FIX}"))?,
    };
    // A keyring that fails on the admin token fails the read, so a form
    // saved after it doesn't drop a token it could not see.
    let admin_token = match file_admin {
        Some(t) => Some(t),
        None => from_ring(&accounts.admin, "admin token")?,
    };
    Ok(Some(Config {
        url,
        token,
        admin_token,
    }))
}

/// Moves the tokens a file still holds to the keyring, then takes them out
/// of the file. A token the keyring refuses stays in the file and is still
/// used.
fn move_to_keyring(
    path: &Path,
    vault: &dyn Vault,
    accounts: &Accounts,
    token: &Option<String>,
    admin: &Option<String>,
) {
    let _held = CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut moved = Vec::new();
    for (key, secret, account, label) in [
        ("token", token, &accounts.token, TOKEN_LABEL),
        ("admin_token", admin, &accounts.admin, ADMIN_LABEL),
    ] {
        let Some(secret) = secret else { continue };
        match vault.set(account, label, secret) {
            Ok(()) => moved.push((key, secret)),
            Err(e) => eprintln!("arakun: could not move arakun-pilot's {key} to the keyring: {e}"),
        }
    }
    if moved.is_empty() {
        return;
    }
    // Read again under the lock: a save may have written new settings since.
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(serde_json::Value::Object(mut file)) = serde_json::from_slice(&bytes) else {
        return;
    };
    let mut changed = false;
    for (key, secret) in moved {
        // Only the value that was moved: a newer one stays, to move next time.
        if file.get(key).and_then(|v| v.as_str()).map(str::trim) == Some(secret.as_str()) {
            file.remove(key);
            changed = true;
        }
    }
    if changed && let Err(e) = write_settings(path, &serde_json::Value::Object(file)) {
        eprintln!("arakun: {e}");
    }
}

/// Writes `json` to the settings file, readable only by this user.
pub(super) fn write_settings(path: &Path, json: &serde_json::Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(json).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    crate::durable::write_private(path, &bytes)
}

/// One reader or writer of the settings file and its tokens at a time.
pub(super) static CONFIG_LOCK: Mutex<()> = Mutex::new(());

pub(super) const TOKEN_LABEL: &str = "arakun: arakun-pilot token";
pub(super) const ADMIN_LABEL: &str = "arakun: arakun-pilot admin token";

/// The keyring entries of the two tokens.
pub(super) struct Accounts {
    pub token: String,
    pub admin: String,
}

impl Accounts {
    pub fn here() -> Self {
        Self::for_state(std::env::var_os("ARAKUN_STATE").as_deref())
    }

    /// The tokens go with `client.json`. A test Arakun keeps that file in
    /// its state's folder, so its tokens are named by that folder: two
    /// state files in one folder share the file, and so the tokens. Without
    /// `ARAKUN_STATE` the file is the shared one, and so are the names.
    fn for_state(state: Option<&std::ffi::OsStr>) -> Self {
        let folder = state.map(|s| {
            let state = crate::secret_store::profile(&s.to_string_lossy());
            Path::new(&state)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(state)
        });
        let name = |n: &str| crate::secret_store::account_for(n, folder.as_deref());
        Self {
            token: name("arakun-pilot-token"),
            admin: name("arakun-pilot-admin-token"),
        }
    }
}

/// Where the tokens are kept: the keyring, or a fake one in the tests.
pub(super) trait Vault {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, label: &str, secret: &str) -> Result<(), String>;
    fn forget(&self, account: &str);
}

pub(super) struct Keyring;

impl Vault for Keyring {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        crate::secret_store::get(account)
    }

    fn set(&self, account: &str, label: &str, secret: &str) -> Result<(), String> {
        crate::secret_store::set(account, label, secret)
    }

    fn forget(&self, account: &str) {
        crate::secret_store::forget(account)
    }
}

/// Why settings that don't parse can't be used, in the user's words. It
/// names the part that is wrong, never its value: that may be a token. A
/// missing token is not one: it is in the keyring.
fn config_problem(bytes: &[u8]) -> String {
    let fix = FIX;
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return format!("its settings file isn't valid JSON; {fix}");
    };
    let text = |key: &str| v.get(key).and_then(|t| t.as_str()).map(str::trim);
    let url = text("url").unwrap_or_default();
    let admin_ok = matches!(
        v.get("admin_token"),
        None | Some(serde_json::Value::Null | serde_json::Value::String(_))
    );
    let url = url.trim_end_matches('/');
    if url.is_empty() {
        format!("its settings have no address; {fix}")
    } else if !admin_ok {
        format!("its admin token isn't text; {fix}")
    } else if !(url.starts_with("https://") || local_http(url)) {
        format!("its address must start with https:// (http:// only for localhost); {fix}")
    } else {
        format!("its settings can't be read; {fix}")
    }
}

// ---------------------------------------------------------------------------
// Items

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Someone else's pull request, read and summed up for the user's click.
    Brief,
    /// The agent pushed fixes to the user's pull request.
    Fixed,
    /// It stopped on something only the user can decide.
    NeedsYou,
    /// The job broke.
    Failed,
    /// A kind from a newer Worker.
    #[default]
    #[serde(other)]
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Brief => "To review",
            Kind::Fixed => "Fixed",
            Kind::NeedsYou => "Needs you",
            Kind::Failed => "Failed",
            Kind::Other => "Notice",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Riesgo {
    Bajo,
    Medio,
    Alto,
    #[serde(other)]
    Otro,
}

impl Riesgo {
    pub fn label(self) -> &'static str {
        match self {
            Riesgo::Bajo => "Low risk",
            Riesgo::Medio => "Medium risk",
            Riesgo::Alto => "High risk",
            Riesgo::Otro => "Risk ?",
        }
    }
}

/// A finding at a place in the code.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Punto {
    #[serde(default, deserialize_with = "or_default")]
    pub archivo: String,
    #[serde(default, deserialize_with = "or_default")]
    pub linea: Option<u64>,
    #[serde(default, deserialize_with = "or_default")]
    pub texto: String,
}

impl Punto {
    /// `src/x.rs:12`, or just the file.
    pub fn place(&self) -> String {
        match self.linea {
            Some(l) if !self.archivo.is_empty() => format!("{}:{l}", self.archivo),
            _ => self.archivo.clone(),
        }
    }
}

/// A ready answer to the agent's question: a button on the card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Opcion {
    #[serde(default, deserialize_with = "or_default")]
    pub id: String,
    #[serde(default, deserialize_with = "or_default")]
    pub texto: String,
}

impl Opcion {
    /// "Lo veo yo": the agent stops and leaves it to the user, so it is
    /// never the button that looks like the answer.
    pub fn quiet(&self) -> bool {
        self.id == "skip"
    }
}

/// One thing the agent wants the user to see. Every field but `id` may be
/// missing or null; fields a newer Worker adds are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Item {
    #[serde(default, deserialize_with = "or_default")]
    pub id: String,
    /// ISO time.
    #[serde(default, deserialize_with = "or_default")]
    pub at: String,
    #[serde(default, deserialize_with = "or_default")]
    pub kind: Kind,
    #[serde(default, deserialize_with = "or_default")]
    pub repo: String,
    #[serde(default, deserialize_with = "or_default")]
    pub number: u64,
    #[serde(default, deserialize_with = "or_default")]
    pub title: String,
    #[serde(default, deserialize_with = "or_default")]
    pub author: String,
    /// The commit the item is about.
    #[serde(default, deserialize_with = "or_default")]
    pub head: String,
    #[serde(default, deserialize_with = "or_default")]
    pub url: String,
    #[serde(default, deserialize_with = "or_default")]
    pub resumen: String,
    #[serde(default, deserialize_with = "or_default")]
    pub toca: Vec<String>,
    #[serde(default, deserialize_with = "or_default")]
    pub riesgo: Option<Riesgo>,
    #[serde(default, deserialize_with = "or_default")]
    pub importante: bool,
    #[serde(default, deserialize_with = "or_default")]
    pub por_que: String,
    #[serde(default, deserialize_with = "or_default")]
    pub recomendacion: String,
    #[serde(default, deserialize_with = "or_default")]
    pub puntos: Vec<Punto>,
    /// English review text to post as a change request.
    #[serde(default, deserialize_with = "or_default")]
    pub borrador: String,
    #[serde(default, deserialize_with = "or_default")]
    pub commits: Vec<String>,
    #[serde(default, deserialize_with = "or_default")]
    pub pregunta: String,
    /// For needs_you: ready answers to `pregunta`. Cleaned on read: none
    /// without an id or a text, at most `OPCIONES_MAX`.
    #[serde(default, deserialize_with = "or_default")]
    pub opciones: Vec<Opcion>,
}

/// A null reads as the field's default, like a missing one.
pub(super) fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// The pull request `repo#number` names, else the one at `url`. An address
/// from the Worker counts only on the host gh talks to.
pub(super) fn pull_ref(repo: &str, number: u64, url: &str) -> Option<ItemRef> {
    let from_repo = (number > 0)
        .then(|| ItemRef::parse(&format!("{repo}#{number}")))
        .flatten();
    from_repo
        .or_else(|| ItemRef::from_url_on(url, &super::pr::gh_host()))
        .map(|i| ItemRef {
            kind: ItemKind::Pull,
            ..i
        })
}

impl Item {
    /// The pull request it is about, when it names one in `REPO`. The
    /// Worker reads other people's pull requests, so it never picks which
    /// repository an approval lands on: anything else names nothing.
    pub fn item_ref(&self) -> Option<ItemRef> {
        pull_ref(&self.repo, self.number, &self.url).filter(|i| i.repo.eq_ignore_ascii_case(REPO))
    }

    /// Whether the tab's focus keeps it: a card about no repository always
    /// shows, as nothing could filter it.
    pub fn in_focus(&self, focus: &[String]) -> bool {
        let repo = self.repo.trim();
        repo.is_empty() || super::in_focus(repo, focus)
    }

    /// Whether it pops the island: the agent stopped for the user, or a
    /// pull request to review is important. The rest only waits in the tab.
    pub fn pops(&self) -> bool {
        match self.kind {
            Kind::NeedsYou => true,
            Kind::Brief => self.importante,
            _ => false,
        }
    }

    /// The island notice: "Your agent · #95" and the question, or the first
    /// sentence of the summary.
    pub fn notice(&self) -> (String, String) {
        let title = if self.number > 0 {
            format!("Your agent · #{}", self.number)
        } else {
            "Your agent".to_string()
        };
        let text = if self.kind == Kind::NeedsYou && !self.pregunta.trim().is_empty() {
            self.pregunta.trim().to_string()
        } else {
            first_sentence(&self.resumen)
        };
        let text = if text.is_empty() {
            self.title.clone()
        } else {
            text
        };
        (title, text)
    }
}

/// Up to the first `.`, `?` or `!` that ends a sentence (followed by a space
/// or the end), keeping it. "v1.2" and "src/x.rs" don't end one.
pub fn first_sentence(text: &str) -> String {
    let t = text.trim();
    let mut chars = t.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '.' | '?' | '!') && chars.peek().is_none_or(|(_, n)| n.is_whitespace()) {
            return t[..i + c.len_utf8()].to_string();
        }
    }
    t.to_string()
}

/// A full commit SHA: 40 hex characters. A card's review is pinned to one,
/// never to a short name GitHub might read as another commit.
pub fn full_sha(s: &str) -> bool {
    let s = s.trim();
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether two full SHAs name the same commit.
pub fn same_commit(a: &str, b: &str) -> bool {
    full_sha(a) && full_sha(b) && a.trim().eq_ignore_ascii_case(b.trim())
}

/// What `GET /items` answers: a list, or `{"items": [...]}`. Anything else
/// (an error object, a login page's JSON) is an error, so the cards on show
/// stay. Each item is read on its own: one with a field of the wrong type is
/// dropped, not the whole answer. Items without an id are dropped too:
/// nothing could mark them done.
pub fn parse_items(bytes: &[u8]) -> Result<Vec<Item>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("unexpected answer: {e}"))?;
    let list = match value {
        serde_json::Value::Array(list) => list,
        serde_json::Value::Object(mut o) => match o.remove("items") {
            Some(serde_json::Value::Array(list)) => list,
            _ => return Err("unexpected answer: no \"items\" list".into()),
        },
        _ => return Err("unexpected answer: not a list of items".into()),
    };
    Ok(list
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Item>(v).ok())
        .filter(|i| !i.id.is_empty())
        .map(|mut i| {
            i.opciones
                .retain(|o| !o.id.trim().is_empty() && !o.texto.trim().is_empty());
            i.opciones.truncate(OPCIONES_MAX);
            i
        })
        .collect())
}

// ---------------------------------------------------------------------------
// The book: what was seen and handled, across restarts

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    /// Items read at least once: they don't pop the island again.
    #[serde(default)]
    pub seen: Vec<String>,
    /// Items the user dealt with: their cards are gone.
    #[serde(default)]
    pub handled: Vec<String>,
    /// Handled items the Worker wasn't told about yet.
    #[serde(default)]
    pub untold: Vec<String>,
}

impl Book {
    pub fn handled(&self, id: &str) -> bool {
        self.handled.iter().any(|h| h == id)
    }

    /// Marks `id` handled; the Worker is told next.
    pub fn handle(&mut self, id: &str) {
        if !self.handled(id) {
            self.handled.push(id.to_string());
        }
        if !self.untold.iter().any(|u| u == id) {
            self.untold.push(id.to_string());
        }
        trim(&mut self.handled);
    }

    /// Hides `id` without telling the Worker it is done: an answer already
    /// moved it on there.
    pub fn hide(&mut self, id: &str) {
        if !self.handled(id) {
            self.handled.push(id.to_string());
        }
        trim(&mut self.handled);
    }

    pub fn told(&mut self, id: &str) {
        self.untold.retain(|u| u != id);
    }

    /// Takes in a read of every item the Worker has open: `items` becomes
    /// them, newest first, so what it closed on its own (the brief of an older
    /// head, the items of a merged PR) goes. Returns those that pop the island:
    /// never seen, not handled, asking for the user. `quiet` (the first read
    /// ever) only learns what is there.
    pub fn absorb(&mut self, items: &mut Vec<Item>, read: Vec<Item>, quiet: bool) -> Vec<Item> {
        let mut pops = Vec::new();
        for item in &read {
            if !self.seen.contains(&item.id) {
                self.seen.push(item.id.clone());
                if !quiet && !self.handled(&item.id) && item.pops() {
                    pops.push(item.clone());
                }
            }
        }
        trim(&mut self.seen);
        *items = read;
        items.sort_by(|a, b| b.at.cmp(&a.at));
        pops
    }
}

/// The oldest ids go first once a list is full.
fn trim(list: &mut Vec<String>) {
    if list.len() > BOOK_MAX {
        list.drain(..list.len() - BOOK_MAX);
    }
}

fn load_book() -> Option<Book> {
    crate::durable::load_json(book_file())
}

fn store_book(book: &Book) {
    let path = book_file();
    let bytes = serde_json::to_vec_pretty(book).unwrap_or_default();
    if let Err(e) = crate::durable::write_private(&path, &bytes) {
        eprintln!("arakun: could not save {}: {e}", path.display());
    }
}

// ---------------------------------------------------------------------------
// The Worker

/// An agent for requests to `base`. Plain `http://` to this machine skips
/// the proxy from the environment (`HTTP_PROXY`, `ALL_PROXY`): it would carry
/// the tokens off the machine unencrypted. `https://` keeps it.
fn agent(base: &str) -> ureq::Agent {
    let timeout = Duration::from_secs(20);
    if !local_http(base) {
        return crate::http::agent(timeout).clone();
    }
    crate::http::builder()
        .proxy(None)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// A query or path piece: ISO times carry `:` and `+`.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// What a status means. Never the token: errors only name the address.
pub(super) fn status_problem(status: u16) -> String {
    match status {
        401 | 403 => {
            "the Worker refused the token (change it in \u{201c}arakun-pilot settings\u{2026}\u{201d})"
                .into()
        }
        _ => format!("the Worker answered {status}"),
    }
}

/// A refused write, with the Worker's own reason (`{"error": "..."}`) when
/// it gave one: "la rama ya existe" helps more than a bare 409.
fn write_problem(status: u16, body: &[u8], admin: bool) -> String {
    if admin && matches!(status, 401 | 403) {
        return "the Worker refused the admin token (change it in \u{201c}arakun-pilot settings\u{2026}\u{201d})"
            .into();
    }
    let reason = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .map(|e| e.trim().chars().take(200).collect::<String>())
        .filter(|e| !e.is_empty());
    match reason {
        Some(r) => format!("{} ({r})", status_problem(status)),
        None => status_problem(status),
    }
}

/// Sends `body` as JSON to `path` with `token`; Ok with what came back on a 2xx.
fn post_json(
    base: &str,
    path: &str,
    token: &str,
    body: &serde_json::Value,
    admin: bool,
) -> Result<Vec<u8>, String> {
    let reply = crate::http::json_call(
        &agent(base),
        "POST",
        &format!("{base}{path}"),
        Some(token),
        Some(body.to_string()),
        64 * 1024,
    )
    .map_err(|e| format!("could not reach {base}: {e}"))?;
    if reply.ok() {
        Ok(reply.bytes)
    } else {
        Err(write_problem(reply.status, &reply.bytes, admin))
    }
}

/// A read of `path` with the read token: the status and up to `limit` bytes.
/// Never the token in an error: only the address.
pub(super) fn get(cfg: &Config, path: &str, limit: u64) -> Result<(u16, Vec<u8>), String> {
    let url = format!("{}{path}", cfg.url);
    let reply =
        crate::http::json_call(&agent(&cfg.url), "GET", &url, Some(&cfg.token), None, limit)
            .map_err(|e| format!("could not reach {}: {e}", cfg.url))?;
    Ok((reply.status, reply.bytes))
}

/// Every item the Worker has open. It closes some on its own, so a read of
/// only the newest would keep those on show.
pub fn fetch(cfg: &Config) -> Result<Vec<Item>, String> {
    match get(cfg, "/items", 8 * 1024 * 1024)? {
        (200, bytes) => parse_items(&bytes),
        (status, _) => Err(status_problem(status)),
    }
}

/// Tells the Worker the user dealt with `id`. One it no longer knows is done too.
pub fn tell_done(cfg: &Config, id: &str) -> Result<(), String> {
    let url = format!("{}/items/{}/done", cfg.url, encode(id));
    let reply = crate::http::json_call(
        &agent(&cfg.url),
        "POST",
        &url,
        Some(&cfg.token),
        None,
        64 * 1024,
    )
    .map_err(|e| format!("could not reach {}: {e}", cfg.url))?;
    match reply.status {
        200..=299 | 404 => Ok(()),
        s => Err(status_problem(s)),
    }
}

/// The most `done` calls in flight at once.
const TELL_AT_ONCE: usize = 8;

/// Tells the Worker every id in `ids`, a few at a time so one slow call
/// doesn't hold up the rest. Returns those it took.
fn tell_all(cfg: &Config, ids: Vec<String>) -> Vec<String> {
    let mut told = Vec::new();
    for batch in ids.chunks(TELL_AT_ONCE) {
        std::thread::scope(|s| {
            let calls: Vec<_> = batch
                .iter()
                .map(|id| s.spawn(move || tell_done(cfg, id).is_ok().then(|| id.clone())))
                .collect();
            told.extend(calls.into_iter().filter_map(|c| c.join().ok().flatten()));
        });
    }
    told
}

/// What the user answers a needs_you card with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// One of the item's `opciones`, by id.
    Opcion(String),
    /// "Other answer…": their own words.
    Texto(String),
}

/// The body of `POST /items/<id>/answer`.
pub fn answer_body(a: &Answer) -> serde_json::Value {
    match a {
        Answer::Opcion(id) => serde_json::json!({ "opcion": id }),
        Answer::Texto(t) => serde_json::json!({ "texto": t.trim() }),
    }
}

/// Hands the user's answer to the agent, which picks the job up again.
pub fn send_answer(cfg: &Config, id: &str, a: &Answer) -> Result<(), String> {
    let path = format!("/items/{}/answer", encode(id));
    post_json(&cfg.url, &path, &cfg.token, &answer_body(a), false).map(|_| ())
}

/// Where a task from the palette lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskTarget {
    /// One of the user's open pull requests in `REPO`, by number.
    Pull(u64),
    /// A new branch off `base`.
    Branch { name: String, base: String },
}

/// Work handed to the cloud agent from the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRequest {
    pub title: String,
    pub prompt: String,
    pub target: TaskTarget,
}

/// The body of `POST /tasks`.
pub fn task_body(t: &TaskRequest) -> serde_json::Value {
    match &t.target {
        TaskTarget::Pull(n) => serde_json::json!({
            "title": t.title, "prompt": t.prompt, "pr": n,
        }),
        TaskTarget::Branch { name, base } => serde_json::json!({
            "title": t.title, "prompt": t.prompt, "branch": name, "base": base,
        }),
    }
}

/// The task's id in what `POST /tasks` answered: `{"id": ...}`, or the same
/// inside `{"task": ...}`. A number reads as its digits.
pub fn parse_task_id(bytes: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let id = v
        .get("id")
        .or_else(|| v.get("task").and_then(|t| t.get("id")))?;
    let id = match id {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!id.is_empty()).then_some(id)
}

/// Why a task can't go with these settings: tasks take the admin token.
pub const NO_ADMIN: &str =
    "Sending tasks needs the admin token: add it in \u{201c}arakun-pilot settings\u{2026}\u{201d}.";

/// Hands the agent new work. Ok with the task's id ("" when the Worker
/// named none); its result comes back later as an item.
pub fn send_task(cfg: &Config, t: &TaskRequest) -> Result<String, String> {
    let admin = cfg.admin_token.as_deref().ok_or(NO_ADMIN)?;
    let bytes = post_json(&cfg.url, "/tasks", admin, &task_body(t), true)?;
    Ok(parse_task_id(&bytes).unwrap_or_default())
}

/// See `Pilot::status`.
fn status_line(
    config: Option<&Config>,
    config_problem: Option<&str>,
    problem: Option<&str>,
    board_failed: Option<&str>,
    last_read: Option<u64>,
    now: u64,
) -> Option<(String, bool)> {
    if let Some(e) = config_problem {
        return Some((format!("pilot is off: {e}"), true));
    }
    let cfg = config?;
    let host = cfg
        .url
        .split_once("://")
        .map_or(cfg.url.as_str(), |(_, rest)| rest);
    // The cards read, but the board didn't: still a problem to fix. A
    // Worker with no board at all is fine and isn't passed here.
    if let (None, Some(e)) = (problem, board_failed) {
        return Some((
            format!("Connected to {host}, but its board failed: {e}"),
            true,
        ));
    }
    Some(match (problem, last_read) {
        // Only a Worker that never answered is out of reach; one that
        // refused the token or failed did answer.
        (Some(e), _) => match e.strip_prefix("could not reach ") {
            Some(rest) => {
                let why = rest.split_once(": ").map_or(rest, |(_, why)| why);
                (format!("Can't reach {host}: {why}"), true)
            }
            None => (format!("Can't read {host}: {e}"), true),
        },
        (None, Some(t)) => {
            let ago = match now.saturating_sub(t) {
                0..60 => "just now".to_string(),
                s => format!("{} ago", super::board::span(s)),
            };
            (format!("Connected to {host} · read {ago}"), false)
        }
        (None, None) => (format!("Connecting to {host}…"), false),
    })
}

// ---------------------------------------------------------------------------
// The model

pub enum PilotEvent {
    /// New items that deserve a notice.
    New(Vec<Item>),
}

pub struct Pilot {
    config: Option<Config>,
    items: Vec<Item>,
    book: Book,
    /// No book on disk yet: the first read doesn't pop anything.
    fresh_book: bool,
    busy: bool,
    /// A refresh was asked for while a read was going.
    again: bool,
    problem: Option<String>,
    /// Why the settings file there is can't be used.
    config_problem: Option<String>,
    /// When the Worker last answered (unix seconds).
    last_read: Option<u64>,
    /// The last board read, kept while a later read fails.
    board: Option<super::board::Board>,
    /// Why the last board read failed; `Missing` while the Worker has none.
    board_problem: Option<super::board::BoardProblem>,
    window_active: bool,
    /// When the last read started.
    last_start: Option<Instant>,
    _poll: Task<()>,
}

impl EventEmitter<PilotEvent> for Pilot {}

/// One read of the Worker: the handled ids it was told, its items and its
/// board.
type Reads = (
    Vec<String>,
    Result<Vec<Item>, String>,
    Result<super::board::Board, super::board::BoardProblem>,
);

/// Whether the poll reads now. It keeps reading while the GitHub tab is
/// hidden: the notices of new items are for then.
fn poll_due(window_active: bool, since_start: Option<Duration>) -> bool {
    // A tick that comes a little early still counts.
    window_active || since_start.is_none_or(|e| e + POLL / 2 >= POLL_IDLE)
}

impl Pilot {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let poll = crate::poll::poll_every(cx, Duration::ZERO, POLL, |this, cx| {
            if poll_due(this.window_active, this.last_start.map(|t| t.elapsed())) {
                this.refresh(cx);
            }
        });
        let book = load_book();
        Self {
            config: None,
            items: Vec::new(),
            fresh_book: book.is_none(),
            book: book.unwrap_or_default(),
            busy: false,
            again: false,
            problem: None,
            config_problem: None,
            last_read: None,
            board: None,
            board_problem: None,
            window_active: true,
            last_start: None,
            _poll: poll,
        }
    }

    /// The window came to the front or went behind. Coming back, a read
    /// older than the usual gap is done again now.
    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        let back = active && !self.window_active;
        self.window_active = active;
        if back && self.last_start.is_none_or(|t| t.elapsed() >= POLL) {
            self.refresh(cx);
        }
    }

    /// The settings of the last read.
    pub fn config(&self) -> Option<&Config> {
        self.config.as_ref()
    }

    /// Why the last read failed.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Why the settings file can't be used: pilot is off until it is fixed.
    pub fn config_problem(&self) -> Option<&str> {
        self.config_problem.as_deref()
    }

    /// One line on how pilot is doing, and whether it is a problem; None
    /// while it was never set up.
    pub fn status(&self) -> Option<(String, bool)> {
        status_line(
            self.config.as_ref(),
            self.config_problem.as_deref(),
            self.problem.as_deref(),
            match &self.board_problem {
                Some(super::board::BoardProblem::Failed(e)) => Some(e.as_str()),
                _ => None,
            },
            self.last_read,
            crate::model::unix_now(),
        )
    }

    /// The board, once read.
    pub fn board(&self) -> Option<&super::board::Board> {
        self.board.as_ref()
    }

    /// Why the last board read failed.
    pub fn board_problem(&self) -> Option<&super::board::BoardProblem> {
        self.board_problem.as_ref()
    }

    /// The cards to show: not handled, newest first.
    pub fn shown(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| !self.book.handled(&i.id))
    }

    pub fn find(&self, id: &str) -> Option<&Item> {
        self.shown().find(|i| i.id == id)
    }

    /// The cards inside the tab's focus (owners and repositories): what is
    /// left out doesn't show, count or alert.
    pub fn shown_in<'a>(&'a self, focus: &'a [String]) -> impl Iterator<Item = &'a Item> {
        self.shown().filter(|i| i.in_focus(focus))
    }

    /// Whether a card in focus waits that would have popped the island.
    pub fn urgent(&self, focus: &[String]) -> bool {
        self.shown_in(focus).any(Item::pops)
    }

    /// Reads the settings again and, when there are some, the new items.
    /// Handled items the Worker missed are told first.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if super::GitHub::disabled() {
            return;
        }
        if self.busy {
            // A read already going may have started before an answer: one
            // more follows it.
            self.again = true;
            return;
        }
        self.busy = true;
        self.last_start = Some(Instant::now());
        let untold = self.book.untold.clone();
        cx.spawn(async move |this, cx| {
            // The settings file is read here too, off the main thread.
            let found = cx
                .background_spawn(crate::guard::task(async move {
                    let settings = read_config();
                    let config = settings.clone().ok().flatten();
                    // A board file stands in for the Worker even without settings.
                    let fixture = std::env::var_os(super::board::FIXTURE_ENV).is_some();
                    if config.is_none() && !fixture {
                        return Some((settings, None));
                    }
                    // The board and the items side by side: a slow one
                    // doesn't hold the other up.
                    // The handled ids the Worker missed go out beside them:
                    // the book hides those cards whatever the read says.
                    let reads = std::thread::scope(|s| {
                        let board = s.spawn(|| super::board::fetch(config.as_ref()));
                        let (told, read) = match &config {
                            None => (Vec::new(), Ok(Vec::new())),
                            Some(cfg) => {
                                let told = s.spawn(|| tell_all(cfg, untold));
                                let read = fetch(cfg);
                                (told.join().unwrap_or_default(), read)
                            }
                        };
                        let board = board.join().unwrap_or_else(|_| {
                            Err(super::board::BoardProblem::Failed(
                                "reading the board failed".into(),
                            ))
                        });
                        (told, read, board)
                    });
                    Some((settings, Some(reads)))
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                if let Some((settings, reads)) = found {
                    this.take_settings(settings, cx);
                    if let Some(reads) = reads {
                        this.take_reads(reads, cx);
                    }
                }
                if std::mem::take(&mut this.again) {
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    /// What the settings file said: other settings start over.
    fn take_settings(&mut self, settings: Result<Option<Config>, String>, cx: &mut Context<Self>) {
        let (config, config_problem) = match settings {
            Ok(c) => (c, None),
            Err(e) => (None, Some(e)),
        };
        if config_problem != self.config_problem {
            self.config_problem = config_problem;
            cx.notify();
        }
        if config != self.config {
            self.last_read = None;
            // Another Worker (or none) knows other items.
            self.items.clear();
            self.problem = None;
            self.board = None;
            self.board_problem = None;
            self.config = config;
            cx.notify();
        }
    }

    /// What the Worker said.
    fn take_reads(&mut self, (told, read, board): Reads, cx: &mut Context<Self>) {
        let before = self.book.clone();
        for id in &told {
            self.book.told(id);
        }
        match board {
            Ok(b) => {
                self.board = Some(b);
                self.board_problem = None;
            }
            Err(super::board::BoardProblem::Missing) => {
                self.board = None;
                self.board_problem = Some(super::board::BoardProblem::Missing);
            }
            Err(e) => self.board_problem = Some(e),
        }
        match read {
            Ok(read) => {
                self.problem = None;
                if self.config.is_some() {
                    self.last_read = Some(crate::model::unix_now());
                }
                let quiet = std::mem::take(&mut self.fresh_book);
                let pops = self.book.absorb(&mut self.items, read, quiet);
                if !pops.is_empty() {
                    cx.emit(PilotEvent::New(pops));
                }
            }
            Err(e) => self.problem = Some(e),
        }
        // Most reads change nothing: the file is written only when
        // something new was seen or told.
        if self.book != before {
            store_book(&self.book);
        }
        cx.notify();
    }

    /// The user dealt with `id`: its card goes, and the Worker is told.
    pub fn done(&mut self, id: &str, cx: &mut Context<Self>) {
        self.book.handle(id);
        store_book(&self.book);
        cx.notify();
        let Some(cfg) = self.config.clone() else {
            return;
        };
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let i = id.clone();
            let told = cx
                .background_spawn(crate::guard::task(async move { tell_done(&cfg, &i) }))
                .await;
            // A failure stays untold; the next read tries again.
            if told.is_ok() {
                let _ = this.update(cx, |this, _| {
                    this.book.told(&id);
                    store_book(&this.book);
                });
            }
        })
        .detach();
    }

    /// The user answered `id`: its card goes. The answer route already told
    /// the Worker, so `done` isn't sent.
    pub fn answered(&mut self, id: &str, cx: &mut Context<Self>) {
        self.book.hide(id);
        store_book(&self.book);
        cx.notify();
    }

    /// Every login a card or the board shows, for their pictures.
    pub fn people(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let board = self.board.iter().flat_map(|b| &b.prs);
        let logins = self
            .shown()
            .map(|i| i.author.as_str())
            .chain(board.flat_map(|p| [p.author.as_str(), p.lo_tiene.login.as_str()]));
        for login in logins {
            if !login.is_empty() && !out.iter().any(|o| o == login) {
                out.push(login.to_string());
            }
        }
        out
    }

    /// The ids shown, to keep view state (open points) only for those.
    pub fn shown_ids(&self) -> HashSet<String> {
        self.shown().map(|i| i.id.clone()).collect()
    }

    /// The item ids of the board's open questions.
    pub fn open_decisions(&self) -> HashSet<String> {
        self.board
            .as_ref()
            .map(super::board::Board::asking)
            .unwrap_or_default()
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn the_status_says_what_is_wrong_first() {
        let cfg = parse_config(br#"{"url": "https://p.dev", "token": "t"}"#).unwrap();
        assert_eq!(status_line(None, None, None, None, None, 0), None);
        let (off, bad) = status_line(
            None,
            Some("its settings have no token"),
            None,
            None,
            None,
            0,
        )
        .unwrap();
        assert!(bad && off.starts_with("pilot is off"));
        let (down, bad) = status_line(
            Some(&cfg),
            None,
            Some("could not reach https://p.dev: timed out"),
            None,
            Some(1),
            100,
        )
        .unwrap();
        assert!(bad, "{down}");
        assert_eq!(down, "Can't reach p.dev: timed out");
        let (refused, bad) = status_line(
            Some(&cfg),
            None,
            Some(&status_problem(401)),
            None,
            Some(1),
            100,
        )
        .unwrap();
        assert!(bad && refused.starts_with("Can't read p.dev: the Worker refused the token"));
        let (ok, bad) = status_line(Some(&cfg), None, None, None, Some(1000), 1000 + 125).unwrap();
        assert!(!bad);
        assert_eq!(ok, "Connected to p.dev · read 2 min ago");
        let (board, bad) =
            status_line(Some(&cfg), None, None, Some("500"), Some(1000), 1000).unwrap();
        assert!(bad && board.contains("board failed: 500"), "{board}");
    }

    #[test]
    fn a_broken_settings_file_says_what_is_missing() {
        assert!(config_problem(b"{oops").contains("isn't valid JSON"));
        assert!(config_problem(br#"{"token": "t"}"#).contains("no address"));
        assert!(config_problem(br#"{"url": "http://p.dev", "token": "t"}"#).contains("https://"));
        let wrong_type = br#"{"url": "https://p.dev", "token": "t", "admin_token": 123}"#;
        let e = config_problem(wrong_type);
        assert!(
            e.contains("admin token isn't text") && !e.contains("123"),
            "{e}"
        );
        assert!(!config_problem(br#"[1]"#).contains("https://"));
    }

    #[test]
    fn every_untold_id_is_told_past_one_batch() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let n = TELL_AT_ONCE + 3;
        std::thread::spawn(move || {
            for stream in listener.incoming().take(n) {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let cfg = Config {
            url: format!("http://127.0.0.1:{port}"),
            token: "t".into(),
            admin_token: None,
        };
        let ids: Vec<String> = (0..n).map(|i| format!("id{i}")).collect();
        let mut told = tell_all(&cfg, ids.clone());
        told.sort();
        let mut want = ids;
        want.sort();
        assert_eq!(told, want);
    }

    const FULL: &str = r#"{
        "id": "brief-95-40a1b2c",
        "at": "2026-11-15T17:02:11Z",
        "kind": "brief",
        "repo": "ArakunHQ/arakun",
        "number": 95,
        "title": "feat: name new environments after the branch",
        "author": "rikvisser-dev",
        "head": "40a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3",
        "url": "https://github.com/ArakunHQ/arakun/pull/95",
        "resumen": "Los entornos nuevos toman el nombre de su rama. Antes salían como env-3.",
        "toca": ["entornos", "src/env.rs"],
        "riesgo": "medio",
        "importante": true,
        "por_que": "Cambia nombres que la gente ya usa en scripts.",
        "recomendacion": "revisar_tu",
        "puntos": [{"archivo": "src/env.rs", "linea": 12, "texto": "Un nombre repetido pisa el otro."}],
        "borrador": "Two branches with the same name collide.",
        "commits": [],
        "pregunta": null,
        "modelo": "opus",
        "costo": {"usd": 0.4}
    }"#;

    #[test]
    fn reads_an_item_with_unknown_fields() {
        let items = parse_items(format!("[{FULL}]").as_bytes()).unwrap();
        let i = &items[0];
        assert_eq!(i.kind, Kind::Brief);
        assert_eq!((i.number, i.author.as_str()), (95, "rikvisser-dev"));
        assert_eq!(i.riesgo, Some(Riesgo::Medio));
        assert!(i.importante);
        assert_eq!(i.puntos[0].place(), "src/env.rs:12");
        assert_eq!(i.toca, ["entornos", "src/env.rs"]);
        // null reads as empty
        assert_eq!(i.pregunta, "");
        let r = i.item_ref().unwrap();
        assert_eq!(
            (r.repo.as_str(), r.number, r.kind),
            ("ArakunHQ/arakun", 95, ItemKind::Pull)
        );
        // The wrapped answer reads the same.
        let wrapped = parse_items(format!("{{\"items\": [{FULL}], \"next\": null}}").as_bytes());
        assert_eq!(wrapped.unwrap(), items);
    }

    #[test]
    fn reads_an_item_with_missing_fields() {
        let items = parse_items(
            r#"[{"id": "a", "kind": "needs_you", "pregunta": "¿Subo la versión mayor?"},
                 {"id": "b", "kind": "rebuilt", "riesgo": "extremo", "puntos": [{"texto": "x", "linea": null}]},
                 {"kind": "fixed"}]"#
                .as_bytes(),
        )
        .unwrap();
        // Without an id nothing could mark it done: it is dropped.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, Kind::NeedsYou);
        assert_eq!(items[0].riesgo, None);
        assert!(items[0].item_ref().is_none());
        assert_eq!(items[1].kind, Kind::Other);
        assert_eq!(items[1].riesgo, Some(Riesgo::Otro));
        assert_eq!(items[1].puntos[0].place(), "");
        // The number can come from the address alone.
        let by_url = Item {
            url: "https://github.com/ArakunHQ/arakun/pull/88".into(),
            ..Item::default()
        };
        assert_eq!(by_url.item_ref().map(|r| r.number), Some(88));
        // An answer with no list of items is an error: the cards on show stay.
        assert!(parse_items(b"{\"error\": \"nope\"}").is_err());
        assert!(parse_items(b"{\"items\": null}").is_err());
        assert!(parse_items(b"\"nope\"").is_err());
        assert!(parse_items(b"<html>").is_err());
        // One item with a field of the wrong type goes; the rest read.
        let mixed = parse_items(br#"[{"id": "a", "number": "95"}, {"id": "b", "number": 95}]"#);
        let ids: Vec<String> = mixed.unwrap().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, ["b"]);
    }

    #[test]
    fn the_tabs_focus_filters_the_cards() {
        let card = |repo: &str| Item {
            repo: repo.into(),
            ..Item::default()
        };
        let focus = vec!["other-org".to_string()];
        assert!(!card("ArakunHQ/arakun").in_focus(&focus));
        assert!(card("other-org/x").in_focus(&focus));
        // No focus keeps everything; a card about no repository always shows.
        assert!(card("ArakunHQ/arakun").in_focus(&[]));
        assert!(card("").in_focus(&focus));
    }

    #[test]
    fn an_item_names_a_pull_request_only_in_pilots_repository() {
        let other = Item {
            repo: "other/proj".into(),
            number: 95,
            ..Item::default()
        };
        assert!(other.item_ref().is_none());
        let elsewhere = Item {
            url: "https://github.com/other/proj/pull/95".into(),
            ..Item::default()
        };
        assert!(elsewhere.item_ref().is_none());
        // An address on another host names nothing, even with the right path.
        let host = Item {
            url: "https://evil.example/ArakunHQ/arakun/pull/95".into(),
            ..Item::default()
        };
        assert!(host.item_ref().is_none());
        let ours = Item {
            repo: "arakunhq/arakun".into(),
            number: 95,
            ..Item::default()
        };
        assert_eq!(ours.item_ref().map(|r| r.number), Some(95));
    }

    fn item(id: &str, at: &str, kind: Kind, importante: bool) -> Item {
        Item {
            id: id.into(),
            at: at.into(),
            kind,
            importante,
            number: 95,
            resumen: "Primera frase. Segunda.".into(),
            ..Item::default()
        }
    }

    #[test]
    fn only_what_needs_the_user_pops_the_island() {
        assert!(item("a", "", Kind::NeedsYou, false).pops());
        assert!(item("a", "", Kind::Brief, true).pops());
        assert!(!item("a", "", Kind::Brief, false).pops());
        assert!(!item("a", "", Kind::Fixed, true).pops());
        assert!(!item("a", "", Kind::Failed, false).pops());
        assert!(!item("a", "", Kind::Other, true).pops());

        let brief = item("a", "", Kind::Brief, true);
        assert_eq!(
            brief.notice(),
            ("Your agent · #95".to_string(), "Primera frase.".to_string())
        );
        let mut ask = item("b", "", Kind::NeedsYou, false);
        ask.pregunta = "¿Le contesto a Rik que no?".into();
        assert_eq!(ask.notice().1, "¿Le contesto a Rik que no?");
    }

    #[test]
    fn a_window_behind_reads_every_five_minutes() {
        assert!(poll_due(true, Some(POLL)));
        assert!(poll_due(false, None));
        assert!(!poll_due(false, Some(POLL)));
        assert!(!poll_due(false, Some(4 * POLL)));
        // The fifth tick, a moment early.
        assert!(poll_due(false, Some(POLL_IDLE - Duration::from_millis(5))));
    }

    #[test]
    fn a_sentence_ends_at_its_stop() {
        assert_eq!(
            first_sentence("Sube a v1.2 en src/x.rs. Luego más."),
            "Sube a v1.2 en src/x.rs."
        );
        assert_eq!(first_sentence("¿Lo apruebo? Sí"), "¿Lo apruebo?");
        assert_eq!(first_sentence("  sin punto  "), "sin punto");
    }

    #[test]
    fn the_book_pops_each_item_once_and_hides_handled_ones() {
        let mut book = Book::default();
        let mut items = Vec::new();
        // The first read ever only learns what is there.
        let first = vec![item("old", "2026-11-15T10:00:00Z", Kind::NeedsYou, false)];
        assert!(book.absorb(&mut items, first, true).is_empty());

        let read = vec![
            item("q", "2026-11-15T12:00:00Z", Kind::NeedsYou, false),
            item("b", "2026-11-15T11:00:00Z", Kind::Brief, false),
            item("i", "2026-11-15T13:00:00Z", Kind::Brief, true),
        ];
        let pops = book.absorb(&mut items, read.clone(), false);
        assert_eq!(
            pops.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["q", "i"]
        );
        // Newest first, and only what this read brought: "old" was closed.
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["i", "q", "b"]
        );
        // A read is everything still open: what the Worker closed ("old")
        // goes, and nothing pops twice.
        assert!(book.absorb(&mut items, read.clone(), false).is_empty());
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["i", "q", "b"]
        );
        // A newer brief of the same PR closed "b" there: it goes here too.
        let mut newer = read;
        newer.retain(|i| i.id != "b");
        newer.push(item("b2", "2026-11-15T13:30:00Z", Kind::Brief, false));
        assert!(book.absorb(&mut items, newer, false).is_empty());
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["b2", "i", "q"]
        );

        // Handled: hidden, owed to the Worker until it is told.
        book.handle("q");
        book.handle("q");
        assert!(book.handled("q"));
        assert_eq!(book.untold, ["q"]);
        book.told("q");
        assert!(book.untold.is_empty() && book.handled("q"));

        // An item handled elsewhere and seen for the first time doesn't pop.
        book.handle("late");
        let late = vec![item("late", "2026-11-15T14:00:00Z", Kind::NeedsYou, false)];
        assert!(book.absorb(&mut items, late, false).is_empty());

        // The book survives a restart as JSON.
        let back: Book = serde_json::from_slice(&serde_json::to_vec(&book).unwrap()).unwrap();
        assert_eq!(back, book);
        // And keeps only the newest ids.
        let mut big = Book::default();
        for n in 0..BOOK_MAX + 5 {
            big.handle(&n.to_string());
        }
        assert_eq!(big.handled.len(), BOOK_MAX);
        assert_eq!(big.handled[0], "5");
    }

    #[test]
    fn the_config_follows_the_state_or_xdg() {
        let home = PathBuf::from("/home/k");
        assert_eq!(
            config_file_from(None, None, home.clone()),
            PathBuf::from("/home/k/.config/arakun-pilot/client.json")
        );
        assert_eq!(
            config_file_from(None, Some("/x/cfg".into()), home.clone()),
            PathBuf::from("/x/cfg/arakun-pilot/client.json")
        );
        // A relative XDG_CONFIG_HOME is ignored, as the spec says.
        assert_eq!(
            config_file_from(None, Some("cfg".into()), home.clone()),
            PathBuf::from("/home/k/.config/arakun-pilot/client.json")
        );
        // A test Arakun keeps its own.
        assert_eq!(
            config_file_from(Some("/nest/state.json".into()), Some("/x".into()), home),
            PathBuf::from("/nest/arakun-pilot/client.json")
        );
    }

    #[test]
    fn the_config_needs_an_address_and_a_token() {
        let c =
            parse_config(br#"{"url": "https://pilot.example.dev/", "token": " t0k "}"#).unwrap();
        assert_eq!(
            (c.url.as_str(), c.token.as_str()),
            ("https://pilot.example.dev", "t0k")
        );
        assert!(!format!("{c:?}").contains("t0k"));
        assert_eq!(c.admin_token, None);
        let a =
            parse_config(br#"{"url": "https://p.dev", "token": "t", "admin_token": " s3cr3t "}"#)
                .unwrap();
        assert_eq!(a.admin_token.as_deref(), Some("s3cr3t"));
        assert!(!format!("{a:?}").contains("s3cr3t"));
        // An empty one is none.
        let e = parse_config(br#"{"url": "https://p.dev", "token": "t", "admin_token": ""}"#);
        assert_eq!(e.unwrap().admin_token, None);
        assert!(parse_config(br#"{"url": "https://p.dev", "token": ""}"#).is_none());
        assert!(parse_config(br#"{"url": "file:///etc", "token": "t"}"#).is_none());
        for local in [
            "http://localhost:8787",
            "http://127.0.0.1",
            "http://[::1]:8787/",
        ] {
            let json = format!(r#"{{"url": "{local}", "token": "t"}}"#);
            assert!(parse_config(json.as_bytes()).is_some(), "{local}");
        }
        for remote in [
            "http://pilot.example.dev",
            "http://localhost.example.dev",
            "http://127.0.0.1.example.dev",
            "http://localhost@evil.dev",
            "http://[::1]evil.dev",
        ] {
            let json = format!(r#"{{"url": "{remote}", "token": "t"}}"#);
            assert!(parse_config(json.as_bytes()).is_none(), "{remote}");
        }
        assert!(parse_config(b"nope").is_none());
    }

    #[test]
    fn plain_http_only_goes_to_this_machine() {
        let ok = |url: &str| {
            let json = format!(r#"{{"url": "{url}", "token": "t"}}"#);
            parse_config(json.as_bytes()).is_some()
        };
        assert!(ok("http://127.0.0.1:8787"));
        assert!(ok("http://localhost:8787/"));
        assert!(ok("http://LOCALHOST"));
        assert!(ok("http://[::1]:8787"));
        assert!(ok("https://pilot.example.com"));
        // Anything else would send the tokens over the network unencrypted.
        assert!(!ok("http://evil.com"));
        assert!(!ok("http://pilot.example.com:8787"));
        assert!(!ok("http://localhost.evil.com"));
        assert!(!ok("http://127.0.0.1.evil.com"));
        assert!(!ok("http://127.0.0.1@evil.com"));
        assert!(!ok("http://localhost:80@evil.com/"));
        assert!(!ok("http://[::1]evil.com"));
    }

    #[test]
    fn commits_match_only_by_their_full_sha() {
        let full = "40a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3";
        assert!(full_sha(full));
        assert!(same_commit(full, &full.to_ascii_uppercase()));
        // A short name could be read as another commit.
        assert!(!same_commit("40a1b2c", full));
        assert!(!full_sha("40a1b2c"));
        assert!(!full_sha(&format!("{}z", &full[..39])));
        assert!(!same_commit("", ""));
        assert!(!same_commit(
            "50a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3",
            full
        ));
    }

    #[test]
    fn reads_ready_answers() {
        let items = parse_items(
            r#"[{"id": "q", "kind": "needs_you", "pregunta": "¿Migro?",
                  "opciones": [{"id": "migrar", "texto": "Sí, con migración"},
                               {"id": "", "texto": "sin id"},
                               {"id": "x", "texto": "  "},
                               {"id": "quedan", "texto": "No, se quedan"},
                               {"id": "a", "texto": "A"}, {"id": "b", "texto": "B"},
                               {"id": "skip", "texto": "Lo veo yo"}]},
                 {"id": "n", "kind": "needs_you", "opciones": null},
                 {"id": "m", "kind": "needs_you"}]"#
                .as_bytes(),
        )
        .unwrap();
        // The broken ones go, and at most four stay.
        let ids: Vec<&str> = items[0].opciones.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["migrar", "quedan", "a", "b"]);
        assert_eq!(items[0].opciones[0].texto, "Sí, con migración");
        assert!(items[1].opciones.is_empty() && items[2].opciones.is_empty());
        assert!(
            Opcion {
                id: "skip".into(),
                texto: "Lo veo yo".into()
            }
            .quiet()
        );
    }

    #[test]
    fn answer_and_task_bodies() {
        assert_eq!(
            answer_body(&Answer::Opcion("migrar".into())),
            serde_json::json!({"opcion": "migrar"})
        );
        assert_eq!(
            answer_body(&Answer::Texto("  que se queden \n".into())),
            serde_json::json!({"texto": "que se queden"})
        );
        let to_pull = TaskRequest {
            title: "arregla el tab".into(),
            prompt: "arregla el tab y su prueba".into(),
            target: TaskTarget::Pull(112),
        };
        assert_eq!(
            task_body(&to_pull),
            serde_json::json!({"title": "arregla el tab", "prompt": "arregla el tab y su prueba", "pr": 112})
        );
        let to_branch = TaskRequest {
            target: TaskTarget::Branch {
                name: "pilot/arregla-el-tab".into(),
                base: "main".into(),
            },
            ..to_pull
        };
        assert_eq!(
            task_body(&to_branch),
            serde_json::json!({"title": "arregla el tab", "prompt": "arregla el tab y su prueba",
                               "branch": "pilot/arregla-el-tab", "base": "main"})
        );
        assert_eq!(
            parse_task_id(br#"{"id": "t_9", "ok": true}"#).as_deref(),
            Some("t_9")
        );
        assert_eq!(
            parse_task_id(br#"{"task": {"id": 42}}"#).as_deref(),
            Some("42")
        );
        assert_eq!(parse_task_id(br#"{"ok": true}"#), None);
        assert_eq!(parse_task_id(b"queued"), None);
    }

    #[test]
    fn refused_writes_say_why_without_the_token() {
        assert_eq!(
            write_problem(409, br#"{"error": "la rama ya existe"}"#, true),
            "the Worker answered 409 (la rama ya existe)"
        );
        assert!(write_problem(403, b"", true).contains("admin token"));
        assert!(write_problem(401, b"", false).contains("refused the token"));
        assert_eq!(
            write_problem(500, b"<html>", false),
            "the Worker answered 500"
        );
        // A task without the admin token never leaves.
        let cfg = parse_config(br#"{"url": "http://127.0.0.1:9", "token": "t"}"#).unwrap();
        let t = TaskRequest {
            title: "x".into(),
            prompt: "x".into(),
            target: TaskTarget::Pull(1),
        };
        assert_eq!(send_task(&cfg, &t).unwrap_err(), NO_ADMIN);
    }

    #[test]
    fn an_answered_item_is_hidden_but_not_owed() {
        let mut book = Book::default();
        book.hide("q");
        assert!(book.handled("q") && book.untold.is_empty());
    }

    #[test]
    fn times_and_ids_are_encoded() {
        assert_eq!(
            encode("2026-11-15T12:00:00+00:00"),
            "2026-11-15T12%3A00%3A00%2B00%3A00"
        );
        assert_eq!(encode("a/b c"), "a%2Fb%20c");
    }

    /// A keyring in memory. `locked` refuses everything, as one that stayed
    /// locked does.
    #[derive(Default)]
    pub(in crate::github) struct Fake {
        pub entries: std::cell::RefCell<std::collections::HashMap<String, String>>,
        pub locked: bool,
    }

    impl Vault for Fake {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            if self.locked {
                return Err("locked".into());
            }
            Ok(self.entries.borrow().get(account).cloned())
        }

        fn set(&self, account: &str, _label: &str, secret: &str) -> Result<(), String> {
            if self.locked {
                return Err("locked".into());
            }
            self.entries
                .borrow_mut()
                .insert(account.to_string(), secret.to_string());
            Ok(())
        }

        fn forget(&self, account: &str) {
            self.entries.borrow_mut().remove(account);
        }
    }

    pub(in crate::github) fn accounts() -> Accounts {
        Accounts::for_state(None)
    }

    pub(in crate::github) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-pilot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("client.json")
    }

    fn write(path: &Path, json: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, json).unwrap();
    }

    #[test]
    fn tokens_in_an_old_file_move_to_the_keyring() {
        let (ring, path) = (Fake::default(), scratch("move"));
        write(
            &path,
            r#"{"url": "https://p.dev/", "token": " t ", "admin_token": "a", "note": "mine"}"#,
        );
        let cfg = read_config_at(&path, &ring, &accounts()).unwrap().unwrap();
        assert_eq!(
            (cfg.url.as_str(), cfg.token.as_str()),
            ("https://p.dev", "t")
        );
        assert_eq!(cfg.admin_token.as_deref(), Some("a"));
        assert_eq!(ring.entries.borrow()["arakun-pilot-token"], "t");
        assert_eq!(ring.entries.borrow()["arakun-pilot-admin-token"], "a");
        let left: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            left,
            serde_json::json!({ "url": "https://p.dev/", "note": "mine" })
        );
        // Read again, the tokens come from the keyring.
        let again = read_config_at(&path, &ring, &accounts()).unwrap().unwrap();
        assert_eq!(again, cfg);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_keyring_that_refuses_leaves_the_tokens_in_the_file() {
        let path = scratch("refuse");
        let ring = Fake {
            locked: true,
            ..Fake::default()
        };
        let old = r#"{"url": "https://p.dev", "token": "t"}"#;
        write(&path, old);
        // The file's token is still used; the admin token can't be read.
        let e = read_config_at(&path, &ring, &accounts()).unwrap_err();
        assert!(e.contains("keyring") && e.contains("admin token"), "{e}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
        let both = r#"{"url": "https://p.dev", "token": "t", "admin_token": "a"}"#;
        write(&path, both);
        let cfg = read_config_at(&path, &ring, &accounts()).unwrap().unwrap();
        assert_eq!(cfg.token, "t");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), both);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_file_without_a_token_in_the_keyring_says_so() {
        let (ring, path) = (Fake::default(), scratch("none"));
        write(&path, r#"{"url": "https://p.dev"}"#);
        let e = read_config_at(&path, &ring, &accounts()).unwrap_err();
        assert!(e.contains("no token"), "{e}");
        ring.put("arakun-pilot-token", "t");
        let cfg = read_config_at(&path, &ring, &accounts()).unwrap().unwrap();
        assert_eq!((cfg.token.as_str(), cfg.admin_token), ("t", None));
        let locked = Fake {
            locked: true,
            ..Fake::default()
        };
        let e = read_config_at(&path, &locked, &accounts()).unwrap_err();
        assert!(e.contains("keyring could not give its token"), "{e}");
        write(&path, r#"{"url": "http://p.dev"}"#);
        let e = read_config_at(&path, &ring, &accounts()).unwrap_err();
        assert!(e.contains("https://"), "{e}");
        assert_eq!(
            read_config_at(&scratch("gone"), &ring, &accounts()),
            Ok(None)
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    impl Fake {
        fn put(&self, account: &str, secret: &str) {
            self.entries
                .borrow_mut()
                .insert(account.into(), secret.into());
        }
    }

    #[test]
    fn a_test_arakun_names_the_tokens_by_its_state_folder() {
        assert_eq!(accounts().token, "arakun-pilot-token");
        let dir = std::env::temp_dir().join("dev");
        let state = dir.join("state.json");
        let a = Accounts::for_state(Some(state.as_os_str()));
        let folder = dir.to_str().unwrap();
        assert_eq!(a.token, format!("arakun-pilot-token@{folder}"));
        assert_eq!(a.admin, format!("arakun-pilot-admin-token@{folder}"));
        // Another state file in the same folder shares client.json, and so
        // the tokens.
        let other = Accounts::for_state(Some(dir.join("other.json").as_os_str()));
        assert_eq!(other.token, a.token);
    }
}
