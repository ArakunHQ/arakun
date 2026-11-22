//! The shell's side of the Dynamic Island (`crate::island_host`): the link to
//! it, what it shows of every project's sessions, its alerts, and "View".

// The island runs on Linux (Wayland) and macOS: elsewhere this builds but does nothing.
#![cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(unused_variables, unused_imports, dead_code, clippy::needless_return)
)]

use super::*;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::sessions::Notice;
use crate::agent::{Entry, Setting, Thread, ThreadStatus};
use crate::island::{self as isl, AlertKind, one_line};
use crate::island_host::brief;
use crate::island_host::proto::{FromIsland, ToIsland};

/// How often the island hears from the sessions: elapsed times tick in seconds.
const FEED_EVERY: Duration = Duration::from_secs(1);
/// Finished and failed sessions stay in the list this long.
const RECENT_SECS: u64 = 10 * 60;

pub(super) struct IslandLink {
    /// To the link's thread; dropped, it hangs up.
    tx: std::sync::mpsc::Sender<ToIsland>,
    /// The island is connected: what is sent now reaches it.
    up: Arc<AtomicBool>,
    /// The feed the island has, so an unchanged one isn't sent again.
    sent: Option<isl::Feed>,
    _events: Task<()>,
    _feed: Task<()>,
    /// What each session showed while it was live, for after it is retired.
    seen: HashMap<SharedString, Seen>,
}

#[derive(Clone)]
struct Seen {
    model: SharedString,
    doing: SharedString,
    elapsed: SharedString,
    cost: SharedString,
    usd: f64,
}

/// What "View" opens for a notice that isn't a session's turn. On the island it
/// is the alert's key: a session's id as it is, anything else `open:…`, so it
/// never passes for a session.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Open {
    Session(SharedString),
    /// The Runs tab of the project at `root`, at a run's page when set.
    Runs {
        root: PathBuf,
        run: Option<u64>,
    },
    Branches {
        root: PathBuf,
    },
    /// A GitHub notification, by thread id; the inbox when a notice was about several.
    GitHub(Option<String>),
    /// A card of the user's PR agent in the GitHub tab, by item id.
    Pilot(String),
    /// A CoderSpace task, by id.
    #[cfg(feature = "coderlabs")]
    Task(String),
    Cloudflare,
    Usage,
    /// The page that says how to install an agent's CLI, in the system browser.
    Install(crate::agent::AgentKind),
    /// Settings, at Branches.
    BranchSettings,
    /// The error feedback form, linked to the error report that was sent (if
    /// any), with the agent and the fixed category of its error.
    Feedback {
        error_event: Option<String>,
        agent: Option<String>,
        category: Option<String>,
    },
    /// A link a teammate's find points at, in the system browser.
    #[cfg(feature = "team")]
    Url(String),
    /// The Team tab: where a request from an admin waits for an answer.
    #[cfg(feature = "team")]
    Team,
}

impl Open {
    pub(super) fn key(&self) -> SharedString {
        match self {
            Open::Session(id) => id.clone(),
            Open::Runs { root, run: None } => format!("open:runs:{}", root.display()).into(),
            Open::Runs {
                root,
                run: Some(run),
            } => format!("open:run:{run}:{}", root.display()).into(),
            Open::Branches { root } => format!("open:branches:{}", root.display()).into(),
            Open::GitHub(None) => "open:github".into(),
            Open::GitHub(Some(thread)) => format!("open:github:{thread}").into(),
            Open::Pilot(id) => format!("open:pilot:{id}").into(),
            #[cfg(feature = "coderlabs")]
            Open::Task(id) => format!("open:task:{id}").into(),
            Open::Cloudflare => "open:cloudflare".into(),
            Open::Usage => "open:usage".into(),
            Open::Install(kind) => format!("open:install:{}", kind.id()).into(),
            Open::BranchSettings => "open:settings:branches".into(),
            Open::Feedback {
                error_event: None,
                agent: None,
                category: None,
            } => "open:feedback".into(),
            // None is an empty part: ids and categories hold no ":".
            Open::Feedback {
                error_event,
                agent,
                category,
            } => format!(
                "open:feedback:{}:{}:{}",
                error_event.as_deref().unwrap_or(""),
                agent.as_deref().unwrap_or(""),
                category.as_deref().unwrap_or("")
            )
            .into(),
            #[cfg(feature = "team")]
            Open::Url(url) => format!("open:url:{url}").into(),
            #[cfg(feature = "team")]
            Open::Team => "open:team".into(),
        }
    }

    /// `None`: an `open:…` this build doesn't know.
    fn parse(key: &str) -> Option<Open> {
        let Some(what) = key.strip_prefix("open:") else {
            return Some(Open::Session(key.to_string().into()));
        };
        let (kind, rest) = what.split_once(':').unwrap_or((what, ""));
        match (kind, rest) {
            ("runs", root) if !root.is_empty() => Some(Open::Runs {
                root: root.into(),
                run: None,
            }),
            ("run", rest) => {
                let (run, root) = rest.split_once(':')?;
                Some(Open::Runs {
                    root: root.into(),
                    run: Some(run.parse().ok()?),
                })
            }
            ("branches", root) if !root.is_empty() => Some(Open::Branches { root: root.into() }),
            ("github", "") => Some(Open::GitHub(None)),
            ("github", thread) => Some(Open::GitHub(Some(thread.to_string()))),
            ("pilot", id) if !id.is_empty() => Some(Open::Pilot(id.to_string())),
            #[cfg(feature = "coderlabs")]
            ("task", id) if !id.is_empty() => Some(Open::Task(id.to_string())),
            ("cloudflare", "") => Some(Open::Cloudflare),
            ("usage", "") => Some(Open::Usage),
            ("install", id) => crate::agent::AgentKind::parse(id).map(Open::Install),
            ("settings", "branches") => Some(Open::BranchSettings),
            ("feedback", "") => Some(Open::Feedback {
                error_event: None,
                agent: None,
                category: None,
            }),
            ("feedback", rest) => {
                let mut part = rest.splitn(3, ':');
                let mut next = || part.next().filter(|p| !p.is_empty()).map(str::to_string);
                Some(Open::Feedback {
                    error_event: next(),
                    agent: next(),
                    category: next(),
                })
            }
            #[cfg(feature = "team")]
            ("team", "") => Some(Open::Team),
            #[cfg(feature = "team")]
            ("url", url) if url.starts_with("https://") || url.starts_with("http://") => {
                Some(Open::Url(url.to_string()))
            }
            _ => None,
        }
    }
}

/// Sends `alert` when the island is connected; while it isn't, the link
/// would drop it, so it isn't sent and the caller tells it some other way.
/// The Inbox keeps the alert as it was; only what goes on screen is masked.
fn send_alert(
    tx: &std::sync::mpsc::Sender<ToIsland>,
    up: &AtomicBool,
    mut alert: isl::Alert,
) -> bool {
    alert.title = crate::redact::text(&alert.title).into_owned().into();
    alert.subtitle = crate::redact::text(&alert.subtitle).into_owned().into();
    up.load(Ordering::Relaxed) && tx.send(ToIsland::Alert { alert }).is_ok()
}

/// What the island is told when this app lets go of it: to quit, when the
/// setting was turned off while the app was linked to it. The host is a
/// process of its own with no window to close and no Dock icon, so otherwise
/// it would stay on screen. Not when the app never linked to it (a start with
/// the setting already off): that island was started by someone else.
fn farewell(linked: bool, enabled: bool) -> Option<ToIsland> {
    (linked && !enabled).then_some(ToIsland::Quit)
}

/// One request to the island, off this thread: it waits for the answer.
fn tell_once(msg: ToIsland) {
    #[cfg(any(unix, windows))]
    std::thread::spawn(move || {
        if let Err(e) = crate::island_host::proto::request(&msg) {
            eprintln!("arakun: island: {e}");
        }
    });
    #[cfg(not(any(unix, windows)))]
    let _ = msg;
}

/// `feed`, unless the island already shows it.
fn changed(sent: &mut Option<isl::Feed>, feed: isl::Feed) -> Option<isl::Feed> {
    if sent.as_ref() == Some(&feed) {
        return None;
    }
    *sent = Some(feed.clone());
    Some(feed)
}

impl Shell {
    /// Connects to the island to match Settings, starting it when none runs.
    pub(crate) fn sync_island(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let want = crate::island::enabled() && crate::island::supported(cx);
        if !want {
            if crate::island::enabled() {
                log_once(
                    "arakun: no Dynamic Island: it needs macOS, or Linux on Wayland with layer-shell",
                );
            }
            if let Some(msg) = farewell(self.island.is_some(), crate::island::enabled()) {
                tell_once(msg);
            }
            self.island = None;
            return;
        }
        if self.island.is_some() {
            return;
        }
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        {
            use futures::StreamExt as _;
            let (tx, mut presses, up) = crate::island_host::link::connect(true);
            let events = cx.spawn_in(window, async move |this, cx| {
                while let Some(ev) = presses.next().await {
                    let handled = this.update_in(cx, |this, window, cx| match ev {
                        FromIsland::Ver { key } | FromIsland::Row { key } => {
                            this.open_from_island(key, window, cx)
                        }
                        FromIsland::Luego { .. } => {}
                    });
                    if handled.is_err() {
                        break;
                    }
                }
            });
            let feed = cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(FEED_EVERY).await;
                    if this
                        .update_in(cx, |this, window, cx| this.feed_island(window, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            self.island = Some(IslandLink {
                tx,
                up,
                sent: None,
                _events: events,
                _feed: feed,
                seen: HashMap::new(),
            });
            self.feed_island(window, cx);
        }
    }

    /// "View", a row or a click on a desktop notification: the window in front, at the
    /// session or what the notice is about.
    pub(crate) fn open_from_island(
        &mut self,
        key: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::background::show(window);
        match Open::parse(&key) {
            Some(Open::Session(id)) => {
                let found = self.projects.iter().enumerate().find_map(|(pi, p)| {
                    p.environments
                        .iter()
                        .position(|e| e.session_index(&id).is_some())
                        .map(|ei| (pi, ei))
                });
                if let Some((pi, ei)) = found {
                    self.open_session_at(pi, ei, id, window, cx);
                }
            }
            Some(Open::Runs { root, run }) => {
                if self.show_project(&root, window, cx) {
                    self.open_runs_tab(run, window, cx);
                }
            }
            Some(Open::Branches { root }) => {
                if self.show_project(&root, window, cx) {
                    self.open_branches_tab(window, cx);
                }
            }
            Some(Open::GitHub(thread)) => self.open_github_notice(thread, window, cx),
            Some(Open::Pilot(id)) => self.open_pilot_notice(&id, window, cx),
            #[cfg(feature = "coderlabs")]
            Some(Open::Task(id)) => {
                self.open_tasks_tab(window, cx);
                self.tasks_view.update(cx, |v, cx| v.select_task(&id, cx));
            }
            Some(Open::Cloudflare) => self.open_cloudflare_tab(window, cx),
            Some(Open::Usage) => self.open_usage_tab(window, cx),
            Some(Open::Install(kind)) => cx.open_url(kind.install_url()),
            Some(Open::Feedback {
                error_event,
                agent,
                category,
            }) => self.open_error_feedback(error_event, agent, category, window, cx),
            Some(Open::BranchSettings) => {
                // An open dialog stays, with what was typed in it.
                if let Some((super::Modal::Settings(dialog), _)) = &self.modal {
                    dialog.update(cx, |d, cx| d.show_branches(cx));
                } else {
                    crate::settings_view::start_on_branches();
                    self.open_settings(window, cx);
                }
            }
            #[cfg(feature = "team")]
            Some(Open::Url(url)) => cx.open_url(&url),
            #[cfg(feature = "team")]
            Some(Open::Team) => self.open_team_tab(window, cx),
            None => {}
        }
    }

    /// The error form, with what links its answer to the error.
    pub(super) fn open_error_feedback(
        &mut self,
        error_event: Option<String>,
        agent: Option<String>,
        category: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use serde_json::Value;
        let extra = [
            ("error_event", error_event),
            ("agent", agent),
            ("category", category),
        ]
        .into_iter()
        .filter_map(|(k, v)| Some((k.to_string(), Value::from(v?))))
        .collect();
        let survey = crate::surveys::store::builtin(&crate::surveys::ids::ERROR);
        self.open_survey(survey, crate::telemetry::Source::Error, extra, window, cx);
    }

    /// The project at `root` in front, if it is still open.
    fn show_project(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(pi) = self.projects.iter().position(|p| p.path == root) else {
            return false;
        };
        if pi != self.active_project {
            self.select_project(pi, window, cx);
        }
        true
    }

    /// A notice that isn't about a session's turn (CI, a deploy, a limit, a
    /// task…): a card on the island when it is there, else a desktop
    /// notification. `open`: what the card's "View" shows; without it the card
    /// has no buttons and leaves by itself.
    pub(super) fn notify(
        &self,
        kind: AlertKind,
        title: &str,
        body: &str,
        open: Option<Open>,
        cx: &mut Context<Self>,
    ) {
        self.notify_from(kind, title, body, open, None, "", cx);
    }

    /// A notice with a picture (a file on disk) and the logo of the service it
    /// comes from (`github`), either of them optional.
    pub(super) fn notify_from(
        &self,
        kind: AlertKind,
        title: &str,
        body: &str,
        open: Option<Open>,
        image: Option<&std::path::Path>,
        brand: &str,
        cx: &mut Context<Self>,
    ) {
        self.notify_full(kind, title, body, "", open, image, brand, cx);
    }

    /// `notify_from` that also keeps `full` (the whole text, when `body` is
    /// only its start) in the Inbox.
    pub(super) fn notify_full(
        &self,
        kind: AlertKind,
        title: &str,
        body: &str,
        full: &str,
        open: Option<Open>,
        image: Option<&std::path::Path>,
        brand: &str,
        cx: &mut Context<Self>,
    ) {
        let key = open.map(|o| o.key()).unwrap_or_default();
        let alert = isl::Alert {
            key: key.clone(),
            kind,
            title: title.to_string().into(),
            subtitle: brief(body).into(),
            req: SharedString::default(),
            image: image
                .map(|p| p.display().to_string())
                .unwrap_or_default()
                .into(),
            brand: brand.to_string().into(),
        };
        if !self.show_alert_full(alert, full, cx) {
            super::sessions::send_notification(&key, title, body, kind == AlertKind::Failure, cx);
        }
    }

    /// A card already made, when the island is connected; `false` and the
    /// caller tells it some other way.
    #[cfg(feature = "team")]
    pub(super) fn island_card(&self, alert: isl::Alert) -> bool {
        self.island
            .as_ref()
            .is_some_and(|link| send_alert(&link.tx, &link.up, alert))
    }

    /// Keeps `alert` in the history, then shows it on the island when it is
    /// connected; `false` and the caller tells it some other way. Every notice
    /// the shell shows goes through here, so the Inbox misses none.
    pub(super) fn show_alert(&self, alert: isl::Alert, cx: &mut Context<Self>) -> bool {
        self.show_alert_full(alert, "", cx)
    }

    fn show_alert_full(&self, alert: isl::Alert, full: &str, cx: &mut Context<Self>) -> bool {
        self.record(&alert, full, cx);
        self.island
            .as_ref()
            .is_some_and(|link| send_alert(&link.tx, &link.up, alert))
    }

    fn record(&self, alert: &isl::Alert, full: &str, cx: &mut Context<Self>) {
        let mut notice = crate::notices::Notice::from_alert(alert, model::unix_now());
        notice.body = crate::notices::cap_body(full);
        let mut history = self.notices.borrow_mut();
        if history.push(notice) {
            crate::notices::store(&mut history);
            cx.notify();
        }
    }

    /// Hands the island what every session is up to, when that changed.
    pub(super) fn feed_island(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(mut link) = self.island.take() else {
            return;
        };
        let now = model::unix_now();
        let mut rows = Vec::new();
        for s in self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
        {
            if let Some(live) = &s.live {
                link.seen
                    .insert(s.id.clone(), seen(s, live.thread.read(cx)));
            }
            let recent = now.saturating_sub(s.updated) < RECENT_SECS;
            let status = match s.status {
                Status::Working => isl::Status::Working,
                Status::NeedsInput => isl::Status::Waiting,
                Status::Error if recent => isl::Status::Error,
                Status::Idle if recent && link.seen.contains_key(&s.id) => isl::Status::Done,
                _ => continue,
            };
            let shown = link.seen.get(&s.id).cloned().unwrap_or_else(|| Seen {
                model: s.agent.label().into(),
                doing: s.title.clone(),
                elapsed: SharedString::default(),
                cost: SharedString::default(),
                usd: 0.,
            });
            rows.push(isl::Row {
                key: s.id.clone(),
                status,
                model: shown.model,
                doing: shown.doing,
                elapsed: shown.elapsed,
                cost: shown.cost,
                updated: s.updated,
            });
        }
        isl::sort_rows(&mut rows);
        let today = crate::platform::local_time(now).map(|t| (t.year, t.month, t.day));
        let spent: f64 = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .filter(|s| {
                crate::platform::local_time(s.updated).map(|t| (t.year, t.month, t.day)) == today
            })
            .filter_map(|s| link.seen.get(&s.id))
            .map(|seen| seen.usd)
            .sum();
        let feed = isl::Feed {
            rows: rows
                .into_iter()
                .map(|mut r| {
                    r.doing = crate::redact::text(&r.doing).into_owned().into();
                    r
                })
                .collect(),
            today: (spent > 0.).then(|| crate::agent::fmt_cost(spent, "USD", false).into()),
            viewing: window
                .is_window_active()
                .then(|| self.active_session_id())
                .flatten(),
            reduce_motion: Some(cx.reduce_motion()),
        };
        if let Some(feed) = changed(&mut link.sent, feed) {
            let _ = link.tx.send(ToIsland::Feed { feed });
        }
        self.island = Some(link);
    }

    /// The card for a turn that ended, a question, or a failure.
    pub(super) fn session_alert(
        &self,
        id: &SharedString,
        thread: &Entity<Thread>,
        notice: Notice,
        title: &str,
        cx: &App,
    ) -> isl::Alert {
        let t = thread.read(cx);
        let model = model_label(t, None);
        let (kind, head, line) = match notice {
            Notice::Finished => {
                let reply = last_text(t.entries()).map(brief).filter(|b| !b.is_empty());
                (AlertKind::Finished, format!("{model} finished"), reply)
            }
            Notice::Input => (
                AlertKind::Input,
                format!("{model} needs your decision"),
                waiting_on(t, brief),
            ),
            Notice::Error => (
                AlertKind::Error,
                format!("{model} failed"),
                error_line(t, brief),
            ),
        };
        isl::Alert {
            key: id.clone(),
            kind,
            title: head.into(),
            subtitle: line.unwrap_or_else(|| one_line(title)).into(),
            req: SharedString::default(),
            image: SharedString::default(),
            brand: SharedString::default(),
        }
    }
}

fn log_once(message: &str) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("{message}");
    }
}

fn seen(s: &model::Session, t: &Thread) -> Seen {
    let elapsed = if t.is_busy() {
        t.turn_elapsed()
    } else {
        t.entries()
            .iter()
            .rev()
            .find_map(|e| match e {
                Entry::User { elapsed_ms, .. } => Some(*elapsed_ms),
                _ => None,
            })
            .flatten()
            .map(Duration::from_millis)
    };
    let doing = match t.status() {
        ThreadStatus::Error(_) => error_line(t, one_line),
        ThreadStatus::NeedsInput => waiting_on(t, one_line),
        _ if !t.is_busy() => last_reply(t.entries()).or_else(|| doing_now(t.entries())),
        _ => doing_now(t.entries()),
    };
    let usage = t.usage();
    let (cost, usd) = match &usage.cost {
        Some((amount, currency)) => (
            crate::agent::fmt_cost(*amount, currency, false),
            if currency.eq_ignore_ascii_case("USD") {
                *amount
            } else {
                0.
            },
        ),
        None => (String::new(), 0.),
    };
    Seen {
        model: model_label(t, Some(s)).into(),
        doing: doing.unwrap_or_else(|| one_line(&s.title)).into(),
        elapsed: elapsed.map(isl::fmt_elapsed).unwrap_or_default().into(),
        cost: cost.into(),
        usd,
    }
}

/// "GLM-5.3", "Opus 5.5": the model's own name, else the agent's.
fn model_label(t: &Thread, s: Option<&model::Session>) -> String {
    t.setting_label(Setting::Model)
        .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case("default"))
        .or_else(|| {
            s.and_then(|s| s.cli_model.as_deref())
                .map(|m| m.rsplit('/').next().unwrap_or(m).to_string())
        })
        .unwrap_or_else(|| t.kind().label().to_string())
}

/// What the agent is doing now: its latest tool call, else the latest of what
/// it thought or wrote, in the running (or last) turn.
fn doing_now(entries: &[Entry]) -> Option<String> {
    for e in entries.iter().rev() {
        let line = match e {
            Entry::User { .. } => return None,
            Entry::Tool(call) => one_line(&call.title),
            Entry::Agent { text } | Entry::Thought { text } => text
                .lines()
                .rev()
                .map(one_line)
                .find(|l| !l.is_empty())
                .unwrap_or_default(),
            _ => continue,
        };
        if !line.is_empty() {
            return Some(line);
        }
    }
    None
}

/// The turn's last reply, whole.
fn last_text(entries: &[Entry]) -> Option<&str> {
    entries
        .iter()
        .rev()
        .take_while(|e| !matches!(e, Entry::User { .. }))
        .find_map(|e| match e {
            Entry::Agent { text } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
}

/// The first line of the turn's last reply: what a finished turn's row says.
fn last_reply(entries: &[Entry]) -> Option<String> {
    last_text(entries).and_then(|text| text.lines().map(one_line).find(|l| !l.is_empty()))
}

/// What the agent waits on, as a row (`one_line`) or a card (`brief`) puts it.
fn waiting_on(t: &Thread, shape: fn(&str) -> String) -> Option<String> {
    if let Some(p) = t.pending_permission() {
        return Some(if p.plan.is_some() {
            "Has a plan ready to approve".into()
        } else {
            shape(&p.title)
        });
    }
    t.pending_question()
        .and_then(|q| q.questions.first())
        .map(|q| shape(&q.question))
}

fn error_line(t: &Thread, shape: fn(&str) -> String) -> Option<String> {
    match t.status() {
        ThreadStatus::Error(msg) => Some(shape(&msg)).filter(|l| !l.is_empty()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(v: serde_json::Value) -> Entry {
        serde_json::from_value(v).unwrap()
    }

    fn user() -> Entry {
        entry(serde_json::json!({"type": "user", "text": "go", "at": 0}))
    }

    fn tool(title: &str) -> Entry {
        entry(
            serde_json::json!({"type": "tool", "id": "1", "title": title, "kind": "execute", "status": "in_progress"}),
        )
    }

    fn agent(text: &str) -> Entry {
        entry(serde_json::json!({"type": "agent", "text": text}))
    }

    #[test]
    fn doing_now_is_the_latest_tool_or_text_of_the_turn() {
        let entries = [
            user(),
            agent("Voy a correr las pruebas."),
            tool("cargo test -p arakun"),
        ];
        assert_eq!(doing_now(&entries).as_deref(), Some("cargo test -p arakun"));
        let entries = [
            user(),
            tool("cargo test"),
            agent("Todo pasa.\n\n## Siguiente\n- revisar `rail.rs`\n"),
        ];
        assert_eq!(doing_now(&entries).as_deref(), Some("revisar rail.rs"));
        // Nothing yet in this turn: the last one's work does not count.
        let entries = [user(), tool("ls"), user()];
        assert_eq!(doing_now(&entries), None);
    }

    #[test]
    fn last_reply_is_its_first_line() {
        let entries = [
            user(),
            agent("**Listo**: qkv m32 −0.4 µs\nmás detalle"),
            tool("git diff"),
        ];
        assert_eq!(
            last_reply(&entries).as_deref(),
            Some("Listo: qkv m32 −0.4 µs")
        );
        assert_eq!(last_reply(&[agent("viejo"), user()]), None);
    }

    #[test]
    fn turning_the_island_off_tells_it_to_quit() {
        assert_eq!(farewell(true, false), Some(ToIsland::Quit));
        // Still on (the platform has no island), or never linked to one.
        assert_eq!(farewell(true, true), None);
        assert_eq!(farewell(false, false), None);
    }

    #[test]
    fn only_a_changed_feed_goes_out() {
        let row = |status| isl::Row {
            key: "s1".into(),
            status,
            model: "Opus 5.5".into(),
            doing: "cargo test".into(),
            elapsed: "0:58".into(),
            cost: SharedString::default(),
            updated: 1,
        };
        let feed = |status| isl::Feed {
            rows: vec![row(status)],
            today: None,
            viewing: None,
            reduce_motion: None,
        };
        let mut sent = None;
        assert!(changed(&mut sent, feed(isl::Status::Working)).is_some());
        assert!(changed(&mut sent, feed(isl::Status::Working)).is_none());
        assert!(changed(&mut sent, feed(isl::Status::Waiting)).is_some());
        let mut viewing = feed(isl::Status::Waiting);
        viewing.viewing = Some("s1".into());
        assert_eq!(changed(&mut sent, viewing.clone()), Some(viewing));
        // A new link starts with nothing sent.
        assert!(changed(&mut None, isl::Feed::default()).is_some());
    }

    #[test]
    fn a_finished_card_says_more_than_its_row() {
        let entries = [
            user(),
            agent(
                "Listo, el parche ya está.\nPasé `cargo test` y todo sale verde.\n\nOtro párrafo.",
            ),
            tool("git diff"),
        ];
        assert_eq!(
            last_reply(&entries).as_deref(),
            Some("Listo, el parche ya está.")
        );
        assert_eq!(
            last_text(&entries).map(brief).as_deref(),
            Some("Listo, el parche ya está. Pasé cargo test y todo sale verde. Otro párrafo.")
        );
        assert_eq!(last_text(&[user(), agent("  \n"), tool("ls")]), None);
    }

    #[test]
    fn notice_keys_never_pass_for_sessions() {
        let all = [
            Open::Session("6f1c2d".into()),
            Open::Runs {
                root: "/home/k/app".into(),
                run: None,
            },
            Open::Runs {
                root: "/home/k/my:app".into(),
                run: Some(1234567),
            },
            Open::Branches {
                root: "/home/k/app".into(),
            },
            Open::GitHub(None),
            Open::GitHub(Some("27219075342".into())),
            Open::Pilot("brief-95-40a1b2c".into()),
            #[cfg(feature = "coderlabs")]
            Open::Task("tsk_9".into()),
            Open::Cloudflare,
            Open::Usage,
            Open::Install(crate::agent::AgentKind::Droid),
            Open::Install(crate::agent::AgentKind::WorkersAi),
            Open::BranchSettings,
            Open::Feedback {
                error_event: None,
                agent: None,
                category: None,
            },
            Open::Feedback {
                error_event: Some("0190-ab".into()),
                agent: Some("claude".into()),
                category: Some("rate limited".into()),
            },
            Open::Feedback {
                error_event: None,
                agent: Some("droid".into()),
                category: None,
            },
            #[cfg(feature = "team")]
            Open::Url("https://example.com/x/y?v=2".into()),
            #[cfg(feature = "team")]
            Open::Team,
        ];
        for open in all {
            let key = open.key();
            assert_eq!(
                key.starts_with("open:"),
                !matches!(open, Open::Session(_)),
                "{key}"
            );
            assert_eq!(Open::parse(&key), Some(open));
        }
        assert_eq!(
            Open::Runs {
                root: "/r".into(),
                run: Some(7)
            }
            .key()
            .as_ref(),
            "open:run:7:/r"
        );
        // From a newer build, or broken: only the window comes up.
        for key in [
            "open:deploys",
            "open:run:x:/r",
            "open:run:7",
            "open:task:",
            "open:usage:more",
            "open:pilot:",
        ] {
            assert_eq!(Open::parse(key), None, "{key}");
        }
    }

    #[test]
    fn an_alert_waits_for_a_connected_island() {
        let alert = || isl::Alert {
            key: "open:usage".into(),
            kind: AlertKind::Warning,
            title: "Claude Code is at 80%".into(),
            subtitle: "5-hour window is filling up".into(),
            req: SharedString::default(),
            image: SharedString::default(),
            brand: SharedString::default(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let up = AtomicBool::new(false);
        // Reconnecting: the link would drop it, so the desktop gets it instead.
        assert!(!send_alert(&tx, &up, alert()));
        assert!(rx.try_recv().is_err());
        up.store(true, Ordering::Relaxed);
        assert!(send_alert(&tx, &up, alert()));
        assert_eq!(rx.try_recv().ok(), Some(ToIsland::Alert { alert: alert() }));
        // The link's thread is gone.
        drop(rx);
        assert!(!send_alert(&tx, &up, alert()));
    }

    #[test]
    fn one_line_trims_and_cuts() {
        assert_eq!(
            one_line("\n  #  Título   con   espacios \nresto"),
            "Título con espacios"
        );
        let long = "a".repeat(200);
        assert_eq!(one_line(&long).chars().count(), 121);
    }
}
