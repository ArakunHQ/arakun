//! The Computers tab: every computer this app runs sessions on, in one list.
//! This computer comes first, then the servers: machines running
//! `arakun server`, paired with the link one prints or signed in to the same
//! account, which join the list by themselves. The computer picked in the
//! list fills the side beside it: how it is reached, its environments and
//! their sessions, and its settings. A server's folders and sessions live in
//! the projects, as the app's own (`shell::servers`): a session picked here
//! opens there.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, px,
};
use serde_json::Value;

use crate::input::{InputEvent, TextInput};
use crate::model::Status;
use crate::servers::balance::{Balance, Preference};
use crate::theme;
use crate::ui::{self, ButtonKind, Tone, Type, Typography as _, icon};

pub enum ServersEvent {
    /// Ask the operator for access to cloud environments, with what for.
    CloudAsk(String),
    /// Make the account's cloud environment.
    CloudMake,
    /// Wake, stop or remove a cloud environment, by its label.
    Cloud {
        label: String,
        act: crate::cloud::CloudAct,
    },
    /// Pair with the server this link is for, copying the MCP sign-ins
    /// there or not.
    Pair { link: String, sign_ins: bool },
    /// Copy the MCP sign-ins to a server, or take them off it.
    SignIns { server: String, on: bool },
    /// Give up the pairing under way.
    CancelPairing,
    /// Remove a server: it is told to forget this app, and one on the
    /// account is taken off the account too.
    Remove(String),
    /// Makes a folder already on the server one sessions can start in.
    AddPlace { server: String, path: String },
    /// Clones a repository on the server, into a folder sessions can start in.
    Clone { server: String, url: String },
    /// Open one of a server's sessions in its project.
    Open { server: String, session: String },
    /// Open one of this computer's sessions, in the environment at `env`.
    OpenHere { env: PathBuf, session: String },
    /// Spread new environments across the computers, or not.
    Spread(bool),
    /// How much a server, or this computer for `None`, is wanted for new environments.
    Prefer {
        server: Option<String>,
        preference: Preference,
    },
    /// Sign in with GitHub, so the servers on the account show up.
    SignIn,
}

/// How the connection to a server stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Reach {
    Connecting,
    Connected,
    /// Not reachable for now, and why; it is tried again by itself.
    Away(String),
    /// The server will not have this app; pairing again is the way back.
    Refused(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub agent: String,
    pub status: Status,
    /// Unix seconds of its last activity.
    pub updated: u64,
    /// The folder it runs in, as its place's path. Empty from a server that
    /// does not say: `env`, the folder's name, is all there is then.
    pub place: String,
    pub env: String,
}

impl SessionRow {
    /// Whether the session runs in `place`.
    pub fn in_place(&self, place: &PlaceRow) -> bool {
        if self.place.is_empty() {
            !self.env.is_empty() && self.env == place.project
        } else {
            self.place == place.path
        }
    }
}

/// A folder on the server where a session can start.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceRow {
    pub path: String,
    pub project: String,
    pub branch: String,
    /// The key of the agent sessions start with there.
    pub agent: String,
    /// The repository's main folder there, which its worktrees share.
    pub repo: String,
    /// Its `origin`, by which the same project is found here. Empty when
    /// the server does not say (an older one) or it has none.
    pub origin: String,
}

/// How the app gets to a server, as the detail says it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Via {
    /// Its own socket, sealed end to end. `clear`: over plain http, and paired
    /// before the socket was sealed, so the server's key is not checked yet.
    Direct { clear: bool },
    /// Through the relay with the account's sign-in.
    Account,
    /// Paired through the relay by link, before accounts.
    OldRelay,
}

/// An agent signed in on a computer, from its `usage` message: its plan and
/// the limits that are running.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub name: String,
    pub plan: String,
    /// Each running window, how full it is in percent, and when it starts over.
    pub limits: Vec<(String, u64, Option<String>)>,
    /// What is wrong with its account, when something is.
    pub problem: Option<String>,
}

/// The agents signed in, from a `usage` message (`wire::plan_usage`).
pub fn agents_of(usage: &Value) -> Vec<AgentRow> {
    let Some(cards) = usage["agents"].as_array() else {
        return Vec::new();
    };
    cards
        .iter()
        .filter(|c| c["signed_in"] == true)
        .map(|c| AgentRow {
            name: c["name"].as_str().unwrap_or("").to_string(),
            plan: c["plan"].as_str().unwrap_or("").to_string(),
            limits: c["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| {
                    Some((
                        r["label"].as_str()?.to_string(),
                        r["percent"].as_u64()?,
                        r["detail"].as_str().map(str::to_string),
                    ))
                })
                .collect(),
            problem: (c["tone"] == "danger")
                .then(|| c["note"].as_str().map(str::to_string))
                .flatten(),
        })
        .filter(|a| !a.name.is_empty())
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerRow {
    pub id: String,
    pub name: String,
    pub origin: String,
    pub reach: Reach,
    pub sessions: Vec<SessionRow>,
    pub places: Vec<PlaceRow>,
    pub via: Via,
    /// Its label on the relay, when it is reached through it.
    pub label: Option<String>,
    /// Unix seconds it connected at, while it is connected.
    pub since: Option<u64>,
    /// Unix seconds it was last heard from, when that is known.
    pub seen: Option<u64>,
    /// Its agents, from its last `usage` message.
    pub agents: Vec<AgentRow>,
    /// This app copies its MCP sign-ins there.
    pub sign_ins: bool,
    /// The server last said it holds some this app copied.
    pub holds_sign_ins: bool,
}

/// A server signed in to the same account, as the account lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct MachineRow {
    pub label: String,
    pub name: String,
    pub online: bool,
    /// Unix seconds the relay last saw it.
    pub last_seen: u64,
}

/// The account's servers, as far as the account has said.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum YourServers {
    #[default]
    SignedOut,
    Loading,
    Listed(Vec<MachineRow>),
    /// The account could not be asked, and why.
    Failed(String),
}

/// The account's cloud environment, as far as the account has said.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum CloudState {
    /// Signed out, not asked yet, or not set up on this service.
    #[default]
    Hidden,
    /// No access yet: where the request stands.
    Ask(crate::cloud::CloudAccess),
    /// Access: the environment once made, and this month's hours.
    Ready {
        machine: Option<crate::cloud::CloudMachine>,
        usage: Option<crate::cloud::CloudUsage>,
    },
}

/// The Cloud section: the state, a call under way, and what went wrong last.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CloudRow {
    pub state: CloudState,
    pub busy: bool,
    pub error: Option<String>,
}

/// A length of time as the service says it: whole minutes under an hour,
/// else hours to a tenth.
pub fn hours(ms: u64) -> String {
    if ms < 3_600_000 {
        let m = (ms as f64 / 60_000.).round() as u64;
        return if m == 1 {
            "1 minute".into()
        } else {
            format!("{m} minutes")
        };
    }
    let h = (ms as f64 / 3_600_000. * 10.).round() / 10.;
    if h == 1. {
        "1 hour".into()
    } else {
        format!("{h} hours")
    }
}

/// What is left of this month's hours.
pub fn usage_line(u: &crate::cloud::CloudUsage) -> String {
    format!(
        "{} of {} left this month",
        hours(u.left_ms),
        hours(u.allowance_ms)
    )
}

/// Whether the account may ask for access now: never did, or was declined
/// long enough ago.
pub fn can_ask(a: &crate::cloud::CloudAccess, now: u64) -> bool {
    match a.status.as_str() {
        "none" => true,
        "declined" => a.retry_at.is_none_or(|t| t <= now),
        _ => false,
    }
}

/// Where the account's request for access stands.
pub fn access_line(a: &crate::cloud::CloudAccess, now: u64) -> String {
    match a.status.as_str() {
        "pending" => "You asked for access. We look at each request by hand and send a notice when it is decided.".into(),
        "granted" => "You have access.".into(),
        "declined" => match a.retry_at.filter(|t| *t > now) {
            Some(t) => {
                let days = (t - now).div_ceil(86_400);
                format!("Not this time. You can ask again in {}.", plural(days as usize, "day", "days"))
            }
            None => "Not last time, but you can ask again.".into(),
        },
        _ => "A cloud environment keeps your sessions working with your computer off. We give access by hand.".into(),
    }
}

/// What the environment's row says: awake, asleep, or why it stopped.
pub fn machine_line(
    m: &crate::cloud::CloudMachine,
    usage: Option<&crate::cloud::CloudUsage>,
) -> String {
    if m.running {
        return "Awake".into();
    }
    match m.stopped.as_deref() {
        None if m.snapshot => "Asleep. Opening it wakes it.".into(),
        None => "Starting. The first start takes a few minutes.".into(),
        Some("task_cap") => format!(
            "Stopped after {}, the most one task may run. Wake it to go on.",
            usage.map_or_else(
                || "the longest a task may run".into(),
                |u| hours(u.task_cap_ms)
            )
        ),
        Some(limit) => match usage.and_then(|u| u.message.clone()) {
            Some(message) => message,
            None if limit == "allowance_used" => "Stopped: this month's hours are used.".into(),
            None => "Stopped by Arakun for now.".into(),
        },
    }
}

/// Whether a wake can lift the stop: not one the operator or the month made.
pub fn can_wake(m: &crate::cloud::CloudMachine) -> bool {
    !m.running && matches!(m.stopped.as_deref(), None | Some("task_cap"))
}

/// An environment and its sessions, on any computer.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct EnvRow {
    pub project: String,
    pub branch: String,
    /// Where it is, as shown: `~` for the home folder.
    pub path: String,
    /// This computer's environment, which a session here opens in. `None`
    /// on a server, whose sessions open by their id.
    pub here: Option<PathBuf>,
    /// The latest sessions, newest first.
    pub sessions: Vec<SessionRow>,
    /// How many more there are in the project.
    pub more: usize,
}

/// What the app knows of the computer it runs on.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ThisComputer {
    pub name: String,
    pub envs: Vec<EnvRow>,
    pub agents: Vec<AgentRow>,
}

#[derive(Clone, PartialEq, Default)]
pub struct ServersState {
    pub servers: Vec<ServerRow>,
    /// The servers of the account this app is signed in to.
    pub yours: YourServers,
    /// The account's cloud environment, or the request for one.
    pub cloud: CloudRow,
    /// A pairing is under way.
    pub pairing: bool,
    /// Servers a folder is being added to or cloned on, until they are done.
    pub adding: HashSet<String>,
    /// What went wrong last, until something else happens.
    pub error: Option<String>,
    /// Where new environments go.
    pub balance: Balance,
}

/// Where a server's MCP sign-ins stand, under their switch.
pub fn sign_ins_note(server: &ServerRow) -> &'static str {
    let clear = server.via == (Via::Direct { clear: true });
    match (server.sign_ins && !clear, server.holds_sign_ins) {
        (true, true) => "It holds a copy. Turn this off to remove it.",
        (true, false) => "A copy goes there when it connects.",
        (false, true) => "It holds a copy, which goes when it next connects.",
        (false, false) if clear => {
            "Not over plain http: they only go to a server whose key is checked."
        }
        (false, false) => "Agents there sign in to your MCP servers themselves.",
    }
}

/// How a computer is doing, as a dot and a word. Only what is live gets a
/// color; a computer that is away is gray, whatever the reason.
pub fn presence(reach: &Reach) -> (Tone, &'static str) {
    match reach {
        Reach::Connected => (Tone::Success, "Online"),
        Reach::Connecting => (Tone::Neutral, "Connecting"),
        Reach::Away(_) => (Tone::Neutral, "Offline"),
        Reach::Refused(_) => (Tone::Neutral, "Not connected"),
    }
}

/// How many sessions are at work and how many wait on the user.
pub fn counts(sessions: &[SessionRow]) -> (usize, usize) {
    sessions.iter().fold((0, 0), |(w, n), s| match s.status {
        Status::Working => (w + 1, n),
        Status::NeedsInput => (w, n + 1),
        _ => (w, n),
    })
}

/// "3 working · 1 needs you", or nothing when nothing is going on.
pub fn activity(working: usize, needs: usize) -> Option<String> {
    let mut parts = Vec::new();
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if needs > 0 {
        parts.push(format!("{needs} needs you"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// "4m", "3h": how long ago, short.
fn short_age(t: u64, now: u64) -> String {
    crate::branch_history::short_ago(t, now)
}

/// "just now", "2h ago": when a computer was last seen.
pub fn ago(t: u64, now: u64) -> String {
    match short_age(t, now).as_str() {
        "now" => "just now".into(),
        age => format!("{age} ago"),
    }
}

/// The line under a server's name in the list: what its sessions are doing
/// while it is connected, and otherwise how it stands and when it was seen.
pub fn row_line(server: &ServerRow, now: u64) -> String {
    let (_, word) = presence(&server.reach);
    match &server.reach {
        Reach::Connected => {
            let (working, needs) = counts(&server.sessions);
            activity(working, needs).unwrap_or_else(|| word.to_string())
        }
        Reach::Away(_) | Reach::Refused(_) => match server.seen {
            Some(t) if t > 0 => format!("{word} · seen {}", ago(t, now)),
            _ => word.to_string(),
        },
        Reach::Connecting => word.to_string(),
    }
}

/// How a server is reached, and for how long, for the detail's header.
pub fn reached(server: &ServerRow, now: u64) -> String {
    let way = match server.via {
        Via::Account => "through your account, end-to-end encrypted",
        Via::Direct { clear: false } => "direct, encrypted",
        Via::Direct { clear: true } => "direct, key not checked yet",
        Via::OldRelay => "paired by link before accounts",
    };
    let mut line = format!("Your server · {way}");
    if let (Reach::Connected, Some(since)) = (&server.reach, server.since) {
        match short_age(since, now).as_str() {
            "now" => line.push_str(" · just connected"),
            age => line.push_str(&format!(" · connected for {age}")),
        }
    }
    line
}

/// The servers in the order the list shows them: by name, so a row stays
/// where it is when its server comes and goes.
pub fn ordered(servers: &[ServerRow]) -> Vec<&ServerRow> {
    let mut list: Vec<&ServerRow> = servers.iter().collect();
    list.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    list
}

/// How many sessions of an environment the detail shows.
const SESSIONS_SHOWN: usize = 4;

/// A server's folders as environments, each with its latest sessions.
pub fn server_envs(server: &ServerRow) -> Vec<EnvRow> {
    server
        .places
        .iter()
        .map(|place| {
            let mut sessions: Vec<SessionRow> = server
                .sessions
                .iter()
                .filter(|s| s.in_place(place))
                .cloned()
                .collect();
            sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
            let more = sessions.len().saturating_sub(SESSIONS_SHOWN);
            sessions.truncate(SESSIONS_SHOWN);
            EnvRow {
                project: place.project.clone(),
                branch: place.branch.clone(),
                path: tilde(&place.path),
                here: None,
                sessions,
                more,
            }
        })
        .collect()
}

/// `path` with a home folder (`/home/<user>`, `/Users/<user>`, `/root`)
/// written `~`, as a server's paths are shown.
pub fn tilde(path: &str) -> String {
    let rest = if let Some(r) = path.strip_prefix("/root") {
        Some(r)
    } else {
        ["/home/", "/Users/"].iter().find_map(|home| {
            let after = path.strip_prefix(home)?;
            let user_end = after.find('/').unwrap_or(after.len());
            (user_end > 0).then(|| &after[user_end..])
        })
    };
    match rest {
        Some(r) if r.is_empty() || r.starts_with('/') => format!("~{r}"),
        _ => path.to_string(),
    }
}

/// A relay label shortened for a row: its first and last four characters.
pub fn short_label(label: &str) -> String {
    if label.chars().count() <= 10 {
        return label.to_string();
    }
    let head: String = label.chars().take(4).collect();
    let tail: String = label.chars().skip(label.chars().count() - 4).collect();
    format!("{head}…{tail}")
}

/// A session's state as a word, and its tone: live states only take a color.
pub fn session_word(status: Status) -> (Tone, &'static str) {
    match status {
        Status::Working => (Tone::Info, "Working"),
        Status::NeedsInput => (Tone::Warning, "Needs you"),
        Status::Error => (Tone::Danger, "Failed"),
        Status::Idle => (Tone::Neutral, "Idle"),
    }
}

/// "4 environments in 2 projects".
fn projects_of(envs: &[EnvRow]) -> usize {
    envs.iter()
        .map(|e| e.project.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The computer picked in the list.
#[derive(Clone, Debug, PartialEq)]
enum Pick {
    This,
    Server(String),
}

/// What "Add a computer" shows.
#[derive(Clone, Copy, PartialEq)]
enum Sheet {
    /// A server of your own, signed in to your account.
    Account,
    /// A pairing link from a server reached directly.
    Link,
}

/// The list of computers, as wide as a sidebar.
const LIST_W: f32 = 300.;
/// As narrow as the list gets in a narrow pane, and as narrow as the detail
/// gets before the list gives way.
const LIST_MIN_W: f32 = 220.;
const DETAIL_MIN_W: f32 = 320.;
/// The column of the detail with the Computer card and the settings.
const SIDE_W: f32 = 320.;

/// What a server is installed with.
const INSTALL: &str = "curl -fsSL https://arakun.dev/install-server | sh";

pub struct ServersView {
    state: ServersState,
    this: ThisComputer,
    pick: Pick,
    /// "Add a computer" is open, on this way.
    sheet: Option<Sheet>,
    /// The form to add a folder is open on this server.
    adding_place: Option<String>,
    link: Entity<TextInput>,
    /// What to clone on a server, and a folder already there.
    clone_url: Entity<TextInput>,
    folder: Entity<TextInput>,
    /// The server what is in those two was typed for.
    draft_for: Option<String>,
    /// Remove was pressed once on this server; the next press counts.
    confirm_remove: Option<String>,
    /// What the account would use a cloud environment for, in its request.
    cloud_reason: Entity<TextInput>,
    /// Remove was pressed once on the cloud environment.
    confirm_cloud_remove: bool,
    /// The ⋯ menu is open on this server.
    menu: Option<String>,
    /// The pairing by link copies the MCP sign-ins.
    pair_sign_ins: bool,
    focus: FocusHandle,
    _inputs: Vec<Subscription>,
}

impl EventEmitter<ServersEvent> for ServersView {}

impl ServersView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| TextInput::new("https://…", cx).mono().redacted());
        let clone_url =
            cx.new(|cx| TextInput::new("https://github.com/owner/repo or owner/repo", cx));
        let folder = cx.new(|cx| TextInput::new("~/code/app", cx).mono());
        let cloud_reason = cx.new(|cx| TextInput::new("What would you use it for? (optional)", cx));
        let on_submit = |cx: &mut Context<Self>,
                         input: &Entity<TextInput>,
                         act: fn(&mut Self, &mut Context<Self>)| {
            cx.subscribe(input, move |this, _, ev: &InputEvent, cx| {
                if *ev == InputEvent::Submit {
                    act(this, cx);
                }
            })
        };
        let inputs = vec![
            on_submit(cx, &link, Self::pair),
            on_submit(cx, &clone_url, Self::clone_repo),
            on_submit(cx, &folder, Self::add_folder),
            on_submit(cx, &cloud_reason, Self::ask_cloud),
        ];
        Self {
            state: ServersState::default(),
            this: ThisComputer::default(),
            pick: Pick::This,
            sheet: None,
            adding_place: None,
            link,
            clone_url,
            folder,
            draft_for: None,
            confirm_remove: None,
            cloud_reason,
            confirm_cloud_remove: false,
            menu: None,
            pair_sign_ins: false,
            focus: cx.focus_handle(),
            _inputs: inputs,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub fn set_state(&mut self, state: ServersState, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        // A server that just joined, paired or through the account, has the
        // floor while "Add a computer" is open: it is what the user waited for.
        let new = state
            .servers
            .iter()
            .find(|s| !self.state.servers.iter().any(|o| o.id == s.id))
            .map(|s| s.id.clone());
        if let (Some(id), Some(_)) = (new, self.sheet) {
            self.link.update(cx, |i, cx| i.set_text("", cx));
            self.sheet = None;
            self.pick = Pick::Server(id);
        }
        let known = |id: &str| state.servers.iter().any(|s| s.id == id);
        if let Pick::Server(id) = &self.pick
            && !known(id)
        {
            self.pick = Pick::This;
        }
        if self.adding_place.as_deref().is_some_and(|id| !known(id)) {
            self.adding_place = None;
        }
        if self.menu.as_deref().is_some_and(|id| !known(id)) {
            self.menu = None;
        }
        self.state = state;
        cx.notify();
    }

    /// What the app knows of this computer now.
    pub fn set_this(&mut self, this: ThisComputer, cx: &mut Context<Self>) {
        if self.this != this {
            self.this = this;
            cx.notify();
        }
    }

    /// The form to clone a repository on `server`, or add one of its folders.
    pub fn add_place(&mut self, server: &str, window: &mut Window, cx: &mut Context<Self>) {
        // What was typed for another server is not meant for this one.
        if self.draft_for.as_deref() != Some(server) {
            self.clone_url.update(cx, |i, cx| i.set_text("", cx));
            self.folder.update(cx, |i, cx| i.set_text("", cx));
            self.draft_for = Some(server.to_string());
        }
        self.pick = Pick::Server(server.to_string());
        self.adding_place = Some(server.to_string());
        self.menu = None;
        window.focus(&self.clone_url.focus_handle(cx), cx);
        cx.notify();
    }

    /// The server added the folder it was asked for, which the shell opens.
    pub fn place_added(&mut self, server: &str, cx: &mut Context<Self>) {
        if self.adding_place.as_deref() != Some(server) {
            return;
        }
        self.clone_url.update(cx, |i, cx| i.set_text("", cx));
        self.folder.update(cx, |i, cx| i.set_text("", cx));
        self.adding_place = None;
        cx.notify();
    }

    fn server(&self, id: &str) -> Option<&ServerRow> {
        self.state.servers.iter().find(|s| s.id == id)
    }

    /// The server the add-folder form is for, when it can be asked now.
    fn adding_to(&self) -> Option<String> {
        let id = self.adding_place.as_ref()?;
        let server = self.server(id)?;
        (server.reach == Reach::Connected && !self.state.adding.contains(id)).then(|| id.clone())
    }

    fn clone_repo(&mut self, cx: &mut Context<Self>) {
        let url = self.clone_url.read(cx).text().trim().to_string();
        if let Some(server) = self.adding_to().filter(|_| !url.is_empty()) {
            cx.emit(ServersEvent::Clone { server, url });
        }
    }

    fn add_folder(&mut self, cx: &mut Context<Self>) {
        let path = self.folder.read(cx).text().trim().to_string();
        if let Some(server) = self.adding_to().filter(|_| !path.is_empty()) {
            cx.emit(ServersEvent::AddPlace { server, path });
        }
    }

    fn ask_cloud(&mut self, cx: &mut Context<Self>) {
        if self.state.cloud.busy {
            return;
        }
        let reason = self.cloud_reason.read(cx).text().trim().to_string();
        cx.emit(ServersEvent::CloudAsk(reason));
    }

    fn pair(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).text().trim().to_string();
        if !link.is_empty() && !self.state.pairing {
            cx.emit(ServersEvent::Pair {
                link,
                sign_ins: self.pair_sign_ins,
            });
        }
    }

    fn open_sheet(&mut self, sheet: Sheet, window: &mut Window, cx: &mut Context<Self>) {
        self.sheet = Some(sheet);
        self.menu = None;
        // Each pairing asks anew.
        self.pair_sign_ins = false;
        if sheet == Sheet::Link {
            window.focus(&self.link.focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn close_sheet(&mut self, cx: &mut Context<Self>) {
        if self.state.pairing {
            cx.emit(ServersEvent::CancelPairing);
        }
        self.sheet = None;
        cx.notify();
    }

    fn signed_in(&self) -> bool {
        self.state.yours != YourServers::SignedOut
    }

    // ---- the list ------------------------------------------------------------

    /// A section's name over its rows.
    fn section(name: &str) -> gpui::Div {
        ui::overline(name)
            .px(ui::sp(12.))
            .pt(ui::sp(12.))
            .pb(ui::sp(4.))
    }

    /// The square an icon of a computer sits in.
    fn computer_icon(name: &str, size: f32) -> gpui::Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(size))
            .rounded(ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .child(icon(name, size * 0.45).text_color(theme::text_muted()))
    }

    /// What a row says at its end: a mark while something waits on the
    /// user, otherwise how many sessions are at work.
    fn row_mark(working: usize, needs: usize) -> Option<AnyElement> {
        if needs > 0 {
            return Some(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(ui::sp(4.))
                    .ty(Type::Label)
                    .text_color(theme::needs_you())
                    .child(icon("alert-circle", 13.).text_color(theme::needs_you()))
                    .child(needs.to_string())
                    .into_any_element(),
            );
        }
        (working > 0).then(|| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(ui::sp(4.))
                .ty(Type::Label)
                .text_color(theme::text_muted())
                .child(ui::spinner(12., theme::working()))
                .child(working.to_string())
                .into_any_element()
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn list_row(
        &self,
        id: (&'static str, usize),
        pick: Pick,
        icon_name: &str,
        name: &str,
        dot: Tone,
        line: String,
        mark: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.pick == pick;
        div()
            .id(id)
            .map(|d| ui::labelled(d, gpui::Role::Button, format!("{name}, {line}")))
            .flex()
            .items_center()
            .gap(ui::sp(10.))
            .mx(ui::sp(8.))
            .px(ui::sp(8.))
            .py(ui::sp(6.))
            .rounded(ui::rad(ui::RADIUS))
            .cursor_pointer()
            .map(ui::focusable)
            .when(selected, |d| d.bg(theme::fill_selected()))
            .when(!selected, |d| d.hover(|s| s.bg(theme::fill_hover())))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.pick = pick.clone();
                this.menu = None;
                cx.notify();
            }))
            .child(Self::computer_icon(icon_name, 32.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .ty(Type::Body)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::content())
                            .child(crate::redact::text(name).into_owned()),
                    )
                    .child(
                        ui::status_dot(line, dot)
                            .ty(Type::Caption)
                            .min_w_0()
                            .truncate(),
                    ),
            )
            .children(mark)
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let here: Vec<SessionRow> = self
            .this
            .envs
            .iter()
            .flat_map(|e| e.sessions.iter().cloned())
            .collect();
        let (working, needs) = counts(&here);
        let this_line = match activity(working, needs) {
            Some(a) => format!("This computer · {a}"),
            None => "This computer".into(),
        };
        let this_row = self.list_row(
            ("computers-row-this", 0),
            Pick::This,
            "computer",
            &self.this.name,
            Tone::Success,
            this_line,
            Self::row_mark(working, needs),
            cx,
        );
        let servers: Vec<ServerRow> = ordered(&self.state.servers).into_iter().cloned().collect();
        let rows: Vec<AnyElement> = servers
            .iter()
            .enumerate()
            .map(|(ix, s)| {
                let (tone, _) = presence(&s.reach);
                let (working, needs) = if s.reach == Reach::Connected {
                    counts(&s.sessions)
                } else {
                    (0, 0)
                };
                self.list_row(
                    ("computers-row", ix),
                    Pick::Server(s.id.clone()),
                    "server",
                    &s.name,
                    tone,
                    row_line(s, now),
                    Self::row_mark(working, needs),
                    cx,
                )
            })
            .collect();
        let empty = servers.is_empty().then(|| {
            ui::text(
                Type::Caption,
                theme::text_faint(),
                if self.signed_in() {
                    "None yet. A server signed in to your account shows up here by itself."
                } else {
                    "None yet. Add one with its pairing link, or sign in."
                },
            )
            .px(ui::sp(16.))
            .py(ui::sp(4.))
        });
        let account = self.render_account_note(cx);
        let spread = self.state.balance.spread;
        let foot = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(12.))
            .px(ui::sp(16.))
            .py(ui::sp(12.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(ui::text(
                        Type::Label,
                        theme::text(),
                        "Spread new environments",
                    ))
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        "The least busy computer that has the project",
                    )),
            )
            .child(
                div()
                    .id("computers-spread")
                    .role(gpui::Role::Switch)
                    .aria_label("Spread new environments")
                    .aria_toggled(ui::toggled(spread))
                    .cursor_pointer()
                    .map(ui::focusable)
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(ServersEvent::Spread(!spread))),
                    )
                    .child(ui::switch(spread)),
            );
        div()
            .flex()
            .flex_col()
            .flex_shrink(1.)
            .w(px(LIST_W))
            .min_w(px(LIST_MIN_W))
            .h_full()
            .border_r_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .id("computers-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(ui::sp(2.))
                    .pb(ui::sp(12.))
                    .overflow_y_scroll()
                    .child(Self::section("This computer"))
                    .child(this_row)
                    .child(Self::section("Your servers"))
                    .children(rows)
                    .children(empty)
                    .children(account)
                    .children(self.render_cloud(cx)),
            )
            .child(foot)
            .into_any_element()
    }

    /// Under the servers: how the account stands, when that keeps some of
    /// them from showing.
    fn render_account_note(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let note = |text: String| {
            ui::text(Type::Caption, theme::text_faint(), text)
                .px(ui::sp(16.))
                .py(ui::sp(4.))
                .into_any_element()
        };
        match &self.state.yours {
            YourServers::SignedOut => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(8.))
                    .mx(ui::sp(8.))
                    .mt(ui::sp(8.))
                    .p(ui::sp(12.))
                    .rounded(ui::rad(ui::RADIUS))
                    .border_1()
                    .border_color(theme::border())
                    .child(ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        "The servers on your account show up here once you sign in.",
                    ))
                    .child(
                        ui::button_with(
                            "computers-sign-in",
                            Some("github"),
                            "Sign in with GitHub",
                            ButtonKind::Outline,
                            true,
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ServersEvent::SignIn))),
                    )
                    .into_any_element(),
            ),
            YourServers::Failed(why) => Some(note(format!(
                "Could not ask your account for its servers: {}",
                crate::redact::text(why)
            ))),
            YourServers::Loading | YourServers::Listed(_) => None,
        }
    }

    /// Under the servers: the account's cloud environment and its hours, or
    /// the request for access to one.
    fn render_cloud(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = &self.state.cloud;
        let busy = row.busy;
        let text = |t: String, color| ui::text(Type::Caption, color, t);
        let buttons = || div().flex().flex_wrap().gap(ui::sp(6.));
        let body: Vec<AnyElement> = match &row.state {
            CloudState::Hidden => return None,
            CloudState::Ask(access) => {
                let now = crate::model::unix_now();
                let mut body =
                    vec![text(access_line(access, now), theme::text_subtle()).into_any_element()];
                if can_ask(access, now) {
                    body.push(
                        ui::field()
                            .child(self.cloud_reason.clone())
                            .into_any_element(),
                    );
                    body.push(
                        buttons()
                            .child(
                                ui::button(
                                    "computers-cloud-ask",
                                    if busy { "Asking…" } else { "Ask for access" },
                                    ButtonKind::Outline,
                                    !busy,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.ask_cloud(cx))),
                            )
                            .into_any_element(),
                    );
                }
                body
            }
            CloudState::Ready {
                machine: None,
                usage,
            } => {
                let mut body = vec![
                    text(
                        "Runs Arakun in the cloud, so sessions keep working with your computer off."
                            .into(),
                        theme::text_subtle(),
                    )
                    .into_any_element(),
                    buttons()
                        .child(
                            ui::button(
                                "computers-cloud-make",
                                if busy { "Making…" } else { "New cloud environment" },
                                ButtonKind::Outline,
                                !busy,
                            )
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ServersEvent::CloudMake))),
                        )
                        .into_any_element(),
                ];
                body.extend(
                    usage
                        .as_ref()
                        .map(|u| text(usage_line(u), theme::text_faint()).into_any_element()),
                );
                body
            }
            CloudState::Ready {
                machine: Some(m),
                usage,
            } => {
                let label = m.label.clone();
                let act = |id: &'static str, name: &'static str, act: crate::cloud::CloudAct| {
                    let label = label.clone();
                    ui::button(id, name, ButtonKind::Outline, !busy).on_click(cx.listener(
                        move |_, _, _, cx| {
                            cx.emit(ServersEvent::Cloud {
                                label: label.clone(),
                                act,
                            })
                        },
                    ))
                };
                let remove = if self.confirm_cloud_remove {
                    let label = label.clone();
                    div()
                        .flex()
                        .gap(ui::sp(6.))
                        .child(
                            ui::button("computers-cloud-keep", "Keep", ButtonKind::Ghost, true)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_cloud_remove = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            ui::button(
                                "computers-cloud-remove-yes",
                                "Remove",
                                ButtonKind::Danger,
                                !busy,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.confirm_cloud_remove = false;
                                    cx.emit(ServersEvent::Cloud {
                                        label: label.clone(),
                                        act: crate::cloud::CloudAct::Remove,
                                    });
                                },
                            )),
                        )
                        .into_any_element()
                } else {
                    ui::button("computers-cloud-remove", "Remove", ButtonKind::Ghost, !busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_cloud_remove = true;
                            cx.notify();
                        }))
                        .into_any_element()
                };
                let mut body = vec![
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(
                            div()
                                .truncate()
                                .ty(Type::Body)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::content())
                                .child(crate::redact::text(&m.name).into_owned()),
                        )
                        .into_any_element(),
                    text(machine_line(m, usage.as_ref()), theme::text_subtle()).into_any_element(),
                ];
                if self.confirm_cloud_remove {
                    body.push(
                        text(
                            "Its files and sessions go with it.".into(),
                            theme::text_subtle(),
                        )
                        .into_any_element(),
                    );
                }
                body.push(
                    buttons()
                        .when(m.running, |d| {
                            d.child(act(
                                "computers-cloud-sleep",
                                "Stop",
                                crate::cloud::CloudAct::Sleep,
                            ))
                        })
                        .when(can_wake(m), |d| {
                            d.child(act(
                                "computers-cloud-wake",
                                "Wake",
                                crate::cloud::CloudAct::Wake,
                            ))
                        })
                        .child(remove)
                        .into_any_element(),
                );
                body.extend(
                    usage
                        .as_ref()
                        .map(|u| text(usage_line(u), theme::text_faint()).into_any_element()),
                );
                body
            }
        };
        Some(
            div()
                .flex()
                .flex_col()
                .child(Self::section("Cloud"))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(ui::sp(8.))
                        .mx(ui::sp(8.))
                        .p(ui::sp(12.))
                        .rounded(ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::border())
                        .children(body)
                        .when_some(row.error.clone(), |d, e| {
                            d.child(text(
                                crate::redact::text(&e).into_owned(),
                                theme::attention().into(),
                            ))
                        }),
                )
                .into_any_element(),
        )
    }

    // ---- the detail ------------------------------------------------------------

    fn header(
        &self,
        icon_name: &str,
        name: &str,
        status: (Tone, &str),
        line: String,
        actions: Option<AnyElement>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap(ui::sp(16.))
            .child(Self::computer_icon(icon_name, 44.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(200.))
                    .gap(ui::sp(4.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(10.))
                            .min_w_0()
                            .child(
                                ui::text(
                                    Type::Title,
                                    theme::content(),
                                    crate::redact::text(name).into_owned(),
                                )
                                .truncate(),
                            )
                            .child(ui::status_dot(status.1.to_string(), status.0)),
                    )
                    .child(ui::text(Type::Body, theme::text_subtle(), line)),
            )
            .children(actions)
    }

    /// Working now, Environments, Agents.
    fn stats(sessions: &[SessionRow], envs: &[EnvRow], agents: &[AgentRow]) -> AnyElement {
        let (working, needs) = counts(sessions);
        let cell = |el: gpui::Div, first: bool| {
            div()
                .flex_1()
                .min_w(px(150.))
                .px(ui::sp(16.))
                .py(ui::sp(12.))
                .when(!first, |d| {
                    d.border_l_1().border_color(theme::border_subtle())
                })
                .child(el)
        };
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        div()
            .flex()
            .flex_wrap()
            .rounded(ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border_subtle())
            .child(cell(
                ui::stat(
                    "Working now",
                    working.to_string(),
                    if needs > 0 {
                        format!("{needs} needs you")
                    } else {
                        "nothing waits on you".to_string()
                    },
                ),
                true,
            ))
            .child(cell(
                ui::stat(
                    "Environments",
                    envs.len().to_string(),
                    format!("in {}", plural(projects_of(envs), "project", "projects")),
                ),
                false,
            ))
            .child(cell(
                ui::stat(
                    "Agents",
                    agents.len().to_string(),
                    if names.is_empty() {
                        "none signed in".to_string()
                    } else {
                        names.join(", ")
                    },
                ),
                false,
            ))
            .into_any_element()
    }

    fn render_envs(
        &self,
        server: Option<&str>,
        envs: &[EnvRow],
        note: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body: Vec<AnyElement> = Vec::new();
        for (ei, env) in envs.iter().enumerate() {
            body.push(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .h(px(ui::MENU_ROW_H))
                    .px(ui::sp(12.))
                    .child(icon("git-branch", 13.).text_color(theme::text_faint()))
                    .child(
                        ui::text(Type::Body, theme::text(), env.project.clone())
                            .font_weight(FontWeight::MEDIUM)
                            .flex_none(),
                    )
                    .child(
                        ui::text(Type::Mono, theme::text_subtle(), env.branch.clone())
                            .min_w_0()
                            .truncate(),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::text(
                            Type::Mono,
                            theme::text_faint(),
                            crate::redact::text(&env.path).into_owned(),
                        )
                        .min_w_0()
                        .truncate(),
                    )
                    .into_any_element(),
            );
            for (si, s) in env.sessions.iter().enumerate() {
                let (tone, word) = session_word(s.status);
                let open = match (&env.here, server) {
                    (Some(path), _) => ServersEvent::OpenHere {
                        env: path.clone(),
                        session: s.id.clone(),
                    },
                    (None, Some(server)) => ServersEvent::Open {
                        server: server.to_string(),
                        session: s.id.clone(),
                    },
                    (None, None) => continue,
                };
                let open = std::rc::Rc::new(open);
                let meta = [
                    s.agent.as_str(),
                    &short_age(s.updated, crate::model::unix_now()),
                ]
                .iter()
                .filter(|p| !p.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" · ");
                body.push(
                    div()
                        .id(("computers-session", ei * 1000 + si))
                        .map(|d| ui::labelled(d, gpui::Role::Button, format!("Open {}", s.title)))
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .h(px(ui::MENU_ROW_H))
                        .pl(ui::sp(32.))
                        .pr(ui::sp(12.))
                        .cursor_pointer()
                        .map(ui::focusable)
                        .hover(|d| d.bg(theme::fill_hover()))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(match &*open {
                                ServersEvent::OpenHere { env, session } => ServersEvent::OpenHere {
                                    env: env.clone(),
                                    session: session.clone(),
                                },
                                ServersEvent::Open { server, session } => ServersEvent::Open {
                                    server: server.clone(),
                                    session: session.clone(),
                                },
                                _ => return,
                            })
                        }))
                        .child(
                            div()
                                .flex_none()
                                .size(px(6.))
                                .rounded_full()
                                .bg(tone.color()),
                        )
                        .child(
                            ui::text(Type::Body, theme::text(), s.title.clone())
                                .flex_1()
                                .min_w_0()
                                .truncate(),
                        )
                        .child(ui::text(Type::Caption, theme::text_faint(), meta).flex_none())
                        .child(
                            ui::text(
                                Type::Caption,
                                if tone == Tone::Neutral || tone == Tone::Info {
                                    theme::text_subtle()
                                } else {
                                    tone.color()
                                },
                                word,
                            )
                            .flex_none(),
                        )
                        .into_any_element(),
                );
            }
            if env.sessions.is_empty() {
                body.push(
                    ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        "No sessions. Start one here from the project.",
                    )
                    .pl(ui::sp(32.))
                    .pb(ui::sp(6.))
                    .into_any_element(),
                );
            } else if env.more > 0 {
                body.push(
                    ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        format!("{} more in the project", env.more),
                    )
                    .pl(ui::sp(32.))
                    .pb(ui::sp(6.))
                    .into_any_element(),
                );
            }
        }
        if envs.is_empty() {
            body.push(
                ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    if server.is_some() {
                        "No folders yet. Add a folder to start sessions there."
                    } else {
                        "No environments yet."
                    },
                )
                .p(ui::sp(12.))
                .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(10.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(ui::sp(8.))
                    .child(ui::heading("Environments"))
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        format!("{} · {note}", envs.len()),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .py(ui::sp(6.))
                    .rounded(ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .overflow_hidden()
                    .children(body),
            )
            .into_any_element()
    }

    /// A card under a heading, in the detail's side column.
    fn side_card(title: &str, body: impl IntoElement) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(10.))
            .child(ui::heading(title.to_string()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(ui::sp(16.))
                    .py(ui::sp(4.))
                    .rounded(ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .child(body),
            )
            .into_any_element()
    }

    /// The Computer card: what is known of it, and nothing that isn't.
    fn computer_card(rows: Vec<(&'static str, String)>, agents: &[AgentRow]) -> AnyElement {
        let mut lines: Vec<AnyElement> = rows
            .into_iter()
            .map(|(k, v)| ui::kv(k, v).into_any_element())
            .collect();
        for (i, a) in agents.iter().enumerate() {
            let plan = if a.plan.is_empty() {
                "Signed in".to_string()
            } else {
                a.plan.clone()
            };
            lines.push(
                div()
                    .id(("computer-agent", i))
                    .flex()
                    .flex_col()
                    .py(ui::sp(8.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .child(
                                ui::text(Type::Body, theme::text_muted(), a.name.clone())
                                    .flex_1()
                                    .min_w_0()
                                    .truncate(),
                            )
                            .child(
                                ui::text(Type::Body, theme::text(), plan)
                                    .min_w_0()
                                    .truncate(),
                            ),
                    )
                    .children(
                        a.limits.iter().map(|(label, pct, when)| {
                            ui::limit_line(label, *pct as f32, when.clone())
                        }),
                    )
                    .children(a.problem.as_ref().map(|p| {
                        ui::text(
                            Type::Caption,
                            theme::text_subtle(),
                            crate::redact::text(p).into_owned(),
                        )
                    }))
                    .into_any_element(),
            );
        }
        Self::side_card("Computer", div().flex().flex_col().children(lines))
    }

    /// New environments: how much this computer is wanted when they spread.
    fn preference_row(&self, server: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        let current = self.state.balance.of(server.as_deref());
        let choices = Preference::ALL.iter().enumerate().map(|(i, &p)| {
            let server = server.clone();
            ui::segment(("computers-prefer", i), None, p.label(), p == current).on_click(
                cx.listener(move |_, _, _, cx| {
                    cx.emit(ServersEvent::Prefer {
                        server: server.clone(),
                        preference: p,
                    });
                }),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(4.))
            .py(ui::sp(12.))
            .child(ui::text(Type::Label, theme::text(), "New environments"))
            .child(ui::text(
                Type::Caption,
                theme::text_subtle(),
                if self.state.balance.spread {
                    "When they spread, how much this one is wanted."
                } else {
                    "When they spread, how much this one is wanted. Spreading is off: a new environment is made on the open environment's computer."
                },
            ))
            .child(div().flex().pt(ui::sp(6.)).child(ui::segmented().children(choices)))
            .into_any_element()
    }

    /// The switch that copies the MCP sign-ins there, and where they stand.
    fn sign_ins_row(&self, server: &ServerRow, cx: &mut Context<Self>) -> AnyElement {
        let clear = server.via == (Via::Direct { clear: true });
        let on = server.sign_ins && !clear;
        let id = server.id.clone();
        let note = sign_ins_note(server);
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(4.))
            .py(ui::sp(12.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .child(
                ui::switch_row("computers-sign-ins", "Copy MCP sign-ins", on)
                    .px_0()
                    .when(!clear, |d| {
                        d.on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ServersEvent::SignIns {
                                server: id.clone(),
                                on: !on,
                            });
                        }))
                    }),
            )
            .when(!note.is_empty(), |d| {
                d.child(ui::text(Type::Caption, theme::text_subtle(), note))
            })
            .into_any_element()
    }

    fn remove_row(&self, server: &ServerRow, cx: &mut Context<Self>) -> AnyElement {
        let name = crate::redact::text(&server.name).into_owned();
        let asking = self.confirm_remove.as_deref() == Some(server.id.as_str());
        let what = if server.via == Via::Account {
            "It leaves this app and your account. Its sessions stay on it."
        } else {
            "It leaves this app. Its sessions stay on it."
        };
        let id = server.id.clone();
        let actions = if asking {
            div()
                .flex()
                .flex_none()
                .gap(ui::sp(6.))
                .child(
                    ui::button("computers-remove-keep", "Keep", ButtonKind::Ghost, true).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.confirm_remove = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    ui::button("computers-remove-yes", "Remove", ButtonKind::Danger, true)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_remove = None;
                            cx.emit(ServersEvent::Remove(id.clone()));
                        })),
                )
        } else {
            div().flex().flex_none().child(
                ui::button("computers-remove", "Remove", ButtonKind::Danger, true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.confirm_remove = Some(id.clone());
                        this.menu = None;
                        cx.notify();
                    }),
                ),
            )
        };
        div()
            .flex()
            .items_center()
            .gap(ui::sp(12.))
            .py(ui::sp(12.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(ui::sp(2.))
                    .child(ui::text(
                        Type::Label,
                        if asking {
                            theme::error().into()
                        } else {
                            theme::text()
                        },
                        if asking {
                            format!("Remove {name}?")
                        } else {
                            format!("Remove {name}")
                        },
                    ))
                    .child(ui::text(Type::Caption, theme::text_subtle(), what)),
            )
            .child(actions)
            .into_any_element()
    }

    /// Why a server is not connected, and what brings it back.
    fn away_notice(server: &ServerRow, now: u64) -> Option<AnyElement> {
        let name = crate::redact::text(&server.name).into_owned();
        let (title, why) = match &server.reach {
            Reach::Away(why) => (format!("{name} is not answering"), why.clone()),
            Reach::Refused(why) => (format!("{name} is not connected"), why.clone()),
            Reach::Connecting | Reach::Connected => return None,
        };
        let seen = server
            .seen
            .filter(|t| *t > 0)
            .map(|t| format!("Last seen {}.", ago(t, now)));
        let away = matches!(server.reach, Reach::Away(_));
        Some(
            div()
                .flex()
                .items_start()
                .gap(ui::sp(10.))
                .p(ui::sp(16.))
                .rounded(ui::rad(ui::RADIUS_LG))
                .border_1()
                .border_color(theme::border())
                .child(icon("alert-circle", 16.).text_color(theme::text_muted()))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(ui::sp(4.))
                        .child(ui::text(Type::Label, theme::text(), title))
                        .child(ui::text(
                            Type::Body,
                            theme::text_subtle(),
                            crate::redact::text(
                                [why.as_str(), seen.as_deref().unwrap_or("")]
                                    .join(" ")
                                    .trim(),
                            )
                            .into_owned(),
                        ))
                        .when(away, |d| {
                            d.child(ui::text(
                                Type::Body,
                                theme::text_subtle(),
                                "Arakun tries again by itself. If it was turned off, start it on that machine:",
                            ))
                            .child(div().flex().child(Self::command("computers-away-copy", "arakun-server serve")))
                        }),
                )
                .into_any_element(),
        )
    }

    /// A command to run, with a button that copies it.
    fn command(id: &'static str, text: &'static str) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .h(px(ui::INPUT_H))
            .pl(ui::sp(10.))
            .pr(ui::sp(4.))
            .rounded(ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::sunk())
            .child(ui::text(Type::Mono, theme::text_faint(), "$"))
            .child(
                ui::text(Type::Mono, theme::content(), text)
                    .flex_1()
                    .min_w_0()
                    .truncate(),
            )
            .child(
                ui::icon_button(id, "copy01", 13., "Copy").on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()))
                }),
            )
            .into_any_element()
    }

    fn render_this(&self, cx: &mut Context<Self>) -> AnyElement {
        let this = &self.this;
        let sessions: Vec<SessionRow> = this
            .envs
            .iter()
            .flat_map(|e| e.sessions.iter().cloned())
            .collect();
        let header = self.header(
            "computer",
            &this.name,
            (Tone::Success, "Online"),
            "This computer · where this app runs".into(),
            None,
        );
        let info =
            Self::computer_card(vec![("Reached", "This app runs here".into())], &this.agents);
        let settings = Self::side_card(
            "Settings",
            div().flex().flex_col().child(self.preference_row(None, cx)),
        );
        self.detail(
            header,
            None,
            Some(Self::stats(&sessions, &this.envs, &this.agents)),
            self.render_envs(None, &this.envs, "a session opens in its project", cx),
            vec![info, settings],
        )
    }

    fn render_server(&self, server: &ServerRow, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let connected = server.reach == Reach::Connected;
        let busy = self.state.adding.contains(&server.id);
        let id = server.id.clone();
        let more_id = server.id.clone();
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(6.))
            .when(connected, |d| {
                d.child(
                    ui::button_with(
                        "computers-add-folder",
                        Some("folder-add"),
                        if busy { "Adding…" } else { "Add a folder" },
                        ButtonKind::Outline,
                        !busy,
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.add_place(&id, window, cx)),
                    ),
                )
            })
            .child(div().relative().child(
                ui::icon_button("computers-more", "more-horizontal", 14., "More").on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.menu = match this.menu {
                            Some(_) => None,
                            None => Some(more_id.clone()),
                        };
                        cx.notify();
                    }),
                ),
            ))
            .into_any_element();
        let header = self.header(
            "server",
            &server.name,
            presence(&server.reach),
            reached(server, now),
            Some(actions),
        );
        let envs = server_envs(server);
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        rows.push((
            "Reached",
            match server.via {
                Via::Account => "Relay · your account".into(),
                Via::Direct { .. } => format!("Direct · {}", crate::redact::text(&server.origin)),
                Via::OldRelay => "Relay · pairing link".into(),
            },
        ));
        if let Some(label) = &server.label {
            rows.push(("Label", crate::redact::value(&short_label(label))));
        }
        if !connected && let Some(t) = server.seen.filter(|t| *t > 0) {
            rows.push(("Last seen", ago(t, now)));
        }
        if server.holds_sign_ins {
            rows.push(("MCP sign-ins", "A copy from here".into()));
        }
        let info = Self::computer_card(rows, &server.agents);
        let settings = Self::side_card(
            "Settings",
            div()
                .flex()
                .flex_col()
                .child(self.preference_row(Some(server.id.clone()), cx))
                .child(self.sign_ins_row(server, cx))
                .child(self.remove_row(server, cx)),
        );
        let form = (self.adding_place.as_deref() == Some(server.id.as_str()))
            .then(|| self.render_add_place(server, cx));
        let notice = Self::away_notice(server, now);
        let top = match (form, notice) {
            (Some(f), Some(n)) => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(12.))
                    .child(n)
                    .child(f)
                    .into_any_element(),
            ),
            (f, n) => f.or(n),
        };
        let note = if connected {
            "a session opens in its project"
        } else {
            "as it last listed them"
        };
        self.detail(
            header,
            top,
            connected.then(|| Self::stats(&server.sessions, &envs, &server.agents)),
            self.render_envs(Some(&server.id), &envs, note, cx),
            vec![info, settings],
        )
    }

    /// The detail's layout: the header, what needs saying first, the stats,
    /// then the environments beside the side column.
    fn detail(
        &self,
        header: gpui::Div,
        top: Option<AnyElement>,
        stats: Option<AnyElement>,
        envs: AnyElement,
        side: Vec<AnyElement>,
    ) -> AnyElement {
        div()
            .id("computers-detail")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(20.))
                    .p(ui::sp(ui::PAGE_PAD))
                    .max_w(px(1200.))
                    .child(header)
                    .children(top)
                    .children(stats)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_start()
                            .gap(ui::sp(24.))
                            .child(div().flex_1().min_w(px(280.)).child(envs))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(ui::sp(20.))
                                    .w(px(SIDE_W))
                                    .flex_none()
                                    .children(side),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The ⋯ menu of a server.
    fn render_menu(&self, server: &ServerRow, cx: &mut Context<Self>) -> AnyElement {
        let id = server.id.clone();
        // What there is to copy: the relay label, or the address it is dialed at.
        let copy = match (&server.label, server.via) {
            (Some(label), _) => Some(("Copy label", label.clone())),
            (None, Via::Direct { .. }) => Some(("Copy address", server.origin.clone())),
            (None, _) => None,
        };
        div()
            .absolute()
            .top(px(ui::PAGE_PAD + 40.))
            .right(px(ui::PAGE_PAD))
            .child(
                ui::menu_panel("computers-menu", 220.)
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        cx.notify();
                    }))
                    .when_some(copy, |d, (what, value)| {
                        d.child(
                            ui::menu_item("computers-copy", "copy01", what, false, true).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
                                    this.menu = None;
                                    cx.notify();
                                }),
                            ),
                        )
                    })
                    .child(
                        ui::menu_item(
                            "computers-menu-remove",
                            "delete02",
                            format!("Remove {}…", crate::redact::text(&server.name)),
                            true,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_remove = Some(id.clone());
                            this.menu = None;
                            cx.notify();
                        })),
                    ),
            )
            .into_any_element()
    }

    // ---- a folder to work in ------------------------------------------------

    fn render_add_place(&self, server: &ServerRow, cx: &mut Context<Self>) -> AnyElement {
        let connected = server.reach == Reach::Connected;
        let busy = self.state.adding.contains(&server.id);
        let ready = connected && !busy;
        let row = |field: Entity<TextInput>, icon_name: &'static str| {
            ui::field()
                .flex_1()
                .child(icon(icon_name, 14.).text_color(theme::text_subtle()))
                .child(field)
        };
        let part = |label: &'static str| {
            div().flex().flex_col().gap(ui::sp(8.)).child(ui::text(
                Type::Label,
                theme::text_subtle(),
                label,
            ))
        };
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(14.))
            .p(ui::sp(16.))
            .rounded(ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .gap(ui::sp(2.))
                            .child(ui::heading(format!(
                                "A folder on {}",
                                crate::redact::text(&server.name)
                            )))
                            .child(ui::text(
                                Type::Caption,
                                theme::text_subtle(),
                                "Sessions start in the folders a server offers. Clone a repository there, or add one it already has.",
                            )),
                    )
                    .child(
                        ui::button("computers-add-place-cancel", "Cancel", ButtonKind::Ghost, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.adding_place = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                part("Clone a repository, with that machine's git and its logins:").child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(row(self.clone_url.clone(), "git-branch"))
                        .child(
                            ui::button(
                                "computers-clone",
                                if busy { "Working…" } else { "Clone" },
                                ButtonKind::Primary,
                                ready,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.clone_repo(cx))),
                        ),
                ),
            )
            .child(
                part("Or add a folder already on it, as a whole path or from ~/:").child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .child(row(self.folder.clone(), "folder01"))
                        .child(
                            ui::button("computers-add-folder-go", "Add", ButtonKind::Outline, ready)
                                .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                        ),
                ),
            )
            .when(!connected, |d| {
                d.child(ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    "The server is not connected right now.",
                ))
            })
            .into_any_element()
    }

    // ---- adding a computer ---------------------------------------------------

    fn way_card(
        &self,
        id: &'static str,
        sheet: Sheet,
        icon_name: &str,
        title: &str,
        body: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on = self.sheet == Some(sheet);
        div()
            .id(id)
            .map(|d| ui::labelled(d, gpui::Role::RadioButton, title.to_string()))
            .aria_toggled(ui::toggled(on))
            .flex()
            .flex_col()
            .flex_1()
            .gap(ui::sp(6.))
            .p(ui::sp(12.))
            .rounded(ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(if on {
                theme::border_strong()
            } else {
                theme::border_subtle()
            })
            .when(on, |d| d.bg(theme::fill_selected()))
            .when(!on, |d| d.hover(|s| s.bg(theme::fill_hover())))
            .cursor_pointer()
            .map(ui::focusable)
            .on_click(cx.listener(move |this, _, window, cx| this.open_sheet(sheet, window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(Self::computer_icon(icon_name, 28.))
                    .child(div().flex_1())
                    .when(on, |d| {
                        d.child(icon("checkmark-circle02", 16.).text_color(theme::content()))
                    }),
            )
            .child(ui::text(Type::Label, theme::text(), title.to_string()))
            .child(ui::text(
                Type::Caption,
                theme::text_subtle(),
                body.to_string(),
            ))
            .into_any_element()
    }

    fn step(n: usize, title: &str, body: AnyElement) -> AnyElement {
        div()
            .flex()
            .items_start()
            .gap(ui::sp(10.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(20.))
                    .rounded_full()
                    .border_1()
                    .border_color(theme::border_strong())
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child(n.to_string()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(ui::sp(6.))
                    .child(ui::text(Type::Label, theme::text(), title.to_string()))
                    .child(body),
            )
            .into_any_element()
    }

    fn render_own_server(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = match &self.state.yours {
            YourServers::SignedOut => div()
                .flex()
                .flex_col()
                .gap(ui::sp(6.))
                .child(ui::text(
                    Type::Caption,
                    theme::text_subtle(),
                    "Sign in here with the same GitHub account first.",
                ))
                .child(
                    div().flex().child(
                        ui::button_with(
                            "computers-sheet-sign-in",
                            Some("github"),
                            "Sign in with GitHub",
                            ButtonKind::Outline,
                            true,
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ServersEvent::SignIn))),
                    ),
                )
                .into_any_element(),
            YourServers::Failed(why) => ui::text(
                Type::Caption,
                theme::text_subtle(),
                format!("Could not ask your account: {}", crate::redact::text(why)),
            )
            .into_any_element(),
            YourServers::Loading | YourServers::Listed(_) => div()
                .flex()
                .items_center()
                .gap(ui::sp(6.))
                .child(ui::spinner(12., theme::text_faint()))
                .child(ui::text(
                    Type::Caption,
                    theme::text_subtle(),
                    "Waiting for a new server on your account…",
                ))
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(14.))
            .p(ui::sp(14.))
            .rounded(ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border_subtle())
            .child(Self::step(
                1,
                "Install Arakun on the server",
                Self::command("computers-copy-install", INSTALL),
            ))
            .child(Self::step(
                2,
                "Sign it in with the same GitHub account",
                Self::command("computers-copy-login", "arakun-server login"),
            ))
            .child(Self::step(3, "It shows up here by itself", status))
            .child(ui::text(
                Type::Caption,
                theme::text_faint(),
                "Your MCP sign-ins are not copied to it. Turn that on in its settings.",
            ))
            .into_any_element()
    }

    fn render_by_link(&self, cx: &mut Context<Self>) -> AnyElement {
        let pairing = self.state.pairing;
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(14.))
            .p(ui::sp(14.))
            .rounded(ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border_subtle())
            .child(Self::step(
                1,
                "Start the server on that machine, and ask it for a link",
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(6.))
                    .child(Self::command("computers-copy-serve", "arakun-server serve"))
                    .child(Self::command("computers-copy-pair", "arakun-server pair"))
                    .into_any_element(),
            ))
            .child(Self::step(
                2,
                "Paste the link here. It is good for two minutes, and for one device",
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(
                        ui::field()
                            .flex_1()
                            .child(icon("link01", 14.).text_color(theme::text_subtle()))
                            .child(self.link.clone()),
                    )
                    .child(
                        ui::button(
                            "computers-pair",
                            if pairing { "Pairing…" } else { "Pair" },
                            ButtonKind::Primary,
                            !pairing,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.pair(cx))),
                    )
                    // Through the relay a pairing can wait on the
                    // server for as long as the link lasts.
                    .when(pairing, |d| {
                        d.child(
                            ui::button("computers-pair-cancel", "Cancel", ButtonKind::Ghost, true)
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(ServersEvent::CancelPairing)),
                                ),
                        )
                    })
                    .into_any_element(),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .child(
                        ui::switch_row(
                            "computers-pair-sign-ins",
                            "Copy my MCP sign-ins there",
                            self.pair_sign_ins,
                        )
                        .px_0()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pair_sign_ins = !this.pair_sign_ins;
                            cx.notify();
                        })),
                    )
                    .child(ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        "Agents there then use the MCP servers you signed in to here, with your tokens. You can change this in its settings.",
                    )),
            )
            .children(self.state.error.clone().map(|e| {
                ui::text(
                    Type::Label,
                    theme::attention().into(),
                    crate::redact::text(&e).into_owned(),
                )
            }))
            .child(ui::text(
                Type::Caption,
                theme::text_faint(),
                "A link that starts with http:// is not encrypted: use one only on a network you trust. Agents there use that machine's logins and run in its folders.",
            ))
            .into_any_element()
    }

    fn render_sheet(&self, sheet: Sheet, cx: &mut Context<Self>) -> AnyElement {
        let body = match sheet {
            Sheet::Account => self.render_own_server(cx),
            Sheet::Link => self.render_by_link(cx),
        };
        div()
            .id("computers-sheet-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(ui::sp(24.))
            .bg(gpui::black().opacity(0.4))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_sheet(cx)),
            )
            .child(
                ui::modal_panel("computers-sheet", 640., "Add a computer")
                    // A click inside does not reach the backdrop.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(ui::modal_header(
                        "Add a computer",
                        Some(
                            "Agents run there. You follow them from here and from your phone."
                                .into(),
                        ),
                        ui::close_button("computers-sheet-close")
                            .on_click(cx.listener(|this, _, _, cx| this.close_sheet(cx))),
                    ))
                    .child(
                        div()
                            .id("computers-sheet-body")
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .gap(ui::sp(12.))
                            .p(ui::sp(16.))
                            .overflow_y_scroll()
                            .child(
                                div()
                                    .flex()
                                    .gap(ui::sp(10.))
                                    .child(self.way_card(
                                        "computers-way-account",
                                        Sheet::Account,
                                        "server",
                                        "Your own server",
                                        "Sign it in with your GitHub account.",
                                        cx,
                                    ))
                                    .child(self.way_card(
                                        "computers-way-link",
                                        Sheet::Link,
                                        "link01",
                                        "By link",
                                        "For one on Tailscale or your network.",
                                        cx,
                                    )),
                            )
                            .child(body),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .px(ui::sp(16.))
                            .pb(ui::sp(14.))
                            .child(
                                ui::button("computers-sheet-done", "Done", ButtonKind::Ghost, true)
                                    .on_click(cx.listener(|this, _, _, cx| this.close_sheet(cx))),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ServersView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let online = 1 + self
            .state
            .servers
            .iter()
            .filter(|s| s.reach == Reach::Connected)
            .count();
        let total = 1 + self.state.servers.len();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(8.))
            .h(px(ui::TOOLBAR_H + 8.))
            .px(ui::sp(12.))
            .border_b_1()
            .border_color(theme::stroke())
            .child(icon("computer", 14.).text_color(theme::text_subtle()))
            .child(ui::text(Type::Label, theme::content(), "Computers"))
            .child(ui::text(
                Type::Label,
                theme::text_faint(),
                format!(
                    "{} · {online} online",
                    plural(total, "computer", "computers")
                ),
            ))
            .child(div().flex_1())
            .child(
                ui::button_with(
                    "computers-add",
                    Some("add01"),
                    "Add a computer",
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    let way = if this.signed_in() {
                        Sheet::Account
                    } else {
                        Sheet::Link
                    };
                    this.open_sheet(way, window, cx)
                })),
            );

        // Pairing says what went wrong in its own sheet.
        let error = self
            .state
            .error
            .clone()
            .filter(|_| self.sheet != Some(Sheet::Link))
            .map(|e| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(ui::sp(8.))
                    .px(ui::sp(12.))
                    .py(ui::sp(6.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(icon("alert-circle", 14.).text_color(theme::attention()))
                    .child(ui::text(
                        Type::Label,
                        theme::attention().into(),
                        crate::redact::text(&e).into_owned(),
                    ))
            });

        let picked = match &self.pick {
            Pick::Server(id) => self.server(id).cloned(),
            Pick::This => None,
        };
        let detail = match &picked {
            Some(server) => self.render_server(server, cx),
            None => self.render_this(cx),
        };
        let menu = picked
            .as_ref()
            .filter(|s| self.menu.as_deref() == Some(s.id.as_str()))
            .map(|s| self.render_menu(s, cx));
        let sheet = self.sheet.map(|s| self.render_sheet(s, cx));

        div()
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .children(error)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_list(cx))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w(px(DETAIL_MIN_W))
                            .min_h_0()
                            .child(detail)
                            .children(menu),
                    ),
            )
            .children(sheet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::{CloudAccess, CloudMachine, CloudUsage};
    use serde_json::json;

    fn cloud(running: bool, snapshot: bool, stopped: Option<&str>) -> CloudMachine {
        CloudMachine {
            label: "abcdefghijklmnop".into(),
            name: "Cloud".into(),
            running,
            snapshot,
            stopped: stopped.map(str::to_string),
        }
    }

    fn usage(message: Option<&str>) -> CloudUsage {
        CloudUsage {
            allowance_ms: 20 * 3_600_000,
            left_ms: 14 * 3_600_000 + 1_800_000,
            task_cap_ms: 2 * 3_600_000,
            stopped: None,
            message: message.map(str::to_string),
        }
    }

    #[test]
    fn cloud_hours_read_as_the_service_says_them() {
        assert_eq!(hours(60_000), "1 minute");
        assert_eq!(hours(45 * 60_000), "45 minutes");
        assert_eq!(hours(3_600_000), "1 hour");
        assert_eq!(hours(20 * 3_600_000), "20 hours");
        assert_eq!(
            usage_line(&usage(None)),
            "14.5 hours of 20 hours left this month"
        );
    }

    #[test]
    fn a_cloud_environment_says_why_it_stopped_and_offers_wake_only_when_it_helps() {
        assert_eq!(machine_line(&cloud(true, true, None), None), "Awake");
        assert!(machine_line(&cloud(false, false, None), None).starts_with("Starting"));
        assert!(machine_line(&cloud(false, true, None), None).starts_with("Asleep"));
        assert_eq!(
            machine_line(&cloud(false, true, Some("task_cap")), Some(&usage(None))),
            "Stopped after 2 hours, the most one task may run. Wake it to go on."
        );
        let said = "This account used its 20 hours of cloud time for this month.";
        assert_eq!(
            machine_line(
                &cloud(false, true, Some("allowance_used")),
                Some(&usage(Some(said)))
            ),
            said
        );
        assert!(can_wake(&cloud(false, true, Some("task_cap"))));
        assert!(can_wake(&cloud(false, false, None)));
        assert!(!can_wake(&cloud(true, true, None)));
        assert!(!can_wake(&cloud(false, true, Some("allowance_used"))));
        assert!(!can_wake(&cloud(false, true, Some("held"))));
    }

    #[test]
    fn the_cloud_request_says_where_it_stands() {
        let access = |status: &str, retry_at| CloudAccess {
            status: status.into(),
            requested_at: Some(1),
            decided_at: None,
            retry_at,
        };
        assert!(can_ask(&access("none", None), 100));
        assert!(!can_ask(&access("pending", None), 100));
        assert!(!can_ask(&access("declined", Some(200)), 100));
        assert!(can_ask(&access("declined", Some(100)), 100));
        assert_eq!(
            access_line(&access("declined", Some(100 + 2 * 86_400 - 5)), 100),
            "Not this time. You can ask again in 2 days."
        );
        assert!(access_line(&access("pending", None), 100).contains("by hand"));
    }

    fn session(id: &str, status: Status, updated: u64, place: &str) -> SessionRow {
        SessionRow {
            id: id.into(),
            title: format!("Session {id}"),
            agent: "Claude Code".into(),
            status,
            updated,
            place: place.into(),
            env: String::new(),
        }
    }

    fn server(id: &str, name: &str, reach: Reach) -> ServerRow {
        ServerRow {
            id: id.into(),
            name: name.into(),
            origin: "https://app.arakun.dev".into(),
            reach,
            sessions: Vec::new(),
            places: Vec::new(),
            via: Via::Account,
            label: Some("abcdefghij234567".into()),
            since: None,
            seen: None,
            agents: Vec::new(),
            sign_ins: false,
            holds_sign_ins: false,
        }
    }

    #[test]
    fn the_sign_ins_note_says_whether_a_copy_is_there() {
        let mut s = server("a", "box", Reach::Connected);
        assert!(sign_ins_note(&s).contains("themselves"));
        s.sign_ins = true;
        assert!(sign_ins_note(&s).contains("when it connects"));
        s.holds_sign_ins = true;
        assert!(sign_ins_note(&s).contains("Turn this off"));
        s.sign_ins = false;
        assert!(sign_ins_note(&s).contains("next connects"));
        // Over plain http nothing goes, whatever the setting says.
        s.holds_sign_ins = false;
        s.sign_ins = true;
        s.via = Via::Direct { clear: true };
        assert!(sign_ins_note(&s).contains("plain http"));
    }

    #[test]
    fn a_status_is_a_word_and_only_what_is_live_has_a_color() {
        assert_eq!(presence(&Reach::Connected), (Tone::Success, "Online"));
        assert_eq!(presence(&Reach::Connecting), (Tone::Neutral, "Connecting"));
        // Away is gray, never amber, whatever the reason.
        assert_eq!(
            presence(&Reach::Away(
                "The server is not at the relay right now.".into()
            )),
            (Tone::Neutral, "Offline")
        );
        assert_eq!(
            presence(&Reach::Refused("Sign in to reconnect.".into())),
            (Tone::Neutral, "Not connected")
        );
        assert_eq!(
            session_word(Status::NeedsInput),
            (Tone::Warning, "Needs you")
        );
        assert_eq!(session_word(Status::Working).1, "Working");
        assert_eq!(session_word(Status::Error), (Tone::Danger, "Failed"));
        assert_eq!(session_word(Status::Idle), (Tone::Neutral, "Idle"));
    }

    #[test]
    fn a_row_says_what_its_sessions_do_or_when_it_was_seen() {
        let now = 1_800_000_000;
        let mut s = server("a", "build-box", Reach::Connected);
        assert_eq!(row_line(&s, now), "Online");
        s.sessions = vec![
            session("1", Status::Working, now, ""),
            session("2", Status::Working, now, ""),
            session("3", Status::NeedsInput, now, ""),
            session("4", Status::Idle, now, ""),
        ];
        assert_eq!(counts(&s.sessions), (2, 1));
        assert_eq!(row_line(&s, now), "2 working · 1 needs you");
        assert_eq!(activity(0, 2).as_deref(), Some("2 needs you"));
        assert_eq!(activity(0, 0), None);
        // Away: what it last said is not live; when it was seen is what counts.
        s.reach = Reach::Away("http://10.0.0.2:7474 refused the connection".into());
        assert_eq!(row_line(&s, now), "Offline");
        s.seen = Some(now - 2 * 3600);
        assert_eq!(row_line(&s, now), "Offline · seen 2h ago");
        s.seen = Some(now - 5);
        assert_eq!(row_line(&s, now), "Offline · seen just now");
        assert_eq!(ago(now - 3 * 86_400, now), "3d ago");
        // The reason, which may hold an address, is never the row's line.
        assert!(!row_line(&s, now).contains("10.0.0.2"));
        s.reach = Reach::Connecting;
        assert_eq!(row_line(&s, now), "Connecting");
    }

    #[test]
    fn the_header_says_how_a_server_is_reached_and_for_how_long() {
        let now = 1_800_000_000;
        let mut s = server("a", "build-box", Reach::Connected);
        s.since = Some(now - 3 * 3600);
        assert_eq!(
            reached(&s, now),
            "Your server · through your account, end-to-end encrypted · connected for 3h"
        );
        s.via = Via::Direct { clear: true };
        s.reach = Reach::Away("gone".into());
        assert_eq!(
            reached(&s, now),
            "Your server · direct, key not checked yet"
        );
        s.via = Via::Direct { clear: false };
        assert_eq!(reached(&s, now), "Your server · direct, encrypted");
    }

    #[test]
    fn servers_are_listed_by_name_whatever_their_state() {
        let list = vec![
            server("3", "zeta", Reach::Connected),
            server("1", "attic-mini", Reach::Away("x".into())),
            server("2", "Build-box", Reach::Connected),
            server("0", "attic-mini", Reach::Connected),
        ];
        let names: Vec<(&str, &str)> = ordered(&list)
            .iter()
            .map(|s| (s.name.as_str(), s.id.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("attic-mini", "0"),
                ("attic-mini", "1"),
                ("Build-box", "2"),
                ("zeta", "3")
            ]
        );
    }

    #[test]
    fn a_servers_folders_are_its_environments_with_their_latest_sessions() {
        let mut s = server("a", "build-box", Reach::Connected);
        s.places = vec![
            PlaceRow {
                path: "/home/karlo/code/arakun".into(),
                project: "arakun".into(),
                branch: "main".into(),
                agent: "claude".into(),
                repo: "/home/karlo/code/arakun".into(),
                origin: String::new(),
            },
            PlaceRow {
                path: "/srv/habitae".into(),
                project: "habitae".into(),
                branch: "main".into(),
                agent: "claude".into(),
                repo: "/srv/habitae".into(),
                origin: String::new(),
            },
        ];
        s.sessions = (0..6)
            .map(|i| session(&i.to_string(), Status::Idle, i, "/home/karlo/code/arakun"))
            .collect();
        let envs = server_envs(&s);
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].path, "~/code/arakun");
        assert_eq!(envs[0].sessions.len(), SESSIONS_SHOWN);
        assert_eq!(envs[0].sessions[0].id, "5", "newest first");
        assert_eq!(envs[0].more, 2);
        assert!(envs[1].sessions.is_empty());
        assert_eq!(envs[1].path, "/srv/habitae");
        assert_eq!(projects_of(&envs), 2);
    }

    #[test]
    fn a_home_folder_is_written_as_a_tilde() {
        assert_eq!(tilde("/home/karlo/code/app"), "~/code/app");
        assert_eq!(tilde("/Users/k"), "~");
        assert_eq!(tilde("/root/x"), "~/x");
        assert_eq!(tilde("/rootless/x"), "/rootless/x");
        assert_eq!(tilde("/home/"), "/home/");
        assert_eq!(tilde("/srv/app"), "/srv/app");
        assert_eq!(short_label("abcdefghij234567"), "abcd…4567");
        assert_eq!(short_label("short"), "short");
    }

    #[test]
    fn the_agents_are_the_ones_signed_in_with_their_running_limits() {
        let usage = json!({
            "type": "usage",
            "agents": [
                { "agent": "claude", "name": "Claude Code", "signed_in": true, "plan": "Max",
                  "rows": [{ "label": "5h", "percent": 18, "detail": "in 3h" },
                           { "label": "7d", "percent": 42, "detail": null }] },
                { "agent": "droid", "name": "Droid", "signed_in": false, "note": "Sign in" },
            ],
        });
        assert_eq!(
            agents_of(&usage),
            [AgentRow {
                name: "Claude Code".into(),
                plan: "Max".into(),
                limits: vec![
                    ("5h".into(), 18, Some("in 3h".into())),
                    ("7d".into(), 42, None)
                ],
                problem: None,
            }]
        );
        assert!(agents_of(&json!({ "type": "usage" })).is_empty());
        // Signed in, but its account can't be read: it says why.
        let broken = json!({ "agents": [
            { "name": "Droid", "signed_in": true, "plan": "", "rows": [], "tone": "danger",
              "note": "Droid's login was rejected." },
        ] });
        assert_eq!(
            agents_of(&broken)[0].problem.as_deref(),
            Some("Droid's login was rejected.")
        );
    }
}
