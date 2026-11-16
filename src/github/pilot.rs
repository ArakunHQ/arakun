//! arakun-pilot: the user's pull request agent on Cloudflare (~/arakun/pilot-cf).
//! It reviews other people's pull requests and fixes the user's own, then
//! leaves items (Spanish summaries) at `GET /items`. Arakun reads them every
//! minute, shows them at the top of the GitHub tab and tells the Worker
//! (`POST /items/<id>/done`) when the user dealt with one.
//!
//! Off unless `client.json` (`{"url": "...", "token": "..."}`) exists.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{AppContext as _, Context, EventEmitter, Task};
use serde::{Deserialize, Deserializer, Serialize};

use super::pr::{ItemKind, ItemRef};

const POLL: Duration = Duration::from_secs(60);
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
}

// The token never reaches a log, not even through `{:?}`.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("url", &self.url)
            .field("token", &"<hidden>")
            .finish()
    }
}

/// The settings in `bytes`; None when they are missing a part.
pub fn parse_config(bytes: &[u8]) -> Option<Config> {
    let c: Config = serde_json::from_slice(bytes).ok()?;
    let url = c.url.trim().trim_end_matches('/').to_string();
    let token = c.token.trim().to_string();
    let web = url.starts_with("https://") || url.starts_with("http://");
    (web && !token.is_empty()).then_some(Config { url, token })
}

pub fn load_config() -> Option<Config> {
    parse_config(&std::fs::read(config_file()).ok()?)
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
            Kind::Brief => "Para revisar",
            Kind::Fixed => "Arreglado",
            Kind::NeedsYou => "Te necesita",
            Kind::Failed => "Falló",
            Kind::Other => "Aviso",
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
            Riesgo::Bajo => "Riesgo bajo",
            Riesgo::Medio => "Riesgo medio",
            Riesgo::Alto => "Riesgo alto",
            Riesgo::Otro => "Riesgo ?",
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
}

/// A null reads as the field's default, like a missing one.
fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

impl Item {
    /// The pull request it is about, when it names one.
    pub fn item_ref(&self) -> Option<ItemRef> {
        let from_repo = (self.number > 0)
            .then(|| ItemRef::parse(&format!("{}#{}", self.repo, self.number)))
            .flatten();
        from_repo
            .or_else(|| ItemRef::from_url(&self.url))
            .map(|i| ItemRef {
                kind: ItemKind::Pull,
                ..i
            })
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

    /// The island notice: "Tu agente · #95" and the question, or the first
    /// sentence of the summary.
    pub fn notice(&self) -> (String, String) {
        let title = if self.number > 0 {
            format!("Tu agente · #{}", self.number)
        } else {
            "Tu agente".to_string()
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

/// Whether two names of a commit are the same commit: equal, or one a
/// prefix of the other (a short SHA), never shorter than 7.
pub fn same_commit(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().to_ascii_lowercase(), b.trim().to_ascii_lowercase());
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.len() >= 7 && long.starts_with(&short)
}

/// What `GET /items` answers: a list, or `{"items": [...]}`. Items without an
/// id are dropped: nothing could mark them done.
pub fn parse_items(bytes: &[u8]) -> Result<Vec<Item>, String> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Answer {
        List(Vec<Item>),
        Wrapped {
            #[serde(default)]
            items: Vec<Item>,
        },
    }
    let answer: Answer =
        serde_json::from_slice(bytes).map_err(|e| format!("respuesta inesperada: {e}"))?;
    let items = match answer {
        Answer::List(l) | Answer::Wrapped { items: l } => l,
    };
    Ok(items.into_iter().filter(|i| !i.id.is_empty()).collect())
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

    pub fn told(&mut self, id: &str) {
        self.untold.retain(|u| u != id);
    }

    /// Takes in what a read brought: `items` gets the new ones and the
    /// changes (by id), newest first. Returns those that pop the island:
    /// never seen, not handled, asking for the user. `quiet` (the first read
    /// ever) only learns what is there.
    pub fn absorb(&mut self, items: &mut Vec<Item>, read: Vec<Item>, quiet: bool) -> Vec<Item> {
        let mut pops = Vec::new();
        for item in read {
            if !self.seen.contains(&item.id) {
                self.seen.push(item.id.clone());
                if !quiet && !self.handled(&item.id) && item.pops() {
                    pops.push(item.clone());
                }
            }
            match items.iter_mut().find(|i| i.id == item.id) {
                Some(old) => *old = item,
                None => items.push(item),
            }
        }
        trim(&mut self.seen);
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
    serde_json::from_slice(&std::fs::read(book_file()).ok()?).ok()
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

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(concat!("arakun/", env!("CARGO_PKG_VERSION")))
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
fn status_problem(status: u16) -> String {
    match status {
        401 | 403 => "el Worker rechazó la llave (revisa el token de client.json)".into(),
        _ => format!("el Worker contestó {status}"),
    }
}

/// The items after `since` (all the Worker keeps without it).
pub fn fetch(cfg: &Config, since: Option<&str>) -> Result<Vec<Item>, String> {
    let mut url = format!("{}/items", cfg.url);
    if let Some(s) = since.filter(|s| !s.is_empty()) {
        url.push_str("?since=");
        url.push_str(&encode(s));
    }
    let mut res = agent()
        .get(&url)
        .header("Authorization", format!("Bearer {}", cfg.token))
        .call()
        .map_err(|e| format!("no pude llegar a {}: {e}", cfg.url))?;
    let status = res.status().as_u16();
    if status != 200 {
        return Err(status_problem(status));
    }
    let bytes = res
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("el Worker cortó la respuesta: {e}"))?;
    parse_items(&bytes)
}

/// Tells the Worker the user dealt with `id`. One it no longer knows is done too.
pub fn tell_done(cfg: &Config, id: &str) -> Result<(), String> {
    let res = agent()
        .post(format!("{}/items/{}/done", cfg.url, encode(id)))
        .header("Authorization", format!("Bearer {}", cfg.token))
        .send_empty()
        .map_err(|e| format!("no pude llegar a {}: {e}", cfg.url))?;
    match res.status().as_u16() {
        200..=299 | 404 => Ok(()),
        s => Err(status_problem(s)),
    }
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
    /// The newest `at` read this run; the next read asks for what is after it.
    since: Option<String>,
    busy: bool,
    problem: Option<String>,
    _poll: Task<()>,
}

impl EventEmitter<PilotEvent> for Pilot {}

impl Pilot {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(POLL).await;
            }
        });
        let book = load_book();
        Self {
            config: None,
            items: Vec::new(),
            fresh_book: book.is_none(),
            book: book.unwrap_or_default(),
            since: None,
            busy: false,
            problem: None,
            _poll: poll,
        }
    }

    /// Why the last read failed.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// The cards to show: not handled, newest first.
    pub fn shown(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| !self.book.handled(&i.id))
    }

    pub fn find(&self, id: &str) -> Option<&Item> {
        self.shown().find(|i| i.id == id)
    }

    pub fn count(&self) -> usize {
        self.shown().count()
    }

    /// Whether a card waits that would have popped the island.
    pub fn urgent(&self) -> bool {
        self.shown().any(Item::pops)
    }

    /// Reads the settings again and, when there are some, the new items.
    /// Handled items the Worker missed are told first.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy || super::GitHub::disabled() {
            return;
        }
        let config = load_config();
        if config != self.config {
            // Another Worker (or none) knows other items.
            self.items.clear();
            self.since = None;
            self.problem = None;
            self.config = config.clone();
            cx.notify();
        }
        let Some(cfg) = config else { return };
        self.busy = true;
        let since = self.since.clone();
        let untold = self.book.untold.clone();
        cx.spawn(async move |this, cx| {
            let (told, read) = cx
                .background_spawn(async move {
                    let told: Vec<String> = untold
                        .into_iter()
                        .filter(|id| tell_done(&cfg, id).is_ok())
                        .collect();
                    (told, fetch(&cfg, since.as_deref()))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                for id in &told {
                    this.book.told(id);
                }
                match read {
                    Ok(read) => {
                        this.problem = None;
                        if let Some(newest) = read.iter().map(|i| &i.at).max()
                            && this.since.as_ref().is_none_or(|s| newest > s)
                        {
                            this.since = Some(newest.clone());
                        }
                        let quiet = std::mem::take(&mut this.fresh_book);
                        let pops = this.book.absorb(&mut this.items, read, quiet);
                        store_book(&this.book);
                        if !pops.is_empty() {
                            cx.emit(PilotEvent::New(pops));
                        }
                    }
                    Err(e) => {
                        this.problem = Some(e);
                        if !told.is_empty() {
                            store_book(&this.book);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
                .background_spawn(async move { tell_done(&cfg, &i) })
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

    /// Every login a card shows, for their pictures.
    pub fn people(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for i in self.shown() {
            if !i.author.is_empty() && !out.contains(&i.author) {
                out.push(i.author.clone());
            }
        }
        out
    }

    /// The ids shown, to keep view state (open points) only for those.
    pub fn shown_ids(&self) -> HashSet<String> {
        self.shown().map(|i| i.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(parse_items(b"{\"error\": \"nope\"}").unwrap().is_empty());
        assert!(parse_items(b"<html>").is_err());
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
            ("Tu agente · #95".to_string(), "Primera frase.".to_string())
        );
        let mut ask = item("b", "", Kind::NeedsYou, false);
        ask.pregunta = "¿Le contesto a Rik que no?".into();
        assert_eq!(ask.notice().1, "¿Le contesto a Rik que no?");
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
        // Newest first.
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["i", "q", "b", "old"]
        );
        // Read again (a restart reads everything): nothing pops twice.
        assert!(book.absorb(&mut items, read, false).is_empty());
        assert_eq!(items.len(), 4);

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
        assert!(parse_config(br#"{"url": "https://p.dev", "token": ""}"#).is_none());
        assert!(parse_config(br#"{"url": "file:///etc", "token": "t"}"#).is_none());
        assert!(parse_config(b"nope").is_none());
    }

    #[test]
    fn commits_match_by_prefix() {
        let full = "40a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3";
        assert!(same_commit(full, full));
        assert!(same_commit("40A1B2C", full));
        assert!(!same_commit("40a1b2", full));
        assert!(!same_commit("", ""));
        assert!(!same_commit("50a1b2c3", full));
    }

    #[test]
    fn times_and_ids_are_encoded() {
        assert_eq!(
            encode("2026-11-15T12:00:00+00:00"),
            "2026-11-15T12%3A00%3A00%2B00%3A00"
        );
        assert_eq!(encode("a/b c"), "a%2Fb%20c");
    }
}
