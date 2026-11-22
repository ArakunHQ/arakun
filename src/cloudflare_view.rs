//! The Cloudflare tab: the signed-in account's problems, domains (traffic, DNS,
//! SSL), Tunnels, Workers and Pages, storage and activity (`cf` CLI sign-in,
//! read-only), with ways to hand a problem to the agent picked for new
//! sessions, open the dashboard, connect Cloudflare's MCP server to that agent
//! and share a local server through a Quick Tunnel only some emails can open.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Div, Entity, EventEmitter, FocusHandle,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, canvas, div, px, relative,
};

use crate::agent::AgentKind;
use crate::cloudflare::{
    self, Ask, Cert, Day, DnsRecord, Insight, Issue, LoadError, Mcp, Snapshot, Subject, Tunnel,
    Worker, Zone, bytes, count,
};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind, icon, icon_button};

/// Opening the tab reloads data older than this; the GraphQL API has a tight budget.
mod overview;

/// Under this width the page keeps less room at its sides.
const NARROW_UNDER: f32 = 760.;
/// The pages never get wider than this.
const PAGE_MAX: f32 = 1320.;

/// How the tab lays itself out at a width.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    /// The page is narrow: less room at its sides.
    narrow: bool,
    /// The page's padding at the sides.
    pad: f32,
    /// The width the page's content gets.
    content: f32,
}

/// The page's padding at the sides, before the Spacing setting scales it.
fn page_pad(narrow: bool) -> f32 {
    if narrow { 16. } else { 28. }
}

fn layout(w: f32) -> Layout {
    let narrow = w < NARROW_UNDER;
    let pad = f32::from(crate::ui::sp(page_pad(narrow)));
    Layout {
        narrow,
        pad,
        content: (w.min(PAGE_MAX) - 2. * pad).max(0.),
    }
}

/// Days of traffic the Overview charts (what `cloudflare` reads).
const DAYS: u64 = 7;
const STALE: u64 = 5 * 60;
/// While Arakun is open the account is read again this often, so the tab is
/// never opened cold; the first time a little after launch.
const WARM: Duration = Duration::from_secs(5 * 60);
const WARM_FIRST: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_secs(3);
/// How long Cloudflare's GraphQL budget takes to come back after running out.
const RATE_LIMIT_RESET: Duration = Duration::from_secs(5 * 60 + 15);

pub enum CloudflareEvent {
    /// Open a dashboard page in a browser tab.
    Open(String),
    /// Start a session of the agent picked for new sessions, in the project
    /// that deploys `ask.workers` when there is one.
    Ask(Ask),
    /// Add Cloudflare's MCP server to the agent (if missing) and sign in to it.
    ConnectMcp(AgentKind, Mcp),
    /// Share `localhost:port` with a Quick Tunnel only `emails` can open.
    Share { port: u16, emails: Vec<String> },
    /// Run `cf auth login` (installing `cf` first when `install`).
    SignIn { install: bool },
    /// Run a command in a terminal (granting the sign-in more read access).
    Run(String),
    /// Something broke since the last look: a desktop notification.
    Notify {
        title: String,
        body: String,
        critical: bool,
    },
    /// Show the project (on this machine) that deploys a Worker.
    OpenProject(PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Summary,
    Domains,
    Tunnels,
    Workers,
    /// A Worker's requests as they happen (`wrangler tail`).
    Logs,
    Containers,
    Storage,
    Security,
    Email,
    Activity,
    Tools,
}

impl Page {
    const ALL: [Page; 11] = [
        Page::Summary,
        Page::Domains,
        Page::Tunnels,
        Page::Workers,
        Page::Logs,
        Page::Containers,
        Page::Storage,
        Page::Security,
        Page::Email,
        Page::Activity,
        Page::Tools,
    ];

    /// What the sidebar gets back when the page is picked.
    fn key(self) -> &'static str {
        match self {
            Page::Summary => "summary",
            Page::Domains => "domains",
            Page::Tunnels => "tunnels",
            Page::Workers => "workers",
            Page::Logs => "logs",
            Page::Containers => "containers",
            Page::Storage => "storage",
            Page::Security => "security",
            Page::Email => "email",
            Page::Activity => "activity",
            Page::Tools => "tools",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Page::Summary => "Overview",
            Page::Logs => "Live logs",
            Page::Containers => "Containers",
            Page::Security => "Security",
            Page::Email => "Email",
            Page::Domains => "Domains",
            Page::Tunnels => "Tunnels",
            Page::Workers => "Workers & Pages",
            Page::Storage => "Storage & AI",
            Page::Activity => "Activity",
            Page::Tools => "Tools",
        }
    }
}

pub struct CloudflareView {
    page: Page,
    snap: Option<Snapshot>,
    error: Option<LoadError>,
    loading: bool,
    /// The quick answers are in; the analytics still on their way.
    analytics_pending: bool,
    /// The projects on this machine; their wrangler configs name D1 databases.
    roots: Vec<PathBuf>,
    /// The agent new sessions start with: it gets Investigate and the MCP server.
    agent: AgentKind,
    /// Zones whose full DNS list is open.
    dns_open: HashSet<String>,
    /// Kinds of issue whose group is open on the Overview.
    groups_open: HashSet<&'static str>,
    /// The fix whose confirmation is showing, the one being applied, and those done (by `Fix::key`).
    fix_confirm: Option<String>,
    fixing: Option<String>,
    fixed: HashSet<String>,
    fix_error: Option<(String, String)>,
    /// Problems already told about (or there before), so only new ones notify.
    alerted: Option<HashSet<String>>,
    /// The Worker being tailed, its newest events first, and what went wrong starting it.
    tail: Option<(String, cloudflare::tail::Tail)>,
    tail_events: std::collections::VecDeque<cloudflare::tail::TailEvent>,
    tail_errors_only: bool,
    tail_error: Option<String>,
    _tail_task: Option<Task<()>>,
    /// Zones whose traffic detail is open, and what was read for each.
    traffic_open: HashSet<String>,
    /// Worker → the project here whose wrangler config deploys it.
    homes: std::collections::HashMap<String, PathBuf>,
    traffic: std::collections::HashMap<String, Result<cloudflare::ZoneTraffic, String>>,
    chart: Entity<ui::AreaChart>,
    /// The width this view got last frame; it decides the layout of this one.
    width: Rc<Cell<f32>>,
    mcp: Mcp,
    /// Until when (unix seconds) to keep asking the agent about the MCP sign-in.
    mcp_watch: u64,
    mcp_task: Option<Task<()>>,
    cloudflared: bool,
    port: Entity<TextInput>,
    emails: Entity<TextInput>,
    share_note: Option<(SharedString, bool)>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _subs: [Subscription; 2],
    _poll: Task<()>,
    _load: Option<Task<()>>,
    /// A reload waiting for Cloudflare's analytics rate limit to reset.
    _retry: Option<Task<()>>,
    _warm: Task<()>,
}

impl EventEmitter<CloudflareEvent> for CloudflareView {}

impl CloudflareView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let port = cx.new(|cx| TextInput::new("3000", cx));
        let emails = cx.new(|cx| TextInput::new("you@example.com, *@team.dev", cx).redacted());
        let on_input = |this: &mut Self,
                        _: Entity<TextInput>,
                        ev: &InputEvent,
                        cx: &mut Context<Self>| match ev {
            InputEvent::Submit => this.share(cx),
            InputEvent::Changed => {
                this.share_note = None;
                cx.notify();
            }
            _ => {}
        };
        let subs = [
            cx.subscribe(&port, on_input),
            cx.subscribe(&emails, on_input),
        ];
        let poll = cx.spawn(async move |this, cx| {
            let mut tick = 0u32;
            loop {
                cx.background_executor().timer(POLL).await;
                tick += 1;
                let Ok(agent) = this.read_with(cx, |this, _| this.agent) else {
                    break;
                };
                let configured = cx
                    .background_spawn(crate::guard::task(async move {
                        cloudflare::mcp_server(agent).is_some()
                    }))
                    .await;
                let alive = this.update(cx, |this, cx| {
                    let watching =
                        this.mcp_watch > crate::model::unix_now() && this.mcp != Mcp::Connected;
                    // Without its CLI the agent isn't asked anything, so the
                    // check is cheap; it notices when one gets installed.
                    if this.mcp == Mcp::NoCli
                        || configured != (this.mcp != Mcp::Missing)
                        || (watching && tick.is_multiple_of(2))
                    {
                        this.check_mcp(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        // Signed in before: keep the account warm in the background.
        let warm = crate::poll::poll_every(cx, WARM_FIRST, WARM, |this, cx| {
            if this.snap.is_some() {
                this.refresh_if_stale(cx);
            }
        });
        Self {
            page: Page::Summary,
            // What the last load found shows at once; a new one follows.
            snap: cloudflare::cached(),
            error: None,
            loading: false,
            analytics_pending: false,
            roots: Vec::new(),
            agent: AgentKind::Claude,
            dns_open: HashSet::new(),
            groups_open: HashSet::new(),
            fix_confirm: None,
            fixing: None,
            fixed: HashSet::new(),
            fix_error: None,
            alerted: cloudflare::cached().map(|s| alerts(&s).into_iter().map(|a| a.0).collect()),
            tail: None,
            tail_events: Default::default(),
            tail_errors_only: false,
            tail_error: None,
            _tail_task: None,
            traffic_open: HashSet::new(),
            homes: Default::default(),
            traffic: Default::default(),
            chart: cx.new(|_| overview::chart()),
            // Wide until measured, as most windows are.
            width: Rc::new(Cell::new(1200.)),
            mcp: cloudflare::mcp_configured(AgentKind::Claude),
            mcp_watch: 0,
            mcp_task: None,
            cloudflared: crate::agent::which("cloudflared").is_some(),
            port,
            emails,
            share_note: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _subs: subs,
            _poll: poll,
            _load: None,
            _retry: None,
            _warm: warm,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub fn set_roots(&mut self, roots: Vec<PathBuf>) {
        if self.roots != roots {
            // Which project deploys each Worker, from their wrangler configs.
            self.homes.clear();
            for root in &roots {
                for name in cloudflare::workers_in(root) {
                    self.homes.entry(name).or_insert_with(|| root.clone());
                }
            }
        }
        self.roots = roots;
    }

    /// Follows the agent picked for new sessions, and asks it about the MCP server.
    pub fn set_agent(&mut self, agent: AgentKind, cx: &mut Context<Self>) {
        if self.agent != agent {
            self.agent = agent;
            self.mcp = cloudflare::mcp_configured(agent);
            self.mcp_task = None;
            self.check_mcp(cx);
            cx.notify();
        }
    }

    /// Whether sessions of the agent can use Cloudflare's MCP server.
    pub fn mcp(&self) -> bool {
        self.mcp == Mcp::Connected
    }

    /// Keeps asking the agent for a while, so a sign-in finished in the terminal shows up.
    pub fn watch_mcp(&mut self) {
        self.mcp_watch = crate::model::unix_now() + 180;
    }

    fn check_mcp(&mut self, cx: &mut Context<Self>) {
        if self.mcp_task.is_some() {
            return;
        }
        let agent = self.agent;
        self.mcp_task = Some(cx.spawn(async move |this, cx| {
            let state = cx
                .background_spawn(crate::guard::task_or(
                    async move { cloudflare::mcp_status(agent) },
                    |panicked| cloudflare::Mcp::Failing(panicked.to_string()),
                ))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.mcp_task = None;
                if this.agent == agent && this.mcp != state {
                    this.mcp = state;
                    cx.notify();
                }
            });
        }));
    }

    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        let fresh = self
            .snap
            .as_ref()
            .is_some_and(|s| crate::model::unix_now().saturating_sub(s.fetched) < STALE);
        if !fresh && self.error.is_none() {
            self.refresh(cx);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.check_mcp(cx);
        cx.notify();
        let roots = self.roots.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            // The quick answers arrive first (through `early`), the analytics with the result.
            let (tx, mut early) = futures::channel::mpsc::unbounded::<cloudflare::Snapshot>();
            let load = cx.background_spawn(crate::guard::task(async move {
                cloudflare::load(&roots, &move |s: &cloudflare::Snapshot| {
                    let _ = tx.unbounded_send(s.clone());
                })
            }));
            while let Some(snap) = early.next().await {
                let alive = this.update(cx, |this, cx| {
                    // Keep the last analytics on screen until the new ones are in.
                    let mut snap = snap;
                    if let Some(old) = &this.snap {
                        keep_analytics(&mut snap, old);
                    }
                    this.snap = Some(snap);
                    this.analytics_pending = true;
                    this.error = None;
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            let result = load.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.analytics_pending = false;
                match result {
                    Ok(snap) => {
                        if snap.problems.iter().any(|p| rate_limited(p)) {
                            this._retry = Some(cx.spawn(async move |this, cx| {
                                cx.background_executor().timer(RATE_LIMIT_RESET).await;
                                let _ = this.update(cx, |this, cx| this.refresh(cx));
                            }));
                        }
                        if this.emails.read(cx).text().trim().is_empty() && !snap.email.is_empty() {
                            let email = snap.email.clone();
                            this.emails.update(cx, |i, cx| i.set_text(email, cx));
                        }
                        this.notify_new(&snap, cx);
                        this.snap = Some(snap);
                        this.error = None;
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            });
        }));
    }

    /// Tells (on the desktop) what broke since the last look; the first look
    /// only learns what's already there.
    fn notify_new(&mut self, snap: &Snapshot, cx: &mut Context<Self>) {
        let now: Vec<(String, String, String, bool)> = alerts(snap);
        let Some(seen) = &mut self.alerted else {
            self.alerted = Some(now.into_iter().map(|a| a.0).collect());
            return;
        };
        let fresh: Vec<_> = now
            .iter()
            .filter(|a| !seen.contains(&a.0))
            .cloned()
            .collect();
        // What got fixed may break again and should tell again.
        seen.retain(|k| now.iter().any(|a| &a.0 == k));
        seen.extend(fresh.iter().map(|a| a.0.clone()));
        if fresh.len() > 3 {
            let critical = fresh.iter().any(|a| a.3);
            cx.emit(CloudflareEvent::Notify {
                title: format!("Cloudflare: {} new problems", fresh.len()),
                body: fresh
                    .iter()
                    .take(3)
                    .map(|a| a.1.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
                critical,
            });
            return;
        }
        for (_, title, body, critical) in fresh {
            cx.emit(CloudflareEvent::Notify {
                title,
                body,
                critical,
            });
        }
    }

    /// Notes that a terminal for `cf auth login` was opened, so the next open retries.
    pub fn signing_in(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    pub fn shared(&mut self, note: String, error: bool, cx: &mut Context<Self>) {
        self.share_note = Some((note.into(), error));
        cx.notify();
    }

    fn share(&mut self, cx: &mut Context<Self>) {
        let port = self
            .port
            .read(cx)
            .text()
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0);
        let emails = cloudflare::share_emails(self.emails.read(cx).text());
        let note = match (port, emails.is_empty()) {
            (None, _) => Some("Type the local port the server listens on, like 3000."),
            (_, true) => Some("Add at least one email (or *@domain) allowed to open it."),
            _ => None,
        };
        if let Some(n) = note {
            self.share_note = Some((n.into(), true));
            cx.notify();
            return;
        }
        if let Some(port) = port {
            cx.emit(CloudflareEvent::Share { port, emails });
        }
    }

    fn open(&self, path: &str, cx: &mut Context<Self>) {
        if let Some(s) = &self.snap {
            cx.emit(CloudflareEvent::Open(s.dash(path)));
        }
    }

    fn ask(&self, subject: &Subject, cx: &mut Context<Self>) {
        if let Some(a) = self
            .snap
            .as_ref()
            .and_then(|s| s.ask(subject, self.mcp(), crate::model::unix_now()))
        {
            cx.emit(CloudflareEvent::Ask(a));
        }
    }

    /// Hands `subject` to a new session of the agent.
    fn ask_button(
        &self,
        id: String,
        label: &'static str,
        subject: Subject,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let tip = match self.agent {
            AgentKind::Claude => "Starts a Claude Code session with what Arakun sees",
            AgentKind::Droid => "Starts a Droid session with what Arakun sees",
            AgentKind::OpenCode => "Starts an OpenCode session with what Arakun sees",
            AgentKind::Codex => "Starts a Codex session with what Arakun sees",
            AgentKind::Devin => "Starts a Devin session with what Arakun sees",
            AgentKind::Cursor => "Starts a Cursor session with what Arakun sees",
            AgentKind::Antigravity => "Starts an Antigravity session with what Arakun sees",
            AgentKind::WorkersAi => "Starts a Workers AI session with what Arakun sees",
        };
        ui::button_with(
            SharedString::from(id),
            Some("sparkles"),
            label,
            ButtonKind::Outline,
            true,
        )
        .tooltip(ui::tip(tip))
        .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx)))
    }

    /// `what` names the resource for screen readers: every row has this
    /// button, and the row's text never reaches them.
    fn open_button(
        &self,
        id: String,
        path: String,
        what: &str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        icon_button(
            SharedString::from(id),
            "link-square02",
            13.,
            "Open in Cloudflare",
        )
        .aria_label(format!("Open {what} in Cloudflare"))
        .on_click(cx.listener(move |this, _, _, cx| this.open(&path, cx)))
    }

    fn layout(&self) -> Layout {
        layout(self.width.get())
    }

    fn render_body(&self, s: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            Page::Summary => self.render_overview(s, cx).into_any_element(),
            Page::Containers => self.render_containers(s, cx).into_any_element(),
            Page::Logs => self.render_logs(s, cx).into_any_element(),
            Page::Security => self.render_security(s, cx).into_any_element(),
            Page::Email => self.render_email(s, cx).into_any_element(),
            Page::Domains => self.render_domains(s, cx).into_any_element(),
            Page::Tunnels => self.render_tunnels(s, cx).into_any_element(),
            Page::Workers => self.render_workers(s, cx).into_any_element(),
            Page::Storage => self.render_storage(s, cx).into_any_element(),
            Page::Activity => self.render_activity(s, cx).into_any_element(),
            Page::Tools => self.render_tools(s, cx).into_any_element(),
        }
    }

    /// `code`: the detail is an error message, set in monospace.
    fn render_issue(&self, i: usize, issue: &Issue, code: bool, cx: &mut Context<Self>) -> Div {
        let color: Hsla = match (issue.high, issue.bots) {
            (true, _) => theme::danger().into(),
            (_, true) => theme::mix(0.45),
            _ => theme::warn().into(),
        };
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(10.))
            .pl(crate::ui::sp(10.))
            .pr(crate::ui::sp(8.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .bg(theme::fill_raised())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(20.))
                    .rounded_full()
                    .bg(color.opacity(0.15))
                    .child(div().size(px(8.)).rounded_full().bg(color)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(crate::ui::sp(2.))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_MD))
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(issue.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_subtle())
                            .truncate()
                            .when(code, |d| {
                                d.font_family(theme::FONT_MONO).text_size(px(ui::TEXT_XS))
                            })
                            .child(issue.detail.clone()),
                    ),
            )
            .child(self.ask_button(
                format!("cf-issue-ask-{i}"),
                "Investigate",
                issue.subject.clone(),
                cx,
            ))
            .when_some(subject_path(&issue.subject), |d, path| {
                d.child(self.open_button(format!("cf-issue-open-{i}"), path, &issue.title, cx))
            })
    }

    fn render_mcp(&self, cx: &mut Context<Self>) -> Div {
        let connected = self.mcp == Mcp::Connected;
        let name = agent_name(self.agent);
        let body = if connected {
            format!(
                "{name} sessions can search Cloudflare's docs and query this account (logs, DNS, Tunnels, Workers, analytics) through Cloudflare's MCP server. Writes ask for permission first: Cloudflare grants extra scopes only when a session needs them."
            )
        } else {
            format!(
                "Lets {name} sessions search Cloudflare's docs and query this account through Cloudflare's MCP server. It starts read-only; when a session needs to change something, Cloudflare asks you to approve that one permission. Sessions can already read the account with the cf CLI without it."
            )
        };
        card(match self.agent {
            AgentKind::Claude => "Cloudflare MCP for Claude Code",
            AgentKind::Droid => "Cloudflare MCP for Droid",
            AgentKind::OpenCode => "Cloudflare MCP for OpenCode",
            AgentKind::Codex => "Cloudflare MCP for Codex",
            AgentKind::Devin => "Cloudflare MCP for Devin",
            AgentKind::Cursor => "Cloudflare MCP for Cursor",
            AgentKind::Antigravity => "Cloudflare MCP for Antigravity",
            AgentKind::WorkersAi => "Cloudflare MCP",
        })
        .child(
            div()
                .text_size(px(ui::TEXT_SM))
                .line_height(px(18.))
                .text_color(theme::text_muted())
                .child(body),
        )
        .child(div().flex().items_center().gap(crate::ui::sp(8.)).map(|d| {
            if connected {
                return d
                    .child(icon("checkmark-circle02", 14.).text_color(good()))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_muted())
                            .child(format!("Connected: Investigate uses it in {name}")),
                    );
            }
            let how = if self.agent == AgentKind::Claude {
                "Opens Claude Code: pick cloudflare in /mcp and approve in the browser."
            } else {
                "Opens a terminal; approve access when the browser asks."
            };
            let (label, hint): (SharedString, String) = match &self.mcp {
                Mcp::Missing => (format!("Connect to {name}").into(), how.to_string()),
                Mcp::NeedsAuth => (
                    "Sign in".into(),
                    format!("Added to {name}; it still needs your approval. {how}"),
                ),
                Mcp::Failing(why) => (
                    "Sign in again".into(),
                    format!("{name} can't reach it ({why})."),
                ),
                Mcp::NoCli => (
                    format!("Connect to {name}").into(),
                    format!(
                        "{name} isn't installed yet. Sign in to {name} in Settings to install it."
                    ),
                ),
                _ => ("Sign in".into(), format!("Checking with {name}…")),
            };
            let installed = self.mcp != Mcp::NoCli;
            let (agent, state) = (self.agent, self.mcp.clone());
            d.child(
                outline(ui::button(
                    "cf-mcp-connect",
                    label,
                    ButtonKind::Primary,
                    installed,
                ))
                .when(installed, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.watch_mcp();
                        cx.emit(CloudflareEvent::ConnectMcp(agent, state.clone()))
                    }))
                }),
            )
            .child(
                div()
                    .min_w_0()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(hint),
            )
        }))
    }

    fn render_share(&self, cx: &mut Context<Self>) -> Div {
        let field = |label: &'static str, input: Entity<TextInput>, w: Option<f32>| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(4.))
                .when(w.is_none(), |d| d.flex_1().min_w_0())
                .when_some(w, |d, w| d.w(px(w)))
                .child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(label),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(ui::BUTTON_H))
                        .px(crate::ui::sp(8.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::border_strong())
                        .text_size(px(ui::TEXT_MD))
                        .child(input),
                )
        };
        card("Share a local server")
            .child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .line_height(px(18.))
                    .text_color(theme::text_muted())
                    .child("A temporary public link to a port on this machine. Only these emails can open it: they get a one-time code by email."),
            )
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(crate::ui::sp(8.))
                    .child(field("Port", self.port.clone(), Some(72.)))
                    .child(field("Who can open it", self.emails.clone(), None))
                    .child(
                        outline(ui::button("cf-share", "Share", ButtonKind::Primary, self.cloudflared))
                            .when(self.cloudflared, |b| b.on_click(cx.listener(|this, _, _, cx| this.share(cx)))),
                    ),
            )
            .when(!self.cloudflared, |d| d.child(note("Install cloudflared to share (your package manager has it).", true)))
            .when_some(self.share_note.clone(), |d, (n, error)| d.child(note(n, error)))
    }

    fn render_domains(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let mut list = section("Domains", Some(s.zones.len().to_string()), None);
        if s.zones.is_empty() {
            list = list.child(quiet("No domains in this account."));
        }
        for z in &s.zones {
            list = list.child(self.render_zone(s, z, cx));
            if self.traffic_open.contains(&z.id) {
                list = list.child(self.render_traffic(z, cx));
            }
        }
        list
    }

    fn render_zone(&self, s: &Snapshot, z: &Zone, cx: &mut Context<Self>) -> Div {
        let name = z.name.clone();
        let classes = z.classes();
        let served = z.served();
        let worst = z.server_statuses().first().copied();
        let workers: Vec<String> = s.workers_on(z).iter().map(|w| w.name.clone()).collect();
        let header = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ui::TEXT_LG))
                    .font_weight(FontWeight::MEDIUM)
                    .child(name.clone()),
            )
            .child(ui::tag(z.plan.clone(), theme::mix(0.6)))
            .when(z.status != "active", |d| {
                d.child(ui::tag(z.status.clone(), theme::warn()))
            })
            .when(z.settings.read, |d| {
                d.child(ui::tag(
                    format!("SSL {}", z.settings.ssl_label()),
                    if z.settings.weak_ssl() {
                        theme::warn().into()
                    } else {
                        theme::mix(0.6)
                    },
                ))
            })
            .when(z.settings.dev_mode, |d| {
                d.child(ui::tag("dev mode", theme::warn()))
            })
            .children(
                z.certs
                    .iter()
                    .filter(|c| c.expires > 0)
                    .map(|c| c.days_left(crate::model::unix_now()))
                    .min()
                    .map(|days| {
                        ui::tag(
                            format!("cert {days}d"),
                            if days < 14 {
                                theme::danger().into()
                            } else if days < 30 {
                                theme::warn().into()
                            } else {
                                theme::mix(0.6)
                            },
                        )
                    }),
            )
            .when(z.email_enabled == Some(true), |d| {
                d.child(ui::tag(
                    format!("email · {} rules", z.email_rules.len()),
                    theme::mix(0.6),
                ))
            })
            .when(z.live_server_errors() > 0, |d| {
                d.child(ui::tag(
                    format!("{} × 5xx", count(z.live_server_errors())),
                    theme::danger(),
                ))
            })
            .child(div().flex_1())
            .when(z.live_server_errors() > 0, |d| {
                d.child(self.ask_button(
                    format!("cf-zone-ask-{name}"),
                    "Investigate 5xx",
                    Subject::Zone(name.clone()),
                    cx,
                ))
            })
            .child({
                let id = z.id.clone();
                let open = self.traffic_open.contains(&z.id);
                ui::button_with(
                    SharedString::from(format!("cf-traffic-{name}")),
                    Some("chart-breakout-square"),
                    if open { "Hide traffic" } else { "Traffic" },
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_traffic(id.clone(), cx)))
            })
            .child({
                let security = format!("{name}/security/events");
                ui::button_with(
                    SharedString::from(format!("cf-sec-{name}")),
                    Some("shield-alert"),
                    "Security events",
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open(&security, cx)))
            })
            .child(self.open_button(format!("cf-zone-{name}"), name.clone(), &name, cx));
        let week = div()
            .flex()
            .items_end()
            .gap(crate::ui::sp(24.))
            .child(spark(&z.days))
            .child(stat("Requests · 7d", count(z.requests())))
            .child(stat("Visitors / day", count(z.visitors_per_day())))
            .child(stat("From cache", format!("{}%", z.cache_pct())))
            .child(stat("Threats", count(z.threats())))
            .child(stat("Data", bytes(z.bytes())));
        let day = (served > 0).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(12.))
                        .child(
                            div()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .child(format!("{} responses · 24h", count(served))),
                        )
                        .child(div().flex_1())
                        .children((0..5).filter(|&i| classes[i] > 0).map(|i| {
                            div()
                                .flex()
                                .items_center()
                                .gap(crate::ui::sp(6.))
                                .text_size(px(ui::TEXT_XS))
                                .child(div().size(px(6.)).rounded_full().bg(class_color(i)))
                                .child(
                                    div()
                                        .text_color(theme::text_muted())
                                        .child(format!("{}xx", i + 1)),
                                )
                                .child(
                                    div()
                                        .text_color(theme::text_subtle())
                                        .child(pct(classes[i], served)),
                                )
                        })),
                )
                .child(share_bar(&classes, served))
        });
        let failing = (!z.failing.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(4.))
                .pt(crate::ui::sp(2.))
                .child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child("Most 5xx · 24h"),
                )
                .children(z.failing.iter().map(|f| {
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .h(px(20.))
                        .child(ui::tag(f.status.to_string(), status_color(f.status)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(theme::FONT_MONO)
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_muted())
                                .truncate()
                                .child(format!("{}{}", f.host, f.path)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .child(format!("{}×", count(f.count))),
                        )
                }))
        });
        let network = self.render_zone_network(s, z, cx);
        let mut meta = Vec::new();
        if let Some((code, _)) = worst {
            meta.push(format!(
                "Mostly {code}: {}",
                cloudflare::status_meaning(code)
            ));
        }
        if !workers.is_empty() {
            meta.push(format!("Workers: {}", workers.join(", ")));
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .p(crate::ui::sp(16.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .bg(theme::fill_raised())
            .child(header)
            .child(week)
            .children(day)
            .children(failing)
            .when(!meta.is_empty(), |d| d.child(meta_line(meta.join(" · "))))
            .child(network)
    }

    /// The zone's SSL settings, renewal and DNS records; the records that need a
    /// look always show, the rest behind a toggle.
    fn render_zone_network(&self, s: &Snapshot, z: &Zone, cx: &mut Context<Self>) -> Div {
        let st = &z.settings;
        let mut facts = Vec::new();
        if st.read {
            facts.push(format!(
                "Always HTTPS {}",
                if st.always_https { "on" } else { "off" }
            ));
            if !st.min_tls.is_empty() {
                facts.push(format!("min TLS {}", st.min_tls));
            }
        }
        if let Some(r) = s.registrations.iter().find(|r| r.name == z.name) {
            facts.push(if r.auto_renew {
                format!("renews {}", cloudflare::date(r.expires))
            } else {
                format!("expires {} (no auto-renew)", cloudflare::date(r.expires))
            });
        }
        let flagged = |r: &DnsRecord| record_note(s, r).is_some_and(|(_, bad)| bad);
        let open = self.dns_open.contains(&z.name);
        let shown: Vec<&DnsRecord> = z.dns.iter().filter(|r| open || flagged(r)).collect();
        let name = z.name.clone();
        let toggle = (!z.dns.is_empty()).then(|| {
            let label = if open {
                "Hide records".to_string()
            } else {
                format!("DNS records · {}", z.dns.len())
            };
            ui::button_with(
                SharedString::from(format!("cf-dns-{name}")),
                Some(if open { "arrow-up01" } else { "arrow-down01" }),
                label,
                ButtonKind::Ghost,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.dns_open.remove(&name) {
                    this.dns_open.insert(name.clone());
                }
                cx.notify();
            }))
        });
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(4.))
            .pt(crate::ui::sp(4.))
            .border_t_1()
            .border_color(theme::stroke())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(meta_line(if facts.is_empty() {
                                "DNS and SSL settings unavailable".into()
                            } else {
                                facts.join(" · ")
                            })),
                    )
                    .when(!z.dns.is_empty(), |d| {
                        d.child(self.ask_button(
                            format!("cf-dns-ask-{}", z.name),
                            "Check DNS",
                            Subject::Dns(z.name.clone()),
                            cx,
                        ))
                    })
                    .children(toggle),
            )
            .children(shown.into_iter().map(|r| dns_row(s, r)))
    }

    fn render_tunnels(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let now = crate::model::unix_now();
        let latest = s.latest_cloudflared();
        let local = &s.cloudflared;
        let here =
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(10.))
                .px(crate::ui::sp(12.))
                .py(crate::ui::sp(10.))
                .rounded(crate::ui::rad(ui::RADIUS_LG))
                .bg(theme::fill_raised())
                .child(icon("computer", 14.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(crate::ui::sp(2.))
                        .child(div().text_size(px(ui::TEXT_MD)).child(
                            if local.installed.is_empty() {
                                "cloudflared isn't installed on this machine".to_string()
                            } else {
                                format!("cloudflared {} on this machine", local.installed)
                            },
                        ))
                        .child(meta_line(if latest.is_empty() {
                            "Couldn't read the latest release".into()
                        } else {
                            format!("Latest release: {latest}")
                        })),
                )
                .map(|d| {
                    if cloudflare::outdated(&local.installed, &latest) {
                        d.child(ui::tag("outdated", theme::warn()))
                            .child(self.ask_button(
                                "cf-cfd-update".into(),
                                "Update",
                                Subject::Cloudflared,
                                cx,
                            ))
                    } else if local.installed.is_empty() {
                        d
                    } else {
                        d.child(ui::tag("up to date", good()))
                    }
                });
        if !s.tunnels_read {
            return div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(16.))
                .child(here)
                .child(quiet(
                    "This sign-in can't read Tunnels yet. Grant read access in Tools.",
                ));
        }
        let (live, idle): (Vec<&Tunnel>, Vec<&Tunnel>) = s
            .tunnels
            .iter()
            .partition(|t| t.up() || !t.hostnames.is_empty());
        let open = ui::button_with(
            SharedString::from("cf-tunnels-open"),
            Some("link-square02"),
            "Open in dashboard",
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.open("tunnels", cx)));
        let mut list = section(
            "Tunnels",
            Some(format!(
                "{} · {} up",
                s.tunnels.len(),
                s.tunnels.iter().filter(|t| t.up()).count()
            )),
            Some(open.into_any_element()),
        );
        if live.is_empty() {
            list = list.child(quiet("No Tunnel is running or routed to."));
        }
        for t in live {
            list = list.child(self.render_tunnel(t, &latest, now, cx));
        }
        let mut unused = section("Unused", Some(idle.len().to_string()), None);
        for t in &idle {
            let last = if t.inactive_at > 0 {
                format!("last connected {}", cloudflare::ago(t.inactive_at, now))
            } else {
                "never connected".into()
            };
            unused = unused.child(line_item(
                "circle-dashed",
                t.name.clone(),
                format!("{} · no DNS record routes to it", t.status),
                last,
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(24.))
            .child(here)
            .child(list)
            .when(!idle.is_empty(), |d| {
                d.child(unused.child(meta_line("Down and nothing routes to them: safe to delete from the dashboard when you no longer need them.".into())))
            })
    }

    fn render_tunnel(&self, t: &Tunnel, latest: &str, now: u64, cx: &mut Context<Self>) -> Div {
        let color: Hsla = match t.status.as_str() {
            "healthy" => good(),
            "degraded" => theme::warn().into(),
            "down" => theme::danger().into(),
            _ => theme::mix(0.5),
        };
        let name = t.name.clone();
        let header = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .child(div().size(px(8.)).rounded_full().bg(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(ui::TEXT_MD))
                    .font_weight(FontWeight::MEDIUM)
                    .truncate()
                    .child(name.clone()),
            )
            .child(ui::tag(t.status.clone(), color))
            .child(ui::tag(
                if t.remote {
                    "dashboard config"
                } else {
                    "local config"
                },
                theme::mix(0.5),
            ))
            .child(self.ask_button(
                format!("cf-tun-ask-{name}"),
                "Investigate",
                Subject::Tunnel(name.clone()),
                cx,
            ))
            .child(self.open_button(format!("cf-tun-open-{name}"), "tunnels".into(), &name, cx));
        let hosts = if t.hostnames.is_empty() {
            "No DNS record routes to it".to_string()
        } else {
            t.hostnames.join(", ")
        };
        row()
            .gap(crate::ui::sp(8.))
            .child(header)
            .child(
                div()
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_muted())
                    .truncate()
                    .child(hosts),
            )
            .when(t.connectors.is_empty(), |d| {
                d.child(meta_line(if t.inactive_at > 0 {
                    format!(
                        "No connector: the last one left {}",
                        cloudflare::ago(t.inactive_at, now)
                    )
                } else {
                    "No connector has ever run it".into()
                }))
            })
            .children(t.connectors.iter().map(|c| {
                let old = cloudflare::outdated(&c.version, latest);
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(20.))
                    .text_size(px(ui::TEXT_XS))
                    .child(icon("computer", 12.).text_color(theme::text_subtle()))
                    .child(ui::tag(
                        c.version.clone(),
                        if old {
                            theme::warn().into()
                        } else {
                            theme::mix(0.6)
                        },
                    ))
                    .when(old, |d| {
                        d.child(
                            div()
                                .text_color(theme::warn())
                                .child(format!("update to {latest}")),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::text_muted())
                            .child(format!(
                                "{} · {}",
                                crate::redact::value(&c.origin_ip),
                                c.colos.join(", ")
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme::text_subtle())
                            .child(format!(
                                "up {}",
                                cloudflare::ago(c.opened, now).trim_end_matches(" ago")
                            )),
                    )
            }))
    }

    fn render_workers(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let now = crate::model::unix_now();
        let requests: u64 = s.workers.iter().map(|w| w.requests).sum();
        let busy = s.workers.iter().filter(|w| w.requests > 0).count();
        let mut list = section(
            "Workers",
            Some(format!(
                "{} · {} active · {} requests in 24h",
                s.workers.len(),
                busy,
                count(requests)
            )),
            None,
        );
        if s.workers.is_empty() {
            list = list.child(quiet("No Workers in this account."));
        }
        for w in &s.workers {
            list = list.child(self.render_worker(w, now, cx));
        }
        let mut pages = section("Pages", Some(s.pages.len().to_string()), None);
        if s.pages.is_empty() {
            pages = pages.child(quiet("No Pages projects."));
        }
        for p in &s.pages {
            let name = p.name.clone();
            let color: Hsla = match p.status.as_str() {
                "success" => good(),
                "failure" => theme::danger().into(),
                "canceled" | "skipped" => theme::mix(0.5),
                _ => theme::accent(),
            };
            let mut meta = Vec::new();
            if p.deployed > 0 {
                meta.push(format!(
                    "deployed {}{}",
                    cloudflare::ago(p.deployed, now),
                    if p.branch.is_empty() {
                        String::new()
                    } else {
                        format!(" from {}", p.branch)
                    }
                ));
            }
            if !p.repo.is_empty() {
                meta.push(format!("{} {}", p.source, p.repo));
            }
            if !p.domains.is_empty() {
                meta.push(p.domains.join(", "));
            }
            pages = pages.child(
                row()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(ui::TEXT_MD))
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(name.clone()),
                            )
                            .when(!p.status.is_empty(), |d| {
                                d.child(ui::tag(
                                    if p.stage.is_empty() {
                                        p.status.clone()
                                    } else {
                                        format!("{} · {}", p.stage, p.status)
                                    },
                                    color,
                                ))
                            })
                            .when(p.failed(), |d| {
                                d.child(self.ask_button(
                                    format!("cf-pages-ask-{name}"),
                                    "Investigate",
                                    Subject::Pages(name.clone()),
                                    cx,
                                ))
                            })
                            .child(self.open_button(
                                format!("cf-pages-open-{name}"),
                                format!("pages/view/{name}"),
                                &name,
                                cx,
                            )),
                    )
                    .child(meta_line(meta.join(" · "))),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(24.))
            .child(list)
            .child(pages)
    }

    fn render_worker(&self, w: &Worker, now: u64, cx: &mut Context<Self>) -> Div {
        let name = w.name.clone();
        let mut meta = vec![if w.deployed > 0 {
            format!(
                "deployed {}{}",
                cloudflare::ago(w.deployed, now),
                if w.deployed_from.is_empty() {
                    String::new()
                } else {
                    format!(" from {}", w.deployed_from)
                }
            )
        } else {
            "never deployed".into()
        }];
        if !w.author.is_empty() {
            meta.push(format!(
                "by {}",
                crate::redact::value(w.author.split('@').next().unwrap_or(&w.author))
            ));
        }
        if !w.domains.is_empty() {
            meta.push(w.domains.join(", "));
        }
        if !w.crons.is_empty() {
            meta.push(format!("cron {}", w.crons.join(", ")));
        }
        let sample = w.samples.first().map(|e| {
            let first = e
                .message
                .lines()
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            (
                first,
                format!("{} · {}", cloudflare::ago(e.when, now), e.trigger),
            )
        });
        let header = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(ui::TEXT_MD))
                    .font_weight(FontWeight::MEDIUM)
                    .truncate()
                    .child(name.clone()),
            )
            .when(w.errors > 0, |d| {
                let color: Hsla = if w.bot_noise() {
                    theme::warn().into()
                } else {
                    theme::danger().into()
                };
                d.child(ui::tag(
                    format!(
                        "{} {}",
                        count(w.errors),
                        if w.errors == 1 { "error" } else { "errors" }
                    ),
                    color,
                ))
            })
            .when(w.bot_noise(), |d| {
                d.child(ui::tag("from bots", theme::text_subtle()))
            })
            .when(w.requests == 0, |d| {
                d.child(ui::tag("idle", theme::text_subtle()))
            })
            .child(self.ask_button(
                format!("cf-inv-{name}"),
                if w.errors > 0 {
                    "Investigate"
                } else {
                    "Health check"
                },
                Subject::Worker(name.clone()),
                cx,
            ))
            .children(self.homes.get(&name).cloned().map(|root| {
                let label = root.file_name().map_or_else(
                    || root.display().to_string(),
                    |n| n.to_string_lossy().to_string(),
                );
                ui::button_with(
                    SharedString::from(format!("cf-home-{name}")),
                    Some("folder01"),
                    label,
                    ButtonKind::Ghost,
                    true,
                )
                .tooltip(ui::tip("Its project on this machine: open it"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(CloudflareEvent::OpenProject(root.clone()))
                }))
            }))
            .child({
                let worker = name.clone();
                ui::button_with(
                    SharedString::from(format!("cf-tail-{name}")),
                    Some("activity01"),
                    "Live logs",
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.page = Page::Logs;
                    this.start_tail(worker.clone(), cx);
                }))
            })
            .child(self.open_button(
                format!("cf-open-{name}"),
                format!("workers/services/view/{name}/production"),
                &name,
                cx,
            ));
        let stats = (w.requests > 0).then(|| {
            div()
                .flex()
                .gap(crate::ui::sp(24.))
                .child(stat("Requests · 24h", count(w.requests)))
                .child(stat("Error rate", pct(w.errors, w.requests)))
                .child(stat("CPU median", cloudflare::ms(w.cpu_p50)))
                .child(stat("CPU p99", cloudflare::ms(w.cpu_p99)))
                .when(w.subrequests > 0, |d| {
                    d.child(stat("Subrequests", count(w.subrequests)))
                })
        });
        row()
            .gap(crate::ui::sp(8.))
            .child(header)
            .children(stats)
            .child(meta_line(meta.join(" · ")))
            .when(!w.bindings.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(crate::ui::sp(4.))
                        .children(w.bindings.iter().map(|b| chip(&b.kind, &b.target))),
                )
            })
            .when_some(sample, |d, (msg, when)| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(2.))
                        .px(crate::ui::sp(8.))
                        .py(crate::ui::sp(6.))
                        .rounded(crate::ui::rad(4.))
                        .bg(theme::fill_hover())
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_muted())
                                .truncate()
                                .child(msg),
                        )
                        .child(
                            div()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .truncate()
                                .child(when),
                        ),
                )
            })
    }

    fn render_storage(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let now = crate::model::unix_now();
        let monthly = s.r2_monthly();
        let objects: u64 = s.buckets.iter().map(|b| b.objects).sum();
        let queries: u64 = s.databases.iter().map(|d| d.reads + d.writes).sum();
        let logged: u64 = s.gateways.iter().map(|g| g.logged).sum();
        let metrics = div()
            .flex()
            .gap(crate::ui::sp(10.))
            .child(metric(
                "R2 stored",
                known(s.usage, bytes(s.r2_bytes())),
                None,
                if !s.usage {
                    "analytics unavailable right now".into()
                } else if monthly >= 0.5 {
                    format!("≈ ${monthly:.0}/month at Standard rates")
                } else {
                    "within the free 10 GB".into()
                },
            ))
            .child(metric(
                "R2 objects",
                known(s.usage, count(objects)),
                None,
                format!("in {}", plural(s.buckets.len(), "bucket", "buckets")),
            ))
            .child(metric(
                "D1 queries · 7d",
                known(s.usage, count(queries)),
                None,
                plural(s.databases.len(), "database", "databases"),
            ))
            .child(metric(
                "AI Gateway requests",
                count(logged),
                None,
                format!(
                    "logged by {}",
                    plural(s.gateways.len(), "gateway", "gateways")
                ),
            ));

        let largest = s.buckets.first().map_or(1, |b| b.size.max(1));
        let mut r2 = section("R2 buckets", Some(s.buckets.len().to_string()), None);
        if s.buckets.is_empty() {
            r2 = r2.child(quiet("No R2 buckets."));
        }
        for b in &s.buckets {
            let path = format!("r2/default/buckets/{}", b.name);
            let users = s.bucket_users(&b.name);
            let mut meta = vec![format!("{} objects", count(b.objects))];
            if b.reads + b.writes > 0 {
                meta.push(format!(
                    "{} reads, {} writes in 24h",
                    count(b.reads),
                    count(b.writes)
                ));
            }
            meta.push(if users.is_empty() {
                format!("created {}", cloudflare::ago(b.created, now))
            } else {
                format!("used by {}", users.join(", "))
            });
            r2 = r2.child(
                row()
                    .id(SharedString::from(format!("cf-r2-{}", b.name)))
                    .cursor_pointer()
                    .hover(|st| st.bg(theme::fill_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| this.open(&path, cx)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(10.))
                            .child(icon("archive02", 14.).text_color(theme::text_subtle()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(ui::TEXT_MD))
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(b.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(ui::TEXT_SM))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(bytes(b.size)),
                            ),
                    )
                    .child(meter(b.size as f32 / largest as f32, theme::accent()))
                    .child(meta_line(meta.join(" · "))),
            );
        }

        let mut d1 = section("D1 databases", Some(s.databases.len().to_string()), None);
        if s.databases.is_empty() {
            d1 = d1.child(quiet("No D1 databases."));
        }
        for db in &s.databases {
            let path = format!("workers/d1/databases/{}", db.id);
            d1 = d1.child(
                row()
                    .id(SharedString::from(format!("cf-d1-{}", db.id)))
                    .cursor_pointer()
                    .hover(|st| st.bg(theme::fill_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| this.open(&path, cx)))
                    .gap(crate::ui::sp(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(10.))
                            .child(
                                icon("left-to-right-list-bullet", 14.)
                                    .text_color(theme::text_subtle()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(ui::TEXT_MD))
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(db.name.clone()),
                            )
                            .when(!db.used_by.is_empty(), |d| {
                                d.child(meta_line(format!("used by {}", db.used_by.join(", "))))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(crate::ui::sp(24.))
                            .pl(crate::ui::sp(24.))
                            .child(stat("Reads · 7d", count(db.reads)))
                            .child(stat("Writes · 7d", count(db.writes)))
                            .child(stat("Rows read", count(db.rows_read)))
                            .child(stat("Rows written", count(db.rows_written)))
                            .when(db.size > 0, |d| d.child(stat("Size", bytes(db.size))))
                            .when(db.tables > 0, |d| d.child(stat("Tables", count(db.tables)))),
                    ),
            );
        }

        let mut queues = section("Queues", Some(s.queues.len().to_string()), None);
        if s.queues.is_empty() {
            queues = queues.child(quiet("No Queues."));
        }
        for q in &s.queues {
            let flow = format!(
                "{} → {}",
                list_or(&q.producers, "no producer"),
                list_or(&q.consumers, "no consumer")
            );
            let when = if q.paused {
                "paused".into()
            } else {
                format!("created {}", cloudflare::ago(q.created, now))
            };
            queues = queues.child(
                line_item("inbox", q.name.clone(), flow, when)
                    .when(q.paused, |d| d.text_color(theme::warn())),
            );
        }

        let mut flows = section("Workflows", Some(s.flows.len().to_string()), None);
        if s.flows.is_empty() {
            flows = flows.child(quiet("No Workflows."));
        }
        for f in &s.flows {
            let name = f.name.clone();
            flows = flows.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .px(crate::ui::sp(12.))
                    .py(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(8.))
                    .bg(theme::fill_raised())
                    .child(icon("git-branch", 14.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(crate::ui::sp(2.))
                            .child(
                                div()
                                    .text_size(px(ui::TEXT_MD))
                                    .truncate()
                                    .child(name.clone()),
                            )
                            .child(meta_line(if f.triggered > 0 {
                                format!(
                                    "Worker {} · last run {}",
                                    f.script,
                                    cloudflare::ago(f.triggered, now)
                                )
                            } else {
                                format!("Worker {}", f.script)
                            })),
                    )
                    .when(f.running > 0, |d| {
                        d.child(ui::tag(
                            format!("{} running", count(f.running)),
                            theme::accent(),
                        ))
                    })
                    .when(f.queued > 0, |d| {
                        d.child(ui::tag(
                            format!("{} queued", count(f.queued)),
                            theme::mix(0.6),
                        ))
                    })
                    .when(f.errored > 0, |d| {
                        d.child(ui::tag(
                            format!("{} errored", count(f.errored)),
                            theme::danger(),
                        ))
                    })
                    .when(f.complete > 0, |d| {
                        d.child(ui::tag(format!("{} complete", count(f.complete)), good()))
                    })
                    .when(f.errored > 0, |d| {
                        d.child(self.ask_button(
                            format!("cf-flow-ask-{name}"),
                            "Investigate",
                            Subject::Workflow(name.clone()),
                            cx,
                        ))
                    })
                    .child(self.open_button(
                        format!("cf-flow-{name}"),
                        "workers/workflows".into(),
                        &name,
                        cx,
                    )),
            );
        }

        let mut ai = section("AI Gateway", Some(s.gateways.len().to_string()), None);
        if s.gateways.is_empty() {
            ai = ai.child(quiet("No AI Gateways."));
        }
        for g in &s.gateways {
            let id = g.id.clone();
            let failed = g.failures();
            let spent: f64 = g.recent.iter().map(|c| c.cost).sum();
            ai = ai.child(
                row()
                    .gap(crate::ui::sp(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(icon("ai-idea", 14.).text_color(theme::text_subtle()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(ui::TEXT_MD))
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(id.clone()),
                            )
                            .child(ui::tag(
                                format!("{} logged", count(g.logged)),
                                theme::mix(0.6),
                            ))
                            .when(failed > 0, |d| {
                                d.child(ui::tag(
                                    format!("{failed} of last {} failed", g.recent.len()),
                                    theme::danger(),
                                ))
                            })
                            .when(failed > 0, |d| {
                                d.child(self.ask_button(
                                    format!("cf-gw-ask-{id}"),
                                    "Investigate",
                                    Subject::Gateway(id.clone()),
                                    cx,
                                ))
                            })
                            .child(self.open_button(
                                format!("cf-gw-{id}"),
                                "ai/ai-gateway".into(),
                                &id,
                                cx,
                            )),
                    )
                    .when(g.recent.is_empty(), |d| {
                        d.child(meta_line("No requests yet.".into()))
                    })
                    .when(!g.recent.is_empty(), |d| {
                        d.child(meta_line(format!(
                            "Last {} requests{}",
                            g.recent.len(),
                            if spent > 0. {
                                format!(" cost {}", dollars(spent))
                            } else {
                                String::new()
                            }
                        )))
                    })
                    .children(g.recent.iter().take(6).map(|c| {
                        let tokens = if c.tokens_in + c.tokens_out > 0 {
                            format!("{} → {} tokens", count(c.tokens_in), count(c.tokens_out))
                        } else {
                            String::new()
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .h(px(20.))
                            .text_size(px(ui::TEXT_XS))
                            .child(ui::tag(
                                c.status.to_string(),
                                if c.ok { good() } else { theme::danger().into() },
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::text_muted())
                                    .child(format!(
                                        "{} · {}{}",
                                        c.provider,
                                        c.model,
                                        if c.cached { " · cached" } else { "" }
                                    )),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme::text_subtle())
                                    .child(tokens),
                            )
                            .when(c.cost > 0., |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .text_color(theme::text_subtle())
                                        .child(dollars(c.cost)),
                                )
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(56.))
                                    .text_right()
                                    .text_color(theme::text_subtle())
                                    .child(cloudflare::ago(c.when, now)),
                            )
                    })),
            );
        }

        let mut kv = section("KV namespaces", Some(s.kv.len().to_string()), None);
        if s.kv.is_empty() {
            kv = kv.child(quiet("No KV namespaces."));
        }
        for n in &s.kv {
            let users: Vec<&str> = s
                .workers
                .iter()
                .filter(|w| {
                    w.bindings.iter().any(|b| {
                        b.kind == "KV" && (b.target == n.title || n.title.ends_with(&b.target))
                    })
                })
                .map(|w| w.name.as_str())
                .collect();
            kv = kv.child(line_item(
                "archive02",
                n.title.clone(),
                if users.is_empty() {
                    String::new()
                } else {
                    format!("bound by {}", users.join(", "))
                },
                String::new(),
            ));
        }

        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(24.))
            .child(metrics)
            .child(r2)
            .child(d1)
            .child(kv)
            .child(ai)
            .child(flows)
            .child(queues)
    }

    fn render_activity(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let now = crate::model::unix_now();
        let mut alerts = section("Alerts · 30 days", Some(s.alerts.len().to_string()), None);
        if s.alerts.is_empty() {
            alerts = alerts.child(quiet("No notifications sent."));
        }
        for a in &s.alerts {
            alerts = alerts.child(line_item(
                "alert-circle",
                a.name.clone(),
                a.detail.clone(),
                cloudflare::ago(a.sent, now),
            ));
        }
        let open = ui::button_with(
            SharedString::from("cf-audit-open"),
            Some("link-square02"),
            "Open in dashboard",
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| this.open("audit-log", cx)));
        let mut audit = section(
            "Audit log",
            Some(s.audit.len().to_string()),
            Some(open.into_any_element()),
        );
        if s.audit.is_empty() {
            audit = audit.child(quiet("Nothing changed recently."));
        }
        for e in &s.audit {
            let who = if e.resource.is_empty() {
                crate::redact::value(&e.actor)
            } else {
                format!("{} · {}", crate::redact::value(&e.actor), e.resource)
            };
            audit = audit.child(line_item(
                "clock01",
                e.action.clone(),
                who,
                cloudflare::ago(e.when, now),
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(24.))
            .child(alerts)
            .child(audit)
    }

    fn render_tools(&self, s: &Snapshot, cx: &mut Context<Self>) -> Div {
        let command = cloudflare::access_command(&s.profile);
        let grant = outline(ui::button(
            "cf-grant",
            "Grant read access",
            ButtonKind::Primary,
            true,
        ))
        .on_click(cx.listener(move |_, _, _, cx| cx.emit(CloudflareEvent::Run(command.clone()))));
        let access = card("What this tab and agent sessions can read")
            .child(
                div().text_size(px(ui::TEXT_SM)).line_height(px(18.)).text_color(theme::text_muted()).child(format!(
                    "Arakun reads the account with the cf CLI's sign-in ({}). Agent sessions use the same sign-in from any project: they can run cf to read DNS, Tunnels, Workers, logs and settings. Granting read access asks Cloudflare once for every read permission this tab uses; writes stay limited to what you approved before.",
                    if s.profile.is_empty() { "default profile".to_string() } else { format!("profile {}", crate::redact::value(&s.profile)) }
                )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(grant)
                    .child(div().min_w_0().text_size(px(ui::TEXT_XS)).text_color(theme::text_subtle()).child("Opens a terminal; approve in the browser, then refresh.")),
            );
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(access)
            .child(
                div()
                    .flex()
                    .gap(crate::ui::sp(10.))
                    // Workers AI has no MCP client; Arakun cannot connect Devin's
                    // or Cursor's or Antigravity's yet.
                    .when(
                        !matches!(
                            self.agent,
                            AgentKind::WorkersAi
                                | AgentKind::Devin
                                | AgentKind::Cursor
                                | AgentKind::Antigravity
                        ),
                        |d| d.child(self.render_mcp(cx)),
                    )
                    .child(self.render_share(cx)),
            )
    }

    fn render_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let center = || {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(crate::ui::sp(10.))
                .pt(crate::ui::sp(60.))
                .text_size(px(ui::TEXT_MD))
                .text_color(theme::text_subtle())
        };
        match &self.error {
            Some(LoadError::NoCli) => center()
                .child("Cloudflare's cf CLI isn't installed.")
                .child(quiet(
                    "Arakun reads your account through its sign-in (read-only).",
                ))
                .child(
                    outline(ui::button(
                        "cf-install",
                        "Install cf and sign in",
                        ButtonKind::Primary,
                        true,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.signing_in(cx);
                        cx.emit(CloudflareEvent::SignIn { install: true })
                    })),
                )
                .into_any_element(),
            Some(LoadError::SignedOut(why)) => center()
                .child("Sign in to Cloudflare to see your account here.")
                .child(quiet(format!("cf says: {why}")))
                .child(
                    outline(ui::button(
                        "cf-login",
                        "Sign in with cf",
                        ButtonKind::Primary,
                        true,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.signing_in(cx);
                        cx.emit(CloudflareEvent::SignIn { install: false })
                    })),
                )
                .into_any_element(),
            Some(LoadError::Failed(e)) => center()
                .child("Couldn't read your Cloudflare account.")
                .child(quiet(e.clone()))
                .child(
                    ui::button("cf-retry", "Try again", ButtonKind::Outline, true)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
                .into_any_element(),
            None => center()
                .child(ui::loading("Reading your Cloudflare account…"))
                .into_any_element(),
        }
    }
}

/// Where in the dashboard an issue's subject lives.
/// Carries the analytics of `old` into a snapshot that has none yet, so numbers
/// don't drop to zero while the new ones load.
fn keep_analytics(snap: &mut cloudflare::Snapshot, old: &cloudflare::Snapshot) {
    if !snap.traffic && old.traffic {
        for z in &mut snap.zones {
            if let Some(o) = old.zones.iter().find(|o| o.id == z.id) {
                z.days = o.days.clone();
                z.statuses = o.statuses.clone();
                z.failing = o.failing.clone();
            }
        }
        snap.traffic = true;
    }
    if !snap.usage && old.usage {
        for w in &mut snap.workers {
            if let Some(o) = old.workers.iter().find(|o| o.name == w.name) {
                w.requests = o.requests;
                w.subrequests = o.subrequests;
                w.cpu_p50 = o.cpu_p50;
                w.cpu_p99 = o.cpu_p99;
                w.errors = o.errors;
                w.samples = o.samples.clone();
            }
        }
        for b in &mut snap.buckets {
            if let Some(o) = old.buckets.iter().find(|o| o.name == b.name) {
                b.size = o.size;
                b.objects = o.objects;
                b.reads = o.reads;
                b.writes = o.writes;
            }
        }
        if snap.databases.is_empty() {
            snap.databases = old.databases.clone();
        }
        snap.usage = true;
    }
}

/// What deserves a notification when it first shows: (key, title, body, critical).
/// Keys don't hold counts, so "36 errors" becoming "40" isn't new.
fn alerts(s: &Snapshot) -> Vec<(String, String, String, bool)> {
    let now = crate::model::unix_now();
    let mut out: Vec<(String, String, String, bool)> = s
        .issues()
        .into_iter()
        .filter(|i| !i.bots)
        .map(|i| {
            (
                format!("issue/{:?}/{}", i.subject, i.high),
                format!("Cloudflare: {}", i.title),
                i.detail.clone(),
                i.high,
            )
        })
        .collect();
    for z in &s.zones {
        for c in z
            .certs
            .iter()
            .filter(|c| c.expires > 0 && c.days_left(now) < 14)
        {
            let host = c.hosts.first().cloned().unwrap_or_else(|| z.name.clone());
            out.push((
                format!("cert/{host}/{}", c.expires),
                format!("Cloudflare: the certificate for {host} expires in {} days", c.days_left(now).max(0)),
                "Cloudflare renews it by itself unless validation fails; check the domain's SSL page.".into(),
                c.days_left(now) < 3,
            ));
        }
    }
    for i in s.insights.iter().filter(|i| i.rank() <= 1) {
        out.push((
            format!("insight/{}/{}", i.class, i.subject),
            format!("Cloudflare Security: {}", i.title()),
            format!("{} · {} severity", i.subject, i.severity),
            true,
        ));
    }
    out
}

fn subject_path(s: &Subject) -> Option<String> {
    Some(match s {
        Subject::Worker(name) => format!("workers/services/view/{name}/production"),
        Subject::Zone(name) => name.clone(),
        Subject::Gateway(_) => "ai/ai-gateway".into(),
        Subject::Workflow(_) => "workers/workflows".into(),
        Subject::Queue(_) => "workers/queues".into(),
        Subject::Tunnel(_) => "tunnels".into(),
        Subject::Dns(zone) => format!("{zone}/dns/records"),
        Subject::Ssl(zone) => format!("{zone}/ssl-tls"),
        Subject::Registration(_) => "registrar/domains".into(),
        Subject::Pages(name) => format!("pages/view/{name}"),
        Subject::Cloudflared => return None,
    })
}

/// Whether a load problem is Cloudflare's analytics rate limit, which resets by itself.
fn rate_limited(problem: &str) -> bool {
    problem.contains("rate limit")
}

/// Whether a load problem comes from a permission the sign-in lacks.
fn access_denied(problem: &str) -> bool {
    let p = problem.to_ascii_lowercase();
    [
        "authentication error",
        "unauthorized",
        "can't list",
        "not authorized",
        "insufficient",
        "permission",
    ]
    .iter()
    .any(|w| p.contains(w))
}

/// The agent's short name, for button labels.
fn agent_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "Claude",
        AgentKind::Droid => "Droid",
        AgentKind::OpenCode => "OpenCode",
        AgentKind::Codex => "Codex",
        AgentKind::Devin => "Devin",
        AgentKind::Cursor => "Cursor",
        AgentKind::Antigravity => "Antigravity",
        AgentKind::WorkersAi => "Workers AI",
    }
}

/// What a DNS record routes to, when that's worth a note; `true` when it's a problem.
fn record_note(s: &Snapshot, r: &DnsRecord) -> Option<(String, bool)> {
    if let Some(id) = r.tunnel() {
        return Some(match s.tunnels.iter().find(|t| t.id == id) {
            Some(t) => (format!("Tunnel {} · {}", t.name, t.status), !t.up()),
            None if s.tunnels_read => ("deleted Tunnel".into(), true),
            None => ("Tunnel".into(), false),
        });
    }
    r.exposes_origin()
        .then(|| ("origin IP visible (not proxied)".into(), false))
}

fn dns_row(s: &Snapshot, r: &DnsRecord) -> Div {
    let note = record_note(s, r);
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(22.))
        .text_size(px(ui::TEXT_XS))
        .child(
            div()
                .flex_none()
                .w(px(44.))
                .child(ui::tag(r.kind.clone(), theme::mix(0.6))),
        )
        .child(
            div()
                .flex_none()
                .max_w(px(260.))
                .truncate()
                .font_family(theme::FONT_MONO)
                .text_color(theme::content())
                .child(r.name.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(theme::FONT_MONO)
                .text_color(theme::text_subtle())
                .child(format!("→ {}", crate::redact::text(&r.content))),
        )
        .when(r.proxied, |d| d.child(ui::tag("proxied", theme::accent())))
        .when_some(note, |d, (text, bad)| {
            d.child(ui::tag(
                text,
                if bad {
                    theme::danger().into()
                } else {
                    theme::mix(0.5)
                },
            ))
        })
}

fn good() -> Hsla {
    theme::success().into()
}

/// 1xx..5xx by index 0..4.
fn class_color(i: usize) -> Hsla {
    match i {
        1 => good(),
        2 => theme::accent(),
        3 => theme::warn().into(),
        4 => theme::danger().into(),
        _ => theme::mix(0.35),
    }
}

fn status_color(code: u16) -> Hsla {
    class_color(usize::from(code / 100).saturating_sub(1))
}

/// 6 of 2000 → "0.3%", with "<0.1%" for shares too small to round.
fn pct(part: u64, whole: u64) -> String {
    if whole == 0 || part == 0 {
        return "0%".into();
    }
    let p = part as f64 * 100. / whole as f64;
    if p < 0.1 {
        "<0.1%".into()
    } else if p < 10. {
        format!("{}%", format!("{p:.1}").trim_end_matches(".0"))
    } else {
        format!("{p:.0}%")
    }
}

/// The value, or a dash when the analytics it comes from couldn't be read.
fn known(ok: bool, value: String) -> String {
    if ok { value } else { "—".into() }
}

/// 1 → "1 gateway", 3 → "3 gateways".
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn dollars(v: f64) -> String {
    if v < 0.01 {
        "<$0.01".into()
    } else {
        format!("${v:.2}")
    }
}

fn list_or(v: &[String], none: &str) -> String {
    if v.is_empty() {
        none.into()
    } else {
        v.join(", ")
    }
}

fn outline(b: Stateful<Div>) -> Stateful<Div> {
    b.border_1().border_color(theme::border_strong())
}

fn card(title: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(crate::ui::sp(12.))
        .p(crate::ui::sp(16.))
        .rounded(crate::ui::rad(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::stroke())
        .child(
            div()
                .text_size(px(ui::TEXT_MD))
                .font_weight(FontWeight::MEDIUM)
                .child(title),
        )
}

fn metric(label: &'static str, value: String, color: Option<Hsla>, sub: String) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(crate::ui::sp(4.))
        .p(crate::ui::sp(12.))
        .rounded(crate::ui::rad(ui::RADIUS_LG))
        .bg(theme::fill_raised())
        .child(
            div()
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_subtle())
                .child(label),
        )
        .child(
            div()
                .text_size(px(ui::TEXT_TITLE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color.unwrap_or_else(theme::content))
                .child(value),
        )
        .child(
            div()
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_subtle())
                .truncate()
                .child(sub),
        )
}

fn section(title: &'static str, count: Option<String>, action: Option<AnyElement>) -> Div {
    div().flex().flex_col().gap(crate::ui::sp(8.)).child(
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .h(px(ui::BUTTON_H))
            .text_size(px(ui::TEXT_SM))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text_subtle())
            .child(div().flex_none().child(title))
            .children(count.map(|c| div().text_color(theme::text_faint()).truncate().child(c)))
            .child(div().flex_1())
            .children(action),
    )
}

fn row() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(6.))
        .px(crate::ui::sp(12.))
        .py(crate::ui::sp(10.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .bg(theme::fill_raised())
}

fn line_item(icon_name: &str, title: String, sub: String, when: String) -> Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(10.))
        .px(crate::ui::sp(12.))
        .py(crate::ui::sp(8.))
        .rounded(crate::ui::rad(8.))
        .bg(theme::fill_raised())
        .child(icon(icon_name, 14.).text_color(theme::text_subtle()))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(crate::ui::sp(2.))
                .child(div().text_size(px(ui::TEXT_MD)).truncate().child(title))
                .when(!sub.is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .truncate()
                            .child(sub),
                    )
                }),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_subtle())
                .child(when),
        )
}

fn meta_line(text: String) -> Div {
    div()
        .text_size(px(ui::TEXT_XS))
        .text_color(theme::text_subtle())
        .truncate()
        .child(text)
}

/// A binding: its kind, then what it points at.
fn chip(kind: &str, target: &str) -> Div {
    div()
        .flex()
        .items_center()
        .flex_none()
        .max_w(px(320.))
        .gap(crate::ui::sp(4.))
        .h(px(20.))
        .px(crate::ui::sp(6.))
        .rounded(crate::ui::rad(ui::RADIUS_SM))
        .bg(theme::fill_hover())
        .text_size(px(ui::TEXT_XS))
        .child(
            div()
                .flex_none()
                .text_color(theme::text_subtle())
                .font_weight(FontWeight::MEDIUM)
                .child(kind.to_string()),
        )
        .when(!target.eq_ignore_ascii_case(kind), |d| {
            d.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text_muted())
                    .child(target.to_string()),
            )
        })
}

fn quiet(text: impl Into<SharedString>) -> Div {
    div()
        .px(crate::ui::sp(2.))
        .py(crate::ui::sp(4.))
        .text_size(px(ui::TEXT_SM))
        .text_color(theme::text_subtle())
        .child(text.into())
}

fn note(text: impl Into<SharedString>, error: bool) -> Div {
    div()
        .text_size(px(ui::TEXT_XS))
        .text_color(if error {
            theme::danger().into()
        } else {
            theme::mix(0.6)
        })
        .child(text.into())
}

fn stat(label: &'static str, value: String) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(2.))
        .child(
            div()
                .text_size(px(ui::TEXT_MD))
                .font_weight(FontWeight::MEDIUM)
                .child(value),
        )
        .child(
            div()
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_subtle())
                .child(label),
        )
}

/// Requests per day as bars.
fn spark(days: &[Day]) -> Div {
    let max = days.iter().map(|d| d.requests).max().unwrap_or(0).max(1) as f32;
    div()
        .flex()
        .flex_none()
        .items_end()
        .gap(crate::ui::sp(2.))
        .h(px(30.))
        .w(px(76.))
        .children(days.iter().map(|d| {
            div()
                .w(px(7.))
                .h(px(2. + 28. * d.requests as f32 / max))
                .rounded(crate::ui::rad(4.))
                .bg(theme::mix(0.3))
        }))
}

/// A thin bar filled to `fraction`.
fn meter(fraction: f32, color: Hsla) -> Div {
    div()
        .h(px(4.))
        .w_full()
        .rounded(crate::ui::rad(4.))
        .bg(theme::fill_hover())
        .child(
            div()
                .h_full()
                .min_w(px(2.))
                .rounded(crate::ui::rad(4.))
                .w(relative(fraction.clamp(0., 1.)))
                .bg(color),
        )
}

/// The day's responses split by status class.
fn share_bar(classes: &[u64; 5], total: u64) -> Div {
    div()
        .flex()
        .gap(crate::ui::sp(2.))
        .h(px(6.))
        .w_full()
        .children((0..5).filter(|&i| classes[i] > 0).map(|i| {
            div()
                .h_full()
                .min_w(px(3.))
                .rounded(crate::ui::rad(4.))
                .w(relative(classes[i] as f32 / total as f32))
                .bg(class_color(i))
        }))
}

impl Render for CloudflareView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The width this view got last frame decides the layout of this one.
        let (cell, id) = (self.width.clone(), cx.entity_id());
        let measure = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, _: &mut Window, cx: &mut App| {
                let now = bounds.size.width.as_f32();
                if (now - cell.get()).abs() > 0.5 {
                    cell.set(now);
                    // A notify while the window paints marks no new frame, so
                    // send it once this one is drawn.
                    cx.defer(move |cx| cx.notify(id));
                }
            },
        )
        .absolute()
        .size_full();
        let now = crate::model::unix_now();
        let header = self.render_header(now, cx);
        let problems = self
            .snap
            .as_ref()
            .map(|s| s.problems.clone())
            .unwrap_or_default();
        // Parts the sign-in has no permission for go in one line with the fix.
        let (denied, failed): (Vec<String>, Vec<String>) =
            problems.into_iter().partition(|p| access_denied(p));
        // The analytics rate limit passes by itself: a quiet line, and a reload is on its way.
        let (waiting, failed): (Vec<String>, Vec<String>) =
            failed.into_iter().partition(|p| rate_limited(p));
        let waiting = (!waiting.is_empty()).then(|| {
            let parts: Vec<&str> = waiting
                .iter()
                .map(|p| p.split(':').next().unwrap_or_default())
                .collect();
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_subtle())
                .child(icon("clock01", 12.).text_color(theme::text_subtle()))
                .child(format!(
                    "{} paused by Cloudflare's rate limit; reloading in about 5 minutes.",
                    parts.join(" and ")
                ))
        });
        let access = (!denied.is_empty()).then(|| {
            let parts: Vec<&str> = denied
                .iter()
                .map(|p| p.split(':').next().unwrap_or_default())
                .collect();
            let command =
                cloudflare::access_command(self.snap.as_ref().map_or("", |s| s.profile.as_str()));
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(10.))
                .px(crate::ui::sp(12.))
                .py(crate::ui::sp(8.))
                .rounded(crate::ui::rad(ui::RADIUS_LG))
                .bg(Hsla::from(theme::warn()).opacity(0.1))
                .child(icon("square-lock02", 14.).text_color(theme::warn()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(ui::TEXT_SM))
                        .child(format!("The sign-in can't read {} yet.", parts.join(", "))),
                )
                .child(
                    outline(ui::button(
                        "cf-grant-top",
                        "Grant read access",
                        ButtonKind::Outline,
                        true,
                    ))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(CloudflareEvent::Run(command.clone()))
                    })),
                )
        });
        let lay = self.layout();
        let content = match self.snap.clone() {
            Some(s) => div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("cloudflare-body")
                        .track_scroll(&self.scroll)
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(crate::ui::sp(16.))
                                .w_full()
                                .max_w(px(PAGE_MAX))
                                .mx_auto()
                                .px(crate::ui::sp(page_pad(lay.narrow)))
                                .py(crate::ui::sp(24.))
                                .children(access)
                                .children(waiting)
                                .children(
                                    failed
                                        .into_iter()
                                        .map(|p| note(format!("Couldn't read {p}"), true)),
                                )
                                .child(self.render_body(&s, cx)),
                        ),
                )
                .into_any_element(),
            None => div()
                .id("cloudflare-body")
                .flex_1()
                .overflow_y_scroll()
                .child(
                    div()
                        .w_full()
                        .max_w(px(980.))
                        .mx_auto()
                        .p(crate::ui::sp(ui::PAGE_PAD))
                        .child(self.render_state(cx)),
                )
                .into_any_element(),
        };
        div()
            .track_focus(&self.focus)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(header)
            .child(content)
            .child(measure)
    }
}

#[cfg(test)]
mod tests {
    use super::alerts;
    use crate::cloudflare::{Cert, Snapshot, Worker, Zone};

    #[test]
    fn alert_keys_ignore_counts() {
        let now = crate::model::unix_now();
        let z = Zone {
            name: "a.dev".into(),
            certs: vec![Cert {
                hosts: vec!["a.dev".into()],
                expires: now + 5 * 86_400,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut s = Snapshot {
            workers: vec![Worker {
                name: "blog".into(),
                errors: 36,
                ..Default::default()
            }],
            zones: vec![z],
            ..Default::default()
        };
        let before: Vec<String> = alerts(&s).into_iter().map(|a| a.0).collect();
        s.workers[0].errors = 40;
        let after: Vec<String> = alerts(&s).into_iter().map(|a| a.0).collect();
        assert_eq!(before, after);
        assert!(before.iter().any(|k| k.starts_with("cert/a.dev")));
    }

    use super::*;

    #[test]
    fn the_layout_follows_the_width() {
        // A small window: less room at the sides, the page keeps its width.
        let l = layout(530.);
        assert!(l.narrow);
        assert_eq!(l.content, 530. - 2. * l.pad);
        // A wide one: the page capped.
        let l = layout(2400.);
        assert!(!l.narrow);
        assert_eq!(l.content, PAGE_MAX - 2. * l.pad);
        assert_eq!(layout(0.).content, 0.);
    }

    #[test]
    fn shares() {
        assert_eq!(
            (
                pct(0, 10),
                pct(6, 2000),
                pct(1, 3),
                pct(1446, 4034),
                pct(1, 2000)
            ),
            (
                "0%".into(),
                "0.3%".into(),
                "33%".into(),
                "36%".into(),
                "<0.1%".into()
            )
        );
        assert_eq!(pct(2, 100), "2%");
        assert_eq!(
            (dollars(0.004), dollars(1.5)),
            ("<$0.01".into(), "$1.50".into())
        );
    }
}
