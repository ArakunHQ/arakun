//! The Marketplace tab. Discover lists the plugins and MCP servers there are to
//! add (`market`). Installed shows what the agents load in the visible
//! environment, one row per item for every agent: Claude Code's MCP servers
//! with their health and scope, plugins, skills and permission rules
//! (`agent::claude_ext`), and Droid's MCP servers, plugins and skills
//! (`agent::extensions`, `agent::plugins`). Show for picks whose state the
//! cells say and never changes the layout (`agents`). Arakun's own extensions,
//! tabs the sidebar shows only once they are on here, have a page of their own
//! (`sidebar::Extensions`).

use std::collections::HashSet;
use std::convert::Infallible;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, Entity, EventEmitter, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    Window, div, px, relative, rgb,
};

use crate::agent::claude_ext::{self, ClaudeExtensions, Health, RuleKind, Scope, Server};
use crate::agent::extensions::{self, Extensions, McpServer};
use crate::agent::plugins::{self, Installed, Plugins, Runtime};
use crate::input::{InputEvent, TextInput};
use crate::loadable::Loadable;
use crate::logos;
use crate::sidebar::{self, View};
use crate::subnav::{Entry, Lead, Line};
use crate::theme;
use crate::ui::Typography as _;
use crate::ui::{self, ButtonKind, Tone, Type, icon};

mod agents;
mod market;
mod remote;
mod widget_index;
mod widgets;

use agents::{Agent, Show, Use, Uses};
use market::{Market, MarketState};
pub use remote::ServerTarget;

/// Reopening the tab rereads what is older than this.
const STALE: Duration = Duration::from_secs(5 * 60);
/// While a sign-in may be running in a terminal, reread this often...
const WATCH_EVERY: Duration = Duration::from_secs(12);
/// ...for this long.
const WATCH_FOR: Duration = Duration::from_secs(4 * 60);
/// Rows a long group shows before "Show all".
const FOLD: usize = 8;

pub enum ExtensionsEvent {
    /// A command to run in a terminal of the visible environment.
    Run(String),
    /// A file to show beside the chat.
    Open(PathBuf),
    /// An extension was turned on or off: the sidebar shows it, or not.
    Toggled(View, bool),
    /// Connect to the paired servers, if that has not happened yet: Discover
    /// is about to install on one.
    ConnectServers,
    /// A message for the paired server with this id (the phone's `ClientMsg`).
    Server(String, serde_json::Value),
    /// Something changed on this computer: the paired servers get what they lack.
    Mirror,
}

/// What the page lists.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Servers,
    Plugins,
    Skills,
    Permissions,
    /// Plugins and MCP servers to add.
    Marketplace,
    /// Arakun's own extensions: tabs for the sidebar.
    Arakun,
}

/// A way out for a server that needs you.
#[derive(Clone, Copy, PartialEq)]
enum Fix {
    SignIn,
    Connect,
    /// Drop the rejected token and sign in with OAuth.
    UseSignIn,
    ReplaceToken,
    Approve,
    RunCommand,
    Retry,
    Remove,
}

impl Fix {
    fn label(self) -> &'static str {
        match self {
            Fix::SignIn => "Sign in",
            Fix::Connect => "Connect",
            Fix::UseSignIn => "Sign in with your account",
            Fix::ReplaceToken => "Replace token",
            Fix::Approve => "Approve in Claude",
            Fix::RunCommand => "Run in terminal",
            Fix::Retry => "Check again",
            Fix::Remove => "Remove",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Fix::SignIn | Fix::Connect | Fix::UseSignIn => "user",
            Fix::ReplaceToken => "square-lock02",
            Fix::Approve => "checkmark-circle02",
            Fix::RunCommand => "computer-terminal01",
            Fix::Retry => "refresh01",
            Fix::Remove => "delete02",
        }
    }
}

/// Why a server needs you, and what to do about it (best first).
fn diagnose(s: &Server) -> Option<(String, Vec<Fix>)> {
    let token = s.token_header().is_some();
    let mut fixes = Vec::new();
    let why = match &s.health {
        Health::Connected => return None,
        Health::Failed(state) => match &s.diagnosis {
            Some(d) if matches!(d.status, 401 | 403) && token => {
                if d.oauth && s.editable() {
                    fixes.push(Fix::UseSignIn);
                }
                if s.editable() {
                    fixes.push(Fix::ReplaceToken);
                }
                let mut why = d.explain(true);
                if d.oauth {
                    why.push_str(
                        " It also supports signing in with your account, which renews itself.",
                    );
                }
                why
            }
            Some(d) if matches!(d.status, 401 | 403) => {
                fixes.push(Fix::SignIn);
                d.explain(false)
            }
            Some(d) => {
                fixes.push(Fix::Retry);
                d.explain(token)
            }
            None if s.transport == "stdio" => {
                if s.runnable() {
                    fixes.push(Fix::RunCommand);
                }
                fixes.push(Fix::Retry);
                format!(
                    "{state}: its command exits before Claude can talk to it. Run it in a terminal to see its error."
                )
            }
            None => {
                fixes.push(Fix::SignIn);
                fixes.push(Fix::Retry);
                format!("{state}. Check the URL, or sign in again.")
            }
        },
        Health::NeedsAuth if s.unused_connector() => {
            fixes.push(Fix::Connect);
            "A connector of your claude.ai account. Connect it to give Claude its tools in every project.".into()
        }
        Health::NeedsAuth => {
            fixes.push(Fix::SignIn);
            "Its sign-in is missing or expired. Sign in once and Claude keeps it fresh.".into()
        }
        Health::Pending => {
            fixes.push(Fix::Approve);
            "The repo's .mcp.json shares it and nobody approved it yet. Claude asks when it starts in this folder.".into()
        }
        Health::Off => {
            "Turned off in this folder. Turn it back on from /mcp in Claude Code.".into()
        }
        Health::Unknown => {
            fixes.push(Fix::Retry);
            "Not checked: claude mcp list did not answer in time.".into()
        }
    };
    if s.editable() && !matches!(s.health, Health::Off | Health::Unknown) {
        fixes.push(Fix::Remove);
    }
    Some((why, fixes))
}

#[derive(Clone, PartialEq)]
enum Busy {
    Server(String),
    Skill(String),
}

pub struct ExtensionsView {
    cwd: Option<PathBuf>,
    /// Whose state the cells say. It never changes which sections show.
    show: Show,
    kind: Kind,
    market: Market,
    ms: MarketState,
    /// Community widgets in Discover.
    ws: widgets::WidgetState,
    query: Entity<TextInput>,
    scroll: ScrollHandle,
    error: Option<SharedString>,
    /// The row whose details are open (`mcp:<name>`, `plugin:<id>`).
    open: Option<String>,
    /// Groups showing every row (`skills:<source>`, `rules`).
    unfolded: HashSet<String>,

    /// What Claude Code loads here. Reading it has no error of its own: a
    /// server that fails shows so in the answer.
    claude: Loadable<ClaudeExtensions, Infallible>,
    claude_at: Option<Instant>,
    plugin_busy: Option<String>,
    /// The server a fix is running on.
    op: Option<String>,
    /// The server whose new token is being typed.
    token_for: Option<String>,
    token: Entity<TextInput>,
    /// The server whose Remove was clicked once.
    confirm_remove: Option<String>,
    notice: Option<SharedString>,
    /// Until when to reread now and then: a sign-in runs in a terminal.
    watch_until: Option<Instant>,
    _token: Subscription,
    _poll: Task<()>,

    droid: Option<Extensions>,
    droid_at: Option<Instant>,
    droid_error: Option<SharedString>,
    droid_busy: Option<Busy>,
    /// Droid isn't installed: nothing is in it, and that is no error.
    droid_absent: bool,
    plugins: Option<Plugins>,
    _droid_task: Option<Task<()>>,
    _plugins_task: Option<Task<()>>,
    _query: Subscription,

    /// The extensions turned on, which the sidebar shows.
    views: sidebar::Extensions,
    /// Where Discover installs, and what the paired servers said they hold.
    targets: remote::Targets,
}

impl EventEmitter<ExtensionsEvent> for ExtensionsView {}

impl ExtensionsView {
    pub fn new(views: sidebar::Extensions, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextInput::new("Search plugins, MCP servers and skills…", cx));
        let sub = cx.subscribe(&query, |this, input, ev: &InputEvent, cx| match ev {
            InputEvent::Changed => {
                this.query_changed(cx);
                cx.notify()
            }
            InputEvent::Cancel => input.update(cx, |i, cx| i.set_text("", cx)),
            _ => {}
        });
        let token = cx.new(|cx| TextInput::new("Paste the new token…", cx).secret());
        let token_sub = cx.subscribe(&token, |this, _, ev: &InputEvent, cx| match ev {
            InputEvent::Submit => this.save_token(cx),
            InputEvent::Cancel => {
                this.token_for = None;
                cx.notify();
            }
            _ => {}
        });
        let poll = crate::poll::poll_every(cx, WATCH_EVERY, WATCH_EVERY, |this, cx| {
            let watching = this.watch_until.is_some_and(|t| Instant::now() < t);
            if watching && !this.claude.loading() && this.op.is_none() {
                this.load_claude(cx);
            }
        });
        Self {
            cwd: None,
            show: Show::All,
            kind: Kind::Marketplace,
            market: Market::Plugins,
            ms: MarketState::default(),
            ws: Default::default(),
            query,
            scroll: ScrollHandle::new(),
            error: None,
            open: None,
            unfolded: HashSet::new(),
            claude: Loadable::default(),
            claude_at: None,
            plugin_busy: None,
            op: None,
            token_for: None,
            token,
            confirm_remove: None,
            notice: None,
            watch_until: None,
            _token: token_sub,
            _poll: poll,
            droid: None,
            droid_at: None,
            droid_error: None,
            droid_busy: None,
            droid_absent: false,
            plugins: None,
            _droid_task: None,
            _plugins_task: None,
            _query: sub,
            views,
            targets: remote::Targets::default(),
        }
    }

    /// Whether the sidebar shows `view`: the core always, an extension once on.
    pub fn shows(&self, view: View) -> bool {
        self.views.shows(view)
    }

    /// Opens the page of Arakun's own extensions.
    pub fn show_extensions(&mut self, cx: &mut Context<Self>) {
        self.set_kind(Kind::Arakun, cx);
    }

    /// Opens Claude Code's installed MCP servers, plugins or skills, where the
    /// Usage tab sends a finding about them.
    pub fn show_claude(&mut self, place: &crate::usage_where::Place, cx: &mut Context<Self>) {
        use crate::usage_where::Place;
        let kind = match place {
            Place::Servers => Kind::Servers,
            Place::Plugins => Kind::Plugins,
            Place::Skills => Kind::Skills,
            Place::File(_) | Place::Nothing => return,
        };
        // The finding is about Claude Code: its state, not Droid's.
        if self.show != Show::All {
            self.show = Show::Only(Agent::Claude);
        }
        self.set_kind(kind, cx);
    }

    /// Turns an extension tab on or off, as its switch here does.
    pub fn set_view(&mut self, view: View, on: bool, cx: &mut Context<Self>) {
        if self.views.set(view, on) {
            self.views.save();
            cx.emit(ExtensionsEvent::Toggled(view, on));
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.query.focus_handle(cx), cx);
    }

    /// Shows what the agents load in `cwd`; a new folder rereads everything.
    pub fn set_dir(&mut self, cwd: PathBuf, cx: &mut Context<Self>) {
        if self.cwd.as_ref() == Some(&cwd) {
            return;
        }
        self.cwd = Some(cwd);
        self.ms.repo_has = None;
        self.load_repo_has(cx);
        // The last folder's read would be dropped when it came; this one starts now.
        self.claude.reset();
        self.claude_at = None;
        self.droid = None;
        self.droid_at = None;
        self.open = None;
        self.error = None;
        cx.notify();
    }

    /// MCP servers of Claude Code failing or waiting for a sign-in, as last read.
    pub fn problems(&self) -> usize {
        self.claude
            .data
            .as_ref()
            .map_or(0, ClaudeExtensions::problems)
    }

    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        let stale = |at: Option<Instant>| at.is_none_or(|t| t.elapsed() > STALE);
        // Every agent's, whichever Show for picks: each row says how every
        // agent stands with it.
        if stale(self.claude_at) && !self.claude.loading() {
            self.load_claude(cx)
        }
        if stale(self.droid_at) && self.droid_busy.is_none() && self._droid_task.is_none() {
            self.load_droid(cx)
        }
        if self.kind == Kind::Marketplace {
            self.open_market(cx);
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.kind == Kind::Marketplace {
            self.ask_remote(true, cx);
            // Asked for, a failed load is tried again at once.
            self.ms.failed_at = None;
            // A failed skills.sh search too, for the same words.
            self.ms.skills.error = None;
            // And the widget index.
            self.ws.cards.error = None;
            self.open_market(cx);
        }
        self.load_claude(cx);
        self.load_droid(cx);
    }

    fn load_claude(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        let first = self.claude.data.is_none();
        if let Some(c) = &mut self.claude.data {
            c.checking = true;
        }
        // Asked again (a fix ran, Retry), the read on its way is old: start over.
        self.claude.cancel();
        let dir = cwd.clone();
        self.claude.start_streaming(
            cx,
            |this| &mut this.claude,
            // The files and the last check first, at once; the slow check after.
            move |early| async move {
                if first {
                    let quick =
                        crate::guard::task_or_default(async { claude_ext::load_quick(&dir) }).await;
                    let _ = early.unbounded_send(quick);
                }
                Some(claude_ext::load(&dir))
            },
            {
                let cwd = cwd.clone();
                move |this, quick, _| {
                    if this.cwd.as_ref() == Some(&cwd) && this.claude.data.is_none() {
                        this.claude.data = Some(quick);
                    }
                }
            },
            move |this, found, cx| {
                if this.cwd.as_ref() != Some(&cwd) {
                    return;
                }
                // A panic in the check shows nothing found, as a panic in the
                // quick read does.
                let found = found.unwrap_or_default();
                // A sign-in finished while watching (it runs in a terminal):
                // the servers get it now, not at the next install.
                let waiting = |c: &ClaudeExtensions| {
                    c.servers
                        .iter()
                        .filter(|s| s.health == claude_ext::Health::NeedsAuth)
                        .count()
                };
                let signed_in = this.claude.data.as_ref().map(waiting) > Some(waiting(&found));
                this.claude.data = Some(found);
                if signed_in {
                    cx.emit(ExtensionsEvent::Mirror);
                }
                this.claude_at = Some(Instant::now());
            },
        );
    }

    /// Rereads Claude's files only, keeping the health already checked.
    fn reload_files(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        cx.spawn(async move |this, cx| {
            let dir = cwd.clone();
            let mut quick = cx
                .background_spawn(crate::guard::task_or_default(async move {
                    claude_ext::load_quick(&dir)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.cwd.as_ref() != Some(&cwd) {
                    return;
                }
                quick.checking = this.claude.loading();
                this.claude.data = Some(quick);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_plugin(&mut self, id: String, enabled: bool, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        if self.plugin_busy.is_some() {
            return;
        }
        self.plugin_busy = Some(id.clone());
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    claude_ext::set_plugin(&cwd, &id, enabled)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.plugin_busy = None;
                match result {
                    // A plugin's switch is in the files: no need to check every server again.
                    Ok(()) => this.reload_files(cx),
                    Err(e) => {
                        this.error = Some(format!("Could not change the plugin: {e:#}").into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Runs a fix on `name` in the background, rereads, then runs `then` in a terminal.
    fn run_op(
        &mut self,
        name: String,
        done: impl Into<String>,
        job: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        then: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.op.is_some() {
            return;
        }
        self.op = Some(name.clone());
        self.error = None;
        self.notice = None;
        self.confirm_remove = None;
        let done = done.into();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(crate::guard::task(async move { job() }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.op = None;
                match result {
                    Ok(()) => {
                        this.notice = Some(format!("{} {done}.", display_name(&name)).into());
                        this.token_for = None;
                        if let Some(command) = then {
                            this.watch();
                            cx.emit(ExtensionsEvent::Run(command));
                        }
                        this.load_claude(cx);
                        this.load_repo_has(cx);
                        cx.emit(ExtensionsEvent::Mirror);
                    }
                    Err(e) => {
                        this.error = Some(format!("{}: {e:#}", display_name(&name)).into());
                        // A job that did part of its work (a package with one
                        // skill that failed) still changed what is installed.
                        this.load_claude(cx);
                        this.load_repo_has(cx);
                        cx.emit(ExtensionsEvent::Mirror);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn watch(&mut self) {
        self.watch_until = Some(Instant::now() + WATCH_FOR);
    }

    /// Runs a command in a terminal that may change what Claude reaches.
    fn run_and_watch(&mut self, command: String, cx: &mut Context<Self>) {
        self.watch();
        cx.emit(ExtensionsEvent::Run(command));
    }

    fn fix(&mut self, s: &Server, action: Fix, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        let (name, scope) = (s.name.clone(), s.scope.clone());
        match action {
            Fix::SignIn | Fix::Connect => self.run_and_watch(claude_ext::login_command(&name), cx),
            Fix::Approve => self.run_and_watch("claude".into(), cx),
            Fix::RunCommand => cx.emit(ExtensionsEvent::Run(s.target.clone())),
            Fix::Retry => self.load_claude(cx),
            Fix::UseSignIn => {
                let header = s.token_header().unwrap_or("Authorization").to_string();
                let login = claude_ext::login_command(&name);
                let n = name.clone();
                self.run_op(
                    name,
                    "now signs in with your account",
                    move || claude_ext::use_sign_in(&cwd, &n, &scope, &header),
                    Some(login),
                    cx,
                );
            }
            Fix::ReplaceToken => {
                self.token_for = Some(name);
                self.token.update(cx, |i, cx| i.set_text("", cx));
                window.focus(&self.token.focus_handle(cx), cx);
                cx.notify();
            }
            Fix::Remove => {
                if self.confirm_remove.as_ref() != Some(&name) {
                    self.confirm_remove = Some(name);
                    cx.notify();
                    return;
                }
                let n = name.clone();
                self.run_op(
                    name,
                    "was removed",
                    move || claude_ext::remove(&cwd, &n, &scope),
                    None,
                    cx,
                );
            }
        }
    }

    fn save_token(&mut self, cx: &mut Context<Self>) {
        let (Some(cwd), Some(name)) = (self.cwd.clone(), self.token_for.clone()) else {
            return;
        };
        let Some(s) = self
            .claude
            .data
            .as_ref()
            .and_then(|x| x.servers.iter().find(|s| s.name == name))
        else {
            return;
        };
        let (scope, header) = (
            s.scope.clone(),
            s.token_header().unwrap_or("Authorization").to_string(),
        );
        let token = self.token.read(cx).text().to_string();
        self.token.update(cx, |i, cx| i.set_text("", cx));
        let n = name.clone();
        self.run_op(
            name,
            "has its new token",
            move || claude_ext::replace_token(&cwd, &n, &scope, &header, &token),
            None,
            cx,
        );
    }

    fn load_droid(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        self.droid_absent = !extensions::installed();
        if self.droid_absent {
            self.droid = Some(Extensions::default());
            self.droid_error = None;
            self.droid_at = Some(Instant::now());
        } else {
            // What the last look found shows until the new one answers.
            if self.droid.is_none() {
                self.droid = extensions::cached(&cwd);
            }
            self.run_droid(None, move || extensions::load(&cwd), cx);
        }
        self._plugins_task = Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(crate::guard::task_or_default(
                    async move { plugins::load() },
                ))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.plugins = Some(found);
                cx.notify();
            });
        }));
    }

    fn run_droid(
        &mut self,
        busy: Option<Busy>,
        job: impl FnOnce() -> anyhow::Result<Extensions> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.droid_busy = busy;
        self.droid_error = None;
        self._droid_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(crate::guard::task(async move { job() }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.droid_busy = None;
                this._droid_task = None;
                this.droid_at = Some(Instant::now());
                match result {
                    Ok(data) => this.droid = Some(data),
                    Err(e) => this.droid_error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn toggle_droid_server(&mut self, name: String, enabled: bool, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        if self.droid_busy.is_some() {
            return;
        }
        let n = name.clone();
        self.run_droid(
            Some(Busy::Server(name)),
            move || extensions::set_server(&cwd, &n, enabled),
            cx,
        );
    }

    fn toggle_droid_skill(&mut self, name: String, enabled: bool, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else { return };
        if self.droid_busy.is_some() {
            return;
        }
        let n = name.clone();
        self.run_droid(
            Some(Busy::Skill(name)),
            move || extensions::set_skill(&cwd, &n, enabled),
            cx,
        );
    }

    fn install_seo(&mut self, cx: &mut Context<Self>) {
        if cfg!(windows) {
            self.error = Some(
                "Arakun installs Claude SEO with a POSIX shell script; on Windows follow the plugin's README (github.com/AgriciDaniel/claude-seo)."
                    .into(),
            );
            cx.notify();
            return;
        }
        let claude = self.plugins.as_ref().is_some_and(|p| p.claude_cli);
        match plugins::write_seo_script(&crate::state::data_dir(), claude) {
            Ok(command) => cx.emit(ExtensionsEvent::Run(command)),
            Err(e) => {
                self.error = Some(format!("Could not write the install script: {e}").into());
                cx.notify();
            }
        }
    }

    fn toggle_open(&mut self, key: String, cx: &mut Context<Self>) {
        self.open = if self.open.as_ref() == Some(&key) {
            None
        } else {
            Some(key)
        };
        cx.notify();
    }

    fn words(&self, cx: &App) -> Vec<String> {
        self.query
            .read(cx)
            .text()
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    // -----------------------------------------------------------------------
    // Claude Code.

    /// Installed: one row per item, whichever agents have it. Show for changes
    /// what the cells say, never which sections show.
    fn render_installed(&self, x: &ClaudeExtensions, cx: &mut Context<Self>) -> Div {
        let words = self.words(cx);
        let shown = |hay: &str| {
            let hay = hay.to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        };
        let filtering = !words.is_empty();
        let kind = self.kind;
        let wants = |k: Kind| kind == k;
        let mut body = div().flex().flex_col().gap(crate::ui::sp(28.));
        if !x.cli {
            body = body.child(note(
                "The claude command is not on your PATH, so this shows the config files only: no health, no plugins.",
                Tone::Warning,
            ));
        }
        if let Some(e) = &self.droid_error {
            body = body.child(note(format!("Droid: {e}"), Tone::Danger));
        }
        if self.droid_absent && self.show == Show::Only(Agent::Droid) {
            body = body.child(note(
                "Droid's CLI is not installed, so nothing here is in Droid yet.",
                Tone::Info,
            ));
        }
        let droid = self.droid.as_ref();
        let droid_servers: &[McpServer] = droid.map_or(&[], |d| &d.servers);
        let droid_skills: &[extensions::Skill] = droid.map_or(&[], |d| &d.skills);
        // Until Droid answers, its column would say "Not in Droid" for all.
        let asking = (self.droid.is_none() || self._droid_task.is_some()) && !self.droid_absent;
        let asking_aside = || {
            asking.then(|| {
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child(ui::spinner(12., theme::text_faint()))
                    .child("Asking Droid…")
            })
        };

        if wants(Kind::Servers) && !filtering {
            body = body.children(self.render_attention(x, cx));
        }

        if wants(Kind::Servers) {
            let (offered, used): (Vec<&Server>, Vec<&Server>) =
                x.servers.iter().partition(|s| s.unused_connector());
            let droid_refs: Vec<&McpServer> = droid_servers.iter().collect();
            let merged: Vec<_> = agents::merge(
                &used,
                |s| display_name(&s.name),
                &droid_refs,
                |d| d.name.clone(),
            )
            .into_iter()
            .filter(|(c, d)| {
                let mut hay = String::new();
                if let Some(s) = c {
                    hay += &format!(
                        "{} {} {} {} ",
                        s.name,
                        s.scope.label(),
                        s.target,
                        health_label(&s.health)
                    );
                }
                if let Some(d) = d {
                    hay += &format!("{} {} {}", d.name, d.source, d.status);
                }
                shown(&hay)
            })
            .collect();
            let offered: Vec<&Server> = offered
                .into_iter()
                .filter(|s| shown(&format!("{} {}", s.name, s.target)))
                .collect();
            if !merged.is_empty() || !offered.is_empty() || !filtering {
                let mut rows: Vec<AnyElement> = merged
                    .iter()
                    .map(|(c, d)| match c {
                        Some(s) => self.server_row(s, d.map(|d| &**d), cx).into_any_element(),
                        None => d
                            .map(|d| self.droid_server_row(d, cx).into_any_element())
                            .unwrap_or_else(|| div().into_any_element()),
                    })
                    .collect();
                if !offered.is_empty() {
                    let all = filtering || self.unfolded.contains("connectors");
                    rows.push(
                        group_row(
                            format!(
                                "claude.ai connectors you haven't connected · {}",
                                offered.len()
                            ),
                            (!all).then(|| {
                                self.show_all("connectors".into(), offered.len(), cx)
                                    .into_any_element()
                            }),
                        )
                        .into_any_element(),
                    );
                    if all {
                        rows.extend(
                            offered
                                .iter()
                                .map(|s| self.server_row(s, None, cx).into_any_element()),
                        );
                    }
                }
                if rows.is_empty() {
                    rows.push(
                        empty_row(
                            "No MCP servers. Add one with `claude mcp add` or `droid mcp add`.",
                        )
                        .into_any_element(),
                    );
                }
                let healthy = x
                    .servers
                    .iter()
                    .filter(|s| s.health == Health::Connected)
                    .count();
                let mine = x.servers.iter().filter(|s| !s.unused_connector()).count();
                let problems = x.problems();
                let aside = if x.checking {
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .ty(Type::Caption)
                        .text_color(theme::text_subtle())
                        .child(ui::spinner(12., theme::text_faint()))
                        .child("Checking connections…")
                        .into_any_element()
                } else if let Some(a) = asking_aside() {
                    a.into_any_element()
                } else if self.show == Show::Only(Agent::Droid) {
                    let on = droid_servers.iter().filter(|s| s.enabled()).count();
                    ui::status_dot(
                        format!("{on} of {} on for Droid", droid_servers.len()),
                        Tone::Success,
                    )
                    .into_any_element()
                } else if problems > 0 {
                    ui::status_dot(
                        format!(
                            "{problems} need{} you",
                            if problems == 1 { "s" } else { "" }
                        ),
                        Tone::Warning,
                    )
                    .into_any_element()
                } else {
                    ui::status_dot(format!("{healthy} of {mine} connected"), Tone::Success)
                        .into_any_element()
                };
                for w in &x.warnings {
                    rows.push(
                        div()
                            .px(crate::ui::sp(16.))
                            .py(crate::ui::sp(10.))
                            .border_t_1()
                            .border_color(theme::border_subtle())
                            .child(note(w.clone(), Tone::Warning))
                            .into_any_element(),
                    );
                }
                body = body.child(section(
                    "MCP servers",
                    "Tools your agents can call, and whether they reach them",
                    Some(aside),
                    table(&SERVER_COLS, rows),
                ));
            }
        }

        if wants(Kind::Plugins) {
            let droid_plugins: &[Installed] = self.plugins.as_ref().map_or(&[], |p| &p.droid);
            let mut merged: Vec<_> = agents::merge(
                &x.plugins,
                |p| p.short().to_string(),
                droid_plugins,
                |i| droid_short(&i.id).to_string(),
            );
            // Claude SEO has an install of its own, for both agents: it keeps
            // a row while neither has it.
            let seo = self.plugins.is_some()
                && !merged
                    .iter()
                    .any(|(c, d)| c.is_some_and(is_seo_claude) || d.is_some_and(is_seo_droid));
            merged.retain(|(c, d)| {
                let mut hay = String::new();
                if let Some(p) = c {
                    hay += &format!("{} {} ", p.id, p.servers.join(" "));
                }
                if let Some(i) = d {
                    hay += &i.id;
                }
                shown(&hay)
            });
            let seo = seo && shown(SEO_WORDS);
            if !merged.is_empty() || seo || !filtering {
                let mut rows: Vec<AnyElement> = merged
                    .iter()
                    .map(|(c, d)| self.plugin_row(*c, *d, cx).into_any_element())
                    .collect();
                if seo {
                    rows.push(self.seo_row(cx).into_any_element());
                }
                if rows.is_empty() {
                    rows.push(
                        empty_row("No plugins installed. Browse them under Discover.")
                            .into_any_element(),
                    );
                }
                let aside = match asking_aside().filter(|_| self.plugins.is_none()) {
                    Some(a) => a.into_any_element(),
                    None => {
                        let (on, all) = match self.show {
                            Show::Only(Agent::Droid) => (
                                droid_plugins.iter().filter(|p| p.enabled).count(),
                                droid_plugins.len(),
                            ),
                            _ => (
                                x.plugins.iter().filter(|p| p.enabled).count(),
                                x.plugins.len(),
                            ),
                        };
                        let whose = match self.show {
                            Show::All => String::new(),
                            Show::Only(a) => format!(" for {}", a.name()),
                        };
                        ui::status_dot(format!("{on} of {all} on{whose}"), Tone::Success)
                            .into_any_element()
                    }
                };
                body = body.child(section(
                    "Plugins",
                    "Bundles of skills, agents, commands and servers",
                    Some(aside),
                    table(&PLUGIN_COLS, rows),
                ));
            }
        }

        if wants(Kind::Skills) {
            let merged: Vec<_> = agents::merge(
                &x.skills,
                |s| skill_short(&s.name).to_string(),
                droid_skills,
                |s| s.name.clone(),
            )
            .into_iter()
            .filter(|(c, d)| {
                let mut hay = String::new();
                if let Some(s) = c {
                    hay += &format!("{} {} {} ", s.name, s.source, s.description);
                }
                if let Some(s) = d {
                    hay += &format!("{} {} {}", s.name, s.location, s.description);
                }
                shown(&hay)
            })
            .collect();
            if !merged.is_empty() || !filtering {
                let source =
                    |c: Option<&claude_ext::Skill>, d: Option<&extensions::Skill>| match (c, d) {
                        (Some(s), _) => s.source.clone(),
                        (None, Some(s)) if !s.location.is_empty() => s.location.clone(),
                        _ => "other".to_string(),
                    };
                let mut rows: Vec<AnyElement> = Vec::new();
                let mut sources: Vec<String> = Vec::new();
                for (c, d) in &merged {
                    let s = source(*c, *d);
                    if !sources.contains(&s) {
                        sources.push(s);
                    }
                }
                let mut n = 0;
                for src in sources {
                    let group: Vec<_> = merged
                        .iter()
                        .filter(|(c, d)| source(*c, *d) == src)
                        .collect();
                    let key = format!("skills:{src}");
                    let all = filtering || self.unfolded.contains(&key);
                    let fold = FOLD * 2;
                    rows.push(
                        group_row(
                            format!("{src} · {}", group.len()),
                            (!all && group.len() > fold).then(|| {
                                self.show_all(key.clone(), group.len(), cx)
                                    .into_any_element()
                            }),
                        )
                        .into_any_element(),
                    );
                    for (c, d) in group.iter().take(if all { usize::MAX } else { fold }) {
                        rows.push(self.skill_row(n, &src, *c, *d, cx).into_any_element());
                        n += 1;
                    }
                }
                if rows.is_empty() {
                    rows.push(empty_row("No skills. Personal ones live in ~/.claude/skills, project ones in .claude/skills.").into_any_element());
                }
                let aside = match asking_aside() {
                    Some(a) => a.into_any_element(),
                    None => {
                        let personal = x.skills.iter().filter(|s| s.source == "personal").count();
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(match self.show {
                                Show::Only(Agent::Droid) => format!(
                                    "{} of {} on for Droid",
                                    droid_skills.iter().filter(|s| s.enabled).count(),
                                    droid_skills.len()
                                ),
                                _ => format!(
                                    "{personal} personal · {} from plugins",
                                    x.skills.len() - personal
                                ),
                            })
                            .into_any_element()
                    }
                };
                body = body.child(section(
                    "Skills",
                    "Know-how your agents load when a task calls for it",
                    Some(aside),
                    table(&SKILL_COLS, rows),
                ));
            }
        }

        if wants(Kind::Permissions) {
            let rules: Vec<_> = x
                .permissions
                .rules
                .iter()
                .filter(|r| shown(&format!("{} {} {}", r.kind.label(), r.rule, r.source)))
                .collect();
            if !rules.is_empty() || !filtering {
                body = body.child(self.render_permissions(x, rules, filtering, cx));
            }
        }
        body
    }

    /// The Status cell: the picked agent's state, or the item's own.
    fn status_cell(&self, uses: &Uses, own: (&str, Tone)) -> Div {
        match agents::state_for(self.show, uses) {
            Some(words) => agents::pill(words, Tone::Neutral, true),
            None => agents::pill(own.0.to_string(), own.1, false),
        }
    }

    /// A switch that turns an item on or off for one agent, or its spinner.
    fn agent_switch(
        &self,
        id: String,
        label: String,
        on: bool,
        busy: bool,
        locked: bool,
        flip: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if busy || locked {
            return switch_or_spinner(on, busy, locked);
        }
        div()
            .id(SharedString::from(id))
            .map(|d| ui::as_switch(d, label, on))
            .cursor_pointer()
            .child(ui::switch(on))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                flip(this, !on, cx)
            }))
            .into_any_element()
    }

    /// Makes an Agents card switch out of a method of the view.
    fn toggle_for(
        &self,
        cx: &mut Context<Self>,
        flip: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
    ) -> Option<agents::Toggle> {
        let this = cx.entity().downgrade();
        Some(Box::new(move |on, _, cx| {
            let _ = this.update(cx, |this, cx| flip(this, on, cx));
        }))
    }

    /// Droid's line in an Agents card, for an item in its MCP config.
    fn droid_server_line(&self, d: Option<&McpServer>, cx: &mut Context<Self>) -> agents::Line {
        let uses = d.map_or(Use::Missing, |d| Use::on(d.enabled()));
        let toggle = d.filter(|d| !d.managed).and_then(|d| {
            let name = d.name.clone();
            self.toggle_for(cx, move |this, on, cx| {
                this.toggle_droid_server(name.clone(), on, cx)
            })
        });
        agents::Line {
            agent: Agent::Droid,
            uses,
            file: d.map_or_else(|| DROID_MCP.to_string(), |d| droid_server_file(&d.source)),
            toggle,
            busy: d.is_some_and(|d| self.droid_busy == Some(Busy::Server(d.name.clone()))),
        }
    }

    /// An MCP server only Droid has.
    fn droid_server_row(&self, s: &McpServer, cx: &mut Context<Self>) -> Div {
        let key = format!("droid-mcp:{}", s.name);
        let open = self.open.as_ref() == Some(&key);
        let on = s.enabled();
        let uses: Uses = [Use::Missing, Use::on(on)];
        let mut tags: Vec<(String, u32)> = vec![(s.server_type.to_uppercase(), CAT_BLUE)];
        if s.tools > 0 {
            tags.push((format!("{} tools", s.tools), CAT_GREEN));
        }
        if s.managed {
            tags.push(("managed".into(), CAT_AMBER));
        }
        let name = s.name.clone();
        let spinning = self.droid_busy == Some(Busy::Server(name.clone()));
        let switch = self.agent_switch(
            format!("droid-server-switch-{name}"),
            format!("{name} for Droid"),
            on,
            spinning,
            s.managed,
            move |this, on, cx| this.toggle_droid_server(name.clone(), on, cx),
            cx,
        );
        let header = row_base(SharedString::from(format!("ext-{key}")), ROW_H)
            .map(|d| ui::labelled(d, gpui::Role::Button, s.name.clone()))
            .when(open, |d| d.bg(theme::fill_hover()))
            .child(name_cell(
                logos::mark(&s.name, ""),
                32.,
                s.name.clone(),
                format!("{} · {}", s.server_type, s.source),
                !on,
            ))
            .child(cell(Some(STATUS_W)).child(self.status_cell(&uses, droid_server_state(s))))
            .child(cell(Some(AGENTS_W)).child(agents::cell(self.show, &uses)))
            .child(cell(Some(SOURCE_W)).child(ui::badge(s.source.clone(), Tone::Neutral)))
            .child(cell(Some(TAGS_W)).child(tag_list(&tags, 3)))
            .child(
                cell(Some(ACTIONS_W))
                    .justify_end()
                    .gap(crate::ui::sp(8.))
                    .child(switch)
                    .child(
                        icon(if open { "arrow-up01" } else { "arrow-down01" }, 14.)
                            .text_color(theme::text_faint()),
                    ),
            )
            .on_click({
                let key = key.clone();
                cx.listener(move |this, _, _, cx| this.toggle_open(key.clone(), cx))
            });
        let detail = open.then(|| {
            let lines = vec![
                agents::Line {
                    agent: Agent::Claude,
                    uses: Use::Missing,
                    file: CLAUDE_MCP.into(),
                    toggle: None,
                    busy: false,
                },
                self.droid_server_line(Some(s), cx),
            ];
            detail_panel()
                .child(agents::card(&key, lines))
                .child(facts_grid(vec![
                    ("Transport", s.server_type.clone(), false),
                    ("Status", s.status.clone(), false),
                    ("Tools", s.tools.to_string(), false),
                ]))
        });
        div().flex().flex_col().child(header).children(detail)
    }

    fn server_row(&self, s: &Server, droid: Option<&McpServer>, cx: &mut Context<Self>) -> Div {
        let key = format!("mcp:{}", s.name);
        let open = self.open.as_ref() == Some(&key);
        let checking = self.claude.data.as_ref().is_some_and(|c| c.checking);
        let uses = server_uses(s, droid);
        let (word, tone) = match droid {
            // Droid picked: its state, not Claude Code's health.
            Some(d) if self.show == Show::Only(Agent::Droid) => droid_server_state(d),
            _ if checking && s.health == Health::Unknown => ("Checking…", Tone::Neutral),
            _ => health_badge(&s.health),
        };
        let mut tags: Vec<(String, u32)> = vec![(s.transport.to_uppercase(), CAT_BLUE)];
        if s.scope == Scope::ClaudeAi {
            tags.push(("OAuth".into(), CAT_GREEN));
        } else if !s.headers.is_empty() {
            tags.push(("Token".into(), CAT_AMBER));
        }
        if !s.env.is_empty() {
            tags.push((format!("{} env", s.env.len()), CAT_SAND));
        }
        if !s.rules.is_empty() {
            tags.push((format!("{} rules", s.rules.len()), CAT_VIOLET));
        }
        // Claude Code's fixes, in its cells: with Droid picked, Droid's state shows.
        let claude_cells = self.show != Show::Only(Agent::Droid);
        let quick: Option<(Fix, bool)> = match &s.health {
            _ if !claude_cells => None,
            Health::NeedsAuth if s.unused_connector() => Some((Fix::Connect, false)),
            Health::NeedsAuth => Some((Fix::SignIn, true)),
            Health::Pending => Some((Fix::Approve, false)),
            _ => None,
        };
        let failing = claude_cells && matches!(s.health, Health::Failed(_));
        let server = s.clone();
        let quick = quick.map(|(fix, primary)| {
            ui::button(
                SharedString::from(format!("ext-quick-{}", s.name)),
                fix.label(),
                if primary {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Outline
                },
                true,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.fix(&server, fix, window, cx)
            }))
        });
        let fix_key = key.clone();
        let fix_button = failing.then(|| {
            ui::button_with(
                SharedString::from(format!("ext-fix-{}", s.name)),
                Some("wrench01"),
                "Fix",
                ButtonKind::Primary,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if this.open.as_ref() != Some(&fix_key) {
                    this.toggle_open(fix_key.clone(), cx)
                }
            }))
        });
        let dim = matches!(s.health, Health::Off) || s.unused_connector();
        let header = row_base(SharedString::from(format!("ext-{key}")), ROW_H)
            .when(open, |d| d.bg(theme::fill_hover()))
            .child(name_cell(
                logos::mark(&s.name, &s.target),
                32.,
                display_name(&s.name),
                s.target.clone(),
                dim,
            ))
            .child(cell(Some(STATUS_W)).child(self.status_cell(&uses, (word, tone))))
            .child(cell(Some(AGENTS_W)).child(agents::cell(self.show, &uses)))
            .child(cell(Some(SOURCE_W)).child(ui::badge(s.scope.label(), Tone::Neutral)))
            .child(cell(Some(TAGS_W)).child(tag_list(&tags, 3)))
            .child(
                cell(Some(ACTIONS_W))
                    .justify_end()
                    .gap(crate::ui::sp(4.))
                    .children(quick)
                    .children(fix_button)
                    .child(
                        icon(if open { "arrow-up01" } else { "arrow-down01" }, 14.)
                            .text_color(theme::text_faint()),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_open(key.clone(), cx)));
        div()
            .flex()
            .flex_col()
            .child(header)
            .when(open, |d| d.child(self.server_detail(s, droid, cx)))
    }

    fn server_detail(&self, s: &Server, droid: Option<&McpServer>, cx: &mut Context<Self>) -> Div {
        let uses = server_uses(s, droid);
        let droid_line = match &uses[Agent::Droid.index()] {
            Use::Cannot(why) => agents::Line {
                agent: Agent::Droid,
                uses: Use::Cannot(why),
                file: String::new(),
                toggle: None,
                busy: false,
            },
            _ => self.droid_server_line(droid, cx),
        };
        let lines = vec![
            agents::Line {
                agent: Agent::Claude,
                uses: uses[Agent::Claude.index()].clone(),
                file: claude_server_file(&s.scope),
                // Claude Code turns a server off per folder, from /mcp.
                toggle: None,
                busy: false,
            },
            droid_line,
        ];
        let mut facts = vec![
            ("Where", s.scope.about().to_string(), false),
            ("Transport", s.transport.clone(), false),
        ];
        facts.push((
            if s.transport == "stdio" {
                "Command"
            } else {
                "URL"
            },
            s.target.clone(),
            true,
        ));
        if !s.headers.is_empty() {
            facts.push((
                "Headers",
                format!("{} · values hidden", s.headers.join(", ")),
                false,
            ));
        }
        if !s.env.is_empty() {
            facts.push((
                "Environment",
                format!("{} · values hidden", s.env.join(", ")),
                false,
            ));
        }
        facts.push((
            "Tools are named",
            format!("{}__<tool>", s.tool_prefix()),
            true,
        ));
        let rules = if s.rules.is_empty() {
            div().ty(Type::Caption).text_color(theme::text_subtle()).child("No rules name its tools: Claude asks before each call unless the permission mode says otherwise.")
        } else {
            div()
                .flex()
                .flex_wrap()
                .gap(crate::ui::sp(6.))
                .children(s.rules.iter().map(rule_chip))
        };
        detail_panel()
            .children(self.fix_box(s, cx))
            .child(agents::card(&format!("mcp-{}", s.name), lines))
            .child(facts_grid(facts))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(8.))
                    .child(ui::overline("Permissions"))
                    .child(rules),
            )
    }

    /// Why the server needs you and the buttons that fix it; none when it's fine.
    fn fix_box(&self, s: &Server, cx: &mut Context<Self>) -> Option<Div> {
        let (why, fixes) = diagnose(s)?;
        let (_, tone) = health_badge(&s.health);
        let busy = self.op.as_ref() == Some(&s.name);
        let typing = self.token_for.as_ref() == Some(&s.name);
        let confirming = self.confirm_remove.as_ref() == Some(&s.name);
        let mut buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(8.));
        for (i, fix) in fixes.iter().copied().enumerate() {
            let label = if fix == Fix::Remove && confirming {
                "Click again to remove"
            } else {
                fix.label()
            };
            let kind = match fix {
                Fix::Remove => ButtonKind::Danger,
                _ if i == 0 => ButtonKind::Primary,
                _ => ButtonKind::Outline,
            };
            let server = s.clone();
            buttons = buttons.child(
                ui::button_with(
                    SharedString::from(format!("ext-fix-{}-{i}", s.name)),
                    Some(fix.icon()),
                    label,
                    kind,
                    !busy,
                )
                .when(!busy, |b| {
                    b.on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.fix(&server, fix, window, cx)
                    }))
                }),
            );
        }
        if busy {
            buttons = buttons.child(ui::spinner(14., theme::text_faint()));
        }
        let header_name = s.token_header().unwrap_or("Authorization").to_string();
        // A project server lives in the repo's `.mcp.json`, which is shared and
        // often committed: say so before the token goes in.
        let shared = s.scope == Scope::Project;
        let save_label = if shared { "Save in .mcp.json" } else { "Save" };
        let token_note = if shared {
            note(
                "This server is in the project's .mcp.json, so the token is saved in the repo and a commit can share it. To keep it out, type ${NAME} to read it from an environment variable instead.",
                Tone::Warning,
            )
        } else {
            div().ty(Type::Caption).text_color(theme::text_subtle()).child(
                "Stays in Claude's config (~/.claude.json) like the old one; Arakun never shows it.",
            )
        };
        Some(
            div()
                .flex()
                .gap(crate::ui::sp(12.))
                .p(crate::ui::sp(14.))
                .rounded(crate::ui::rad(ui::RADIUS_LG))
                .border_1()
                .border_color(tone.color().opacity(0.35))
                .bg(tone.color().opacity(0.06))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(28.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .bg(tone.color().opacity(0.16))
                        .child(
                            icon(
                                if tone == Tone::Danger {
                                    "alert-circle"
                                } else {
                                    "help-circle"
                                },
                                15.,
                            )
                            .text_color(tone.color()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(crate::ui::sp(10.))
                        .child(div().ty(Type::Body).text_color(theme::text()).child(why))
                        .when(typing, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(crate::ui::sp(8.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_1()
                                            .items_center()
                                            .gap(crate::ui::sp(8.))
                                            .h(px(ui::BUTTON_H))
                                            .px(crate::ui::sp(10.))
                                            .rounded(crate::ui::rad(ui::RADIUS))
                                            .border_1()
                                            .border_color(theme::border_strong())
                                            .bg(theme::background())
                                            .ty(Type::Body)
                                            .child(
                                                icon("square-lock02", 13.)
                                                    .text_color(theme::text_subtle()),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .ty(Type::Caption)
                                                    .text_color(theme::text_subtle())
                                                    .child(format!("{header_name}:")),
                                            )
                                            .child(
                                                div().flex_1().min_w_0().child(self.token.clone()),
                                            ),
                                    )
                                    .child(
                                        ui::button(
                                            "ext-token-save",
                                            save_label,
                                            ButtonKind::Primary,
                                            !busy,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.save_token(cx)),
                                        ),
                                    )
                                    .child(
                                        ui::button(
                                            "ext-token-cancel",
                                            "Cancel",
                                            ButtonKind::Ghost,
                                            true,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.token_for = None;
                                                cx.notify();
                                            }),
                                        ),
                                    ),
                            )
                            .child(token_note)
                        })
                        .when(!fixes.is_empty() && !typing, |d| d.child(buttons)),
                ),
        )
    }

    /// The servers that need you, each with its fix, above everything else.
    fn render_attention(&self, x: &ClaudeExtensions, cx: &mut Context<Self>) -> Option<Div> {
        let needy: Vec<&Server> = x
            .servers
            .iter()
            .filter(|s| {
                matches!(s.health, Health::Failed(_) | Health::Pending)
                    || (s.health == Health::NeedsAuth && !s.unused_connector())
            })
            .collect();
        if needy.is_empty() {
            return None;
        }
        let mut cards = div().flex().flex_col().gap(crate::ui::sp(10.));
        for (i, s) in needy.into_iter().enumerate() {
            let (word, tone) = health_badge(&s.health);
            cards = cards.child(
                div()
                    .id(("ext-needy", i))
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(12.))
                    .p(crate::ui::sp(16.))
                    .rounded(crate::ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_raised())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(12.))
                            .child(logos::tile(logos::mark(&s.name, &s.target), 36.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(crate::ui::sp(8.))
                                            .child(
                                                div()
                                                    .ty(Type::Heading)
                                                    .text_color(theme::text())
                                                    .child(display_name(&s.name)),
                                            )
                                            .child(status_pill(word, tone))
                                            .child(ui::badge(s.scope.label(), Tone::Neutral)),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .ty(Type::Caption)
                                            .text_color(theme::text_subtle())
                                            .child(s.target.clone()),
                                    ),
                            ),
                    )
                    .children(self.fix_box(s, cx)),
            );
        }
        Some(section(
            "Needs your attention",
            "Servers Claude can't use right now, and how to fix them",
            None,
            cards,
        ))
    }

    /// A plugin, for whichever agents have it: Claude Code's from its
    /// settings, Droid's from ~/.factory.
    fn plugin_row(
        &self,
        c: Option<&claude_ext::Plugin>,
        d: Option<&Installed>,
        cx: &mut Context<Self>,
    ) -> Div {
        let short = c.map_or_else(
            || d.map_or("", |d| droid_short(&d.id)).to_string(),
            |p| p.short().to_string(),
        );
        let key = format!("plugin:{short}");
        let open = self.open.as_ref() == Some(&key);
        let uses: Uses = [
            c.map_or(Use::Missing, |p| Use::on(p.enabled)),
            d.map_or(Use::Missing, |p| Use::on(p.enabled)),
        ];
        let mut tags: Vec<(String, u32)> = Vec::new();
        if let Some(p) = c {
            if p.skills > 0 {
                tags.push((plural(p.skills, "skill"), CAT_BLUE));
            }
            if p.agents > 0 {
                tags.push((plural(p.agents, "agent"), CAT_VIOLET));
            }
            if p.commands > 0 {
                tags.push((plural(p.commands, "command"), CAT_CYAN));
            }
            if !p.servers.is_empty() {
                tags.push(("MCP".into(), CAT_CORAL));
            }
            if p.hooks {
                tags.push(("hooks".into(), CAT_AMBER));
            }
            if p.short().ends_with("-lsp") {
                tags.push(("language server".into(), CAT_SAND));
            }
        }
        // The picked agent's switch; with every agent, Claude Code's (Droid's
        // plugins have none here: its CLI turns them on and off).
        let on = match (self.show, c, d) {
            (Show::Only(Agent::Droid), _, Some(d)) => d.enabled,
            (_, Some(p), _) => p.enabled,
            (_, None, Some(d)) => d.enabled,
            _ => false,
        };
        let switch = c
            .filter(|_| self.show != Show::Only(Agent::Droid))
            .map(|p| {
                let id = p.id.clone();
                self.agent_switch(
                    format!("ext-switch-{id}"),
                    p.short().to_string(),
                    p.enabled,
                    self.plugin_busy.as_ref() == Some(&p.id),
                    false,
                    move |this, on, cx| this.set_plugin(id.clone(), on, cx),
                    cx,
                )
            });
        let seo = c.is_some_and(is_seo_claude) || d.is_some_and(is_seo_droid);
        let seo_button = seo.then(|| self.seo_button(cx)).flatten();
        let subtitle = match (c, d) {
            (Some(p), _) => format!("v{} · {}", p.version, p.marketplace()),
            (None, Some(i)) => format!(
                "v{} · {}",
                i.version,
                i.id.split_once('@').map_or("", |(_, m)| m)
            ),
            _ => String::new(),
        };
        let header = row_base(SharedString::from(format!("ext-{key}")), ROW_H)
            .map(|d| ui::labelled(d, gpui::Role::Button, short.clone()))
            .when(open, |d| d.bg(theme::fill_hover()))
            .child(name_cell(
                logos::mark(&short, ""),
                32.,
                short.clone(),
                subtitle,
                !on,
            ))
            .child(cell(Some(STATUS_W)).child(self.status_cell(
                &uses,
                if on {
                    ("On", Tone::Success)
                } else {
                    ("Off", Tone::Neutral)
                },
            )))
            .child(cell(Some(AGENTS_W)).child(agents::cell(self.show, &uses)))
            .child(cell(Some(SOURCE_W)).child(ui::badge("user", Tone::Neutral)))
            .child(cell(Some(TAGS_W)).child(tag_list(&tags, 2)))
            .child(
                cell(Some(ACTIONS_W))
                    .justify_end()
                    .gap(crate::ui::sp(8.))
                    .children(seo_button)
                    .children(switch)
                    .child(
                        icon(if open { "arrow-up01" } else { "arrow-down01" }, 14.)
                            .text_color(theme::text_faint()),
                    ),
            )
            .on_click({
                let key = key.clone();
                cx.listener(move |this, _, _, cx| this.toggle_open(key.clone(), cx))
            });
        let detail = open.then(|| {
            let claude_toggle = c.and_then(|p| {
                let id = p.id.clone();
                self.toggle_for(cx, move |this, on, cx| this.set_plugin(id.clone(), on, cx))
            });
            let lines = vec![
                agents::Line {
                    agent: Agent::Claude,
                    uses: uses[0].clone(),
                    file: CLAUDE_SETTINGS.into(),
                    toggle: claude_toggle,
                    busy: c.is_some_and(|p| self.plugin_busy.as_ref() == Some(&p.id)),
                },
                agents::Line {
                    agent: Agent::Droid,
                    uses: uses[1].clone(),
                    file: d.map_or_else(|| DROID_PLUGINS.to_string(), |i| short_path(&i.path)),
                    toggle: None,
                    busy: false,
                },
            ];
            let mut facts = Vec::new();
            if let Some(p) = c {
                facts.push(("Id", p.id.clone(), true));
                if !p.servers.is_empty() {
                    facts.push(("MCP servers", p.servers.join(", "), false));
                }
                facts.push(("Installed in", short_path(&p.path), true));
            } else if let Some(i) = d {
                facts.push(("Id", i.id.clone(), true));
            }
            if seo && let Some(p) = &self.plugins {
                facts.push(("Claude SEO", seo_state(p).0, false));
            }
            detail_panel()
                .child(agents::card(&key, lines))
                .child(facts_grid(facts))
        });
        div().flex().flex_col().child(header).children(detail)
    }

    /// Claude SEO while neither agent has it: its own install sets it up for both.
    fn seo_row(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let uses: Uses = [Use::Missing, Use::Missing];
        row_base("ext-plugin-seo".into(), ROW_H)
            .child(name_cell(
                logos::mark("seo", ""),
                32.,
                "claude-seo".into(),
                "Site audits, schema, Core Web Vitals, AI search and local SEO".into(),
                true,
            ))
            .child(
                cell(Some(STATUS_W))
                    .child(self.status_cell(&uses, ("Not installed", Tone::Neutral))),
            )
            .child(cell(Some(AGENTS_W)).child(agents::cell(self.show, &uses)))
            .child(cell(Some(SOURCE_W)).child(ui::badge("user", Tone::Neutral)))
            .child(cell(Some(TAGS_W)).child(tag_list(
                &[("Claude Code and Droid".to_string(), CAT_GREEN)],
                1,
            )))
            .child(
                cell(Some(ACTIONS_W))
                    .justify_end()
                    .children(self.seo_button(cx)),
            )
    }

    /// Install, Set up or Update for Claude SEO; none when it can't run here.
    fn seo_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.plugins.as_ref()?;
        let (status, action) = seo_state(p);
        let label = action?;
        let enabled = self.cwd.is_some();
        let kind = if matches!(label, "Install" | "Set up") {
            ButtonKind::Primary
        } else {
            ButtonKind::Outline
        };
        Some(
            ui::button("seo-install", label, kind, enabled)
                .tooltip(ui::tip_text(format!(
                    "Claude SEO for Droid and Claude Code · {status}"
                )))
                .when(enabled, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.install_seo(cx)
                    }))
                })
                .into_any_element(),
        )
    }

    /// A skill, for whichever agents have it. Claude Code's opens its SKILL.md;
    /// Droid's has a switch.
    fn skill_row(
        &self,
        i: usize,
        source: &str,
        c: Option<&claude_ext::Skill>,
        d: Option<&extensions::Skill>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let name = c.map_or_else(|| d.map_or("", |d| d.name.as_str()), |s| s.name.as_str());
        let short = skill_short(name).to_string();
        let description = c.map_or_else(
            || d.map_or(String::new(), |d| d.description.clone()),
            |s| s.description.clone(),
        );
        let uses: Uses = [
            c.map_or(Use::Missing, |_| Use::On),
            d.map_or(Use::Missing, |d| Use::on(d.enabled)),
        ];
        let mark = if matches!(source, "personal" | "project") || c.is_none() {
            logos::mark(name, "")
        } else {
            logos::mark(source, "")
        };
        let dim = match self.show {
            Show::All => !uses.contains(&Use::On),
            Show::Only(a) => uses[a.index()] != Use::On,
        };
        let path = c.map(|s| s.path.join("SKILL.md"));
        let open_button = path.clone().map(|open_path| {
            ui::icon_button(
                SharedString::from(format!("skill-open-{source}-{i}")),
                "file01",
                14.,
                "Open SKILL.md",
            )
            .aria_label(format!("Open SKILL.md of {short}"))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(ExtensionsEvent::Open(open_path.clone()))
            }))
        });
        // Droid's switch where its state is what the row is about.
        let switch = d
            .filter(|_| c.is_none() || self.show == Show::Only(Agent::Droid))
            .map(|d| {
                let n = d.name.clone();
                self.agent_switch(
                    format!("droid-skill-switch-{source}-{i}"),
                    format!("{} for Droid", d.name),
                    d.enabled,
                    self.droid_busy == Some(Busy::Skill(d.name.clone())),
                    false,
                    move |this, on, cx| this.toggle_droid_skill(n.clone(), on, cx),
                    cx,
                )
            });
        let row = row_base(SharedString::from(format!("skill-{source}-{i}")), 52.)
            .child(name_cell(mark, 28., short.clone(), description, dim))
            .child(cell(Some(AGENTS_W)).child(agents::cell(self.show, &uses)))
            .child(cell(Some(SOURCE_W)).child(ui::badge(source.to_string(), Tone::Neutral)))
            .child(
                cell(Some(ACTIONS_W))
                    .justify_end()
                    .gap(crate::ui::sp(8.))
                    .children(switch)
                    .children(open_button),
            );
        match (path, d) {
            (Some(path), _) => row
                .map(|r| ui::labelled(r, gpui::Role::Button, format!("Open {short}")))
                .on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(ExtensionsEvent::Open(path.clone()))),
                ),
            (None, Some(d)) => {
                let (n, on) = (d.name.clone(), d.enabled);
                row.map(|r| ui::as_switch(r, format!("{short} for Droid"), on))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_droid_skill(n.clone(), !on, cx)
                    }))
            }
            (None, None) => row,
        }
    }

    fn render_permissions(
        &self,
        x: &ClaudeExtensions,
        rules: Vec<&claude_ext::Rule>,
        filtering: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = &x.permissions;
        let (mode, about) = match &p.mode {
            Some((m, source)) => (m.clone(), format!("Set in your {source} settings.")),
            None => (
                "default".to_string(),
                "Claude asks before edits, commands and tools the rules don't already allow."
                    .to_string(),
            ),
        };
        let mut files = div().flex().flex_wrap().gap(crate::ui::sp(6.));
        for (i, (source, path)) in p.files.iter().enumerate() {
            let path = path.clone();
            files =
                files.child(
                    div()
                        .id(("ext-settings", i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(ui::BUTTON_H))
                        .px(crate::ui::sp(10.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::background())
                        .cursor_pointer()
                        .hover(|d| d.bg(theme::fill_hover()))
                        .ty(Type::Label)
                        .text_color(theme::text_muted())
                        .child(icon("file01", 13.).text_color(theme::text_subtle()))
                        .child(div().text_color(theme::text()).child(source.to_string()))
                        .child(short_path(&path))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ExtensionsEvent::Open(path.clone()))
                        })),
                );
        }
        let banner = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(14.))
            .px(crate::ui::sp(16.))
            .py(crate::ui::sp(14.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .bg(theme::fill_selected())
                    .child(icon("shield-alert", 16.).text_color(theme::text())),
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
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(
                                div()
                                    .ty(Type::Body)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Mode"),
                            )
                            .child(ui::badge(mode, Tone::Accent)),
                    )
                    .child(
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(about),
                    ),
            )
            .child(files);

        let all = filtering || self.unfolded.contains("rules");
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut ordered: Vec<&&claude_ext::Rule> = Vec::new();
        for kind in [RuleKind::Deny, RuleKind::Ask, RuleKind::Allow] {
            ordered.extend(rules.iter().filter(|r| r.kind == kind));
        }
        let fold = 30;
        for (i, r) in ordered
            .iter()
            .take(if all { usize::MAX } else { fold })
            .enumerate()
        {
            let (tool, target) = split_rule(&r.rule);
            let tone = rule_tone(r.kind);
            let lead = match tool.strip_prefix("mcp__") {
                Some(server) => logos::tile(
                    logos::mark(
                        &server
                            .split("__")
                            .next()
                            .unwrap_or(server)
                            .replace('_', " "),
                        "",
                    ),
                    24.,
                ),
                None => div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(crate::ui::rad(ui::RADIUS_SM))
                    .bg(theme::fill_selected())
                    .child(icon(tool_icon(tool), 13.).text_color(theme::text_muted())),
            };
            rows.push(
                div()
                    .id(("ext-rule", i))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(16.))
                    .h(px(44.))
                    .px(crate::ui::sp(16.))
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .child(
                        cell(None)
                            .gap(crate::ui::sp(10.))
                            .child(lead)
                            .child(
                                div()
                                    .flex_none()
                                    .ty(Type::Body)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(tool.to_string()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .ty(Type::Mono)
                                    .text_color(theme::text_subtle())
                                    .child(target.to_string()),
                            ),
                    )
                    .child(
                        cell(Some(STATUS_W)).child(status_pill(capitalize(r.kind.label()), tone)),
                    )
                    .child(cell(Some(AGENTS_W)).child(agents::cell(self.show, &RULE_USES)))
                    .child(cell(Some(SOURCE_W)).child(ui::badge(r.source, Tone::Neutral)))
                    .into_any_element(),
            );
        }
        if !all && ordered.len() > fold {
            rows.push(
                group_row(
                    String::new(),
                    Some(
                        self.show_all("rules".into(), ordered.len(), cx)
                            .into_any_element(),
                    ),
                )
                .into_any_element(),
            );
        }
        if ordered.is_empty() {
            rows.push(
                empty_row("No allow, ask or deny rules: Claude asks every time.")
                    .into_any_element(),
            );
        }
        let n = |k: RuleKind| p.rules.iter().filter(|r| r.kind == k).count();
        let aside = div()
            .ty(Type::Caption)
            .text_color(theme::text_subtle())
            .child(format!(
                "{} allow · {} ask · {} deny",
                n(RuleKind::Allow),
                n(RuleKind::Ask),
                n(RuleKind::Deny)
            ));
        section(
            "Permissions",
            "What Claude may do without asking, and what it never may",
            Some(aside.into_any_element()),
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(12.))
                .child(banner)
                .child(table(&RULE_COLS, rows)),
        )
    }

    fn show_all(&self, key: String, n: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(SharedString::from(format!("ext-all-{key}")))
            .flex()
            .items_center()
            .gap(crate::ui::sp(4.))
            .h(px(24.))
            .px(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS_SM))
            .cursor_pointer()
            .ty(Type::Label)
            .text_color(theme::text_muted())
            .hover(|d| d.bg(theme::fill_selected()).text_color(theme::text()))
            .child(format!("Show all {n}"))
            .child(icon("arrow-down01", 12.).text_color(theme::text_subtle()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.unfolded.insert(key.clone());
                cx.notify();
            }))
    }
}

// ---------------------------------------------------------------------------
// The table.

struct Col {
    label: &'static str,
    width: Option<f32>,
}

const STATUS_W: f32 = 136.;
const AGENTS_W: f32 = 150.;
const SOURCE_W: f32 = 104.;
const TAGS_W: f32 = 210.;
const ACTIONS_W: f32 = 100.;
const ROW_H: f32 = 60.;

const SERVER_COLS: [Col; 6] = [
    Col {
        label: "Name",
        width: None,
    },
    Col {
        label: "Status",
        width: Some(STATUS_W),
    },
    Col {
        label: "Agents",
        width: Some(AGENTS_W),
    },
    Col {
        label: "Scope",
        width: Some(SOURCE_W),
    },
    Col {
        label: "Details",
        width: Some(TAGS_W),
    },
    Col {
        label: "",
        width: Some(ACTIONS_W),
    },
];
const VIEW_COLS: [Col; 3] = [
    Col {
        label: "Name",
        width: None,
    },
    Col {
        label: "Status",
        width: Some(STATUS_W),
    },
    Col {
        label: "",
        width: Some(ACTIONS_W),
    },
];
const PLUGIN_COLS: [Col; 6] = [
    Col {
        label: "Name",
        width: None,
    },
    Col {
        label: "Status",
        width: Some(STATUS_W),
    },
    Col {
        label: "Agents",
        width: Some(AGENTS_W),
    },
    Col {
        label: "Scope",
        width: Some(SOURCE_W),
    },
    Col {
        label: "Brings",
        width: Some(TAGS_W),
    },
    Col {
        label: "",
        width: Some(ACTIONS_W),
    },
];
const SKILL_COLS: [Col; 4] = [
    Col {
        label: "Name",
        width: None,
    },
    Col {
        label: "Agents",
        width: Some(AGENTS_W),
    },
    Col {
        label: "Source",
        width: Some(SOURCE_W),
    },
    Col {
        label: "",
        width: Some(ACTIONS_W),
    },
];
const RULE_COLS: [Col; 4] = [
    Col {
        label: "Rule",
        width: None,
    },
    Col {
        label: "Effect",
        width: Some(STATUS_W),
    },
    Col {
        label: "Agents",
        width: Some(AGENTS_W),
    },
    Col {
        label: "Source",
        width: Some(SOURCE_W),
    },
];
const CAT_BLUE: u32 = theme::CATEGORY[0];
const CAT_AMBER: u32 = theme::CATEGORY[1];
const CAT_GREEN: u32 = theme::CATEGORY[2];
const CAT_VIOLET: u32 = theme::CATEGORY[3];
const CAT_CORAL: u32 = theme::CATEGORY[4];
const CAT_CYAN: u32 = theme::CATEGORY[5];
const CAT_SAND: u32 = theme::CATEGORY[7];

/// The name column's least width: below it the table scrolls sideways
/// instead of squeezing names to nothing under the fixed columns.
const NAME_MIN_W: f32 = 180.;

fn table(cols: &[Col], rows: Vec<AnyElement>) -> Stateful<Div> {
    // Cells, the gaps between them and the padding at both ends.
    let min_w = cols
        .iter()
        .map(|c| match c.width {
            None => NAME_MIN_W,
            Some(TAGS_W) => TAGS_MIN_W,
            Some(w) => w,
        })
        .sum::<f32>()
        + 16. * (cols.len() as f32 - 1.)
        + 32.;
    let id: SharedString = format!(
        "ext-table-{}",
        cols.iter().map(|c| c.label).collect::<String>()
    )
    .into();
    let header = div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(16.))
        .h(px(36.))
        .px(crate::ui::sp(16.))
        .bg(theme::fill_raised())
        .children(cols.iter().map(|c| {
            cell(c.width)
                .ty(Type::Label)
                .text_color(theme::text_subtle())
                .child(c.label)
        }));
    div()
        .id(id)
        .rounded(crate::ui::rad(ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border())
        .bg(theme::background())
        .overflow_x_scroll()
        .child(
            div()
                .flex()
                .flex_col()
                .min_w(px(min_w))
                .child(header)
                .children(rows),
        )
}

/// How narrow the tags column gets: beside the left column the table is often
/// narrower than its columns, and the tags give way before the name does.
const TAGS_MIN_W: f32 = 64.;

fn cell(width: Option<f32>) -> Div {
    match width {
        Some(TAGS_W) => div()
            .flex()
            .flex_shrink(1.)
            .min_w(px(TAGS_MIN_W))
            .overflow_hidden()
            .items_center()
            .w(px(TAGS_W)),
        Some(w) => div().flex().flex_none().items_center().w(px(w)),
        None => div().flex().flex_1().min_w(px(NAME_MIN_W)).items_center(),
    }
}

fn row_base(id: SharedString, height: f32) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(crate::ui::sp(16.))
        .h(px(height))
        .px(crate::ui::sp(16.))
        .border_t_1()
        .border_color(theme::border_subtle())
        .cursor_pointer()
        .hover(|d| d.bg(theme::fill_hover()))
}

fn name_cell(mark: logos::Mark, size: f32, title: String, subtitle: String, dim: bool) -> Div {
    cell(None)
        .gap(crate::ui::sp(12.))
        .child(logos::tile(mark, size).when(dim, |d| d.opacity(0.55)))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap(crate::ui::sp(1.))
                .child(
                    div()
                        .truncate()
                        .ty(Type::Body)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if dim {
                            theme::text_muted()
                        } else {
                            theme::text()
                        })
                        .child(title),
                )
                .when(!subtitle.is_empty(), |d| {
                    d.child(
                        div()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(subtitle),
                    )
                }),
        )
}

/// A state in an outlined pill: a dot in the state's color and the word.
fn status_pill(word: impl Into<SharedString>, tone: Tone) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .h(px(24.))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised())
        .ty(Type::Label)
        .text_color(theme::text())
        .child(div().size(px(6.)).rounded_full().bg(tone.color()))
        .child(word.into())
}

/// Colored chips, the first `max` of them and a "+N" for the rest.
fn tag_list(tags: &[(String, u32)], max: usize) -> Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(4.))
        .overflow_hidden()
        .children(
            tags.iter()
                .take(max)
                .map(|(t, c)| ui::tag(t.clone(), rgb(*c))),
        )
        .when(tags.len() > max, |d| {
            d.child(ui::badge(format!("+{}", tags.len() - max), Tone::Neutral))
        })
}

fn group_row(label: String, action: Option<AnyElement>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(36.))
        .px(crate::ui::sp(16.))
        .border_t_1()
        .border_color(theme::border_subtle())
        .bg(theme::fill_raised())
        .when(!label.is_empty(), |d| d.child(ui::overline(&label)))
        .child(div().flex_1())
        .children(action)
}

fn empty_row(text: &str) -> Div {
    div()
        .px(crate::ui::sp(16.))
        .py(crate::ui::sp(18.))
        .border_t_1()
        .border_color(theme::border_subtle())
        .ty(Type::Body)
        .text_color(theme::text_subtle())
        .child(text.to_string())
}

fn detail_panel() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(16.))
        .pl(crate::ui::sp(60.))
        .pr(crate::ui::sp(16.))
        .py(crate::ui::sp(16.))
        .border_t_1()
        .border_color(theme::border_subtle())
        .bg(theme::fill_raised())
}

/// Label over value, two to a line.
fn facts_grid(facts: Vec<(&'static str, String, bool)>) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap_y(crate::ui::sp(14.))
        .children(facts.into_iter().map(|(label, value, mono)| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(3.))
                .w(relative(0.5))
                .pr(crate::ui::sp(16.))
                .child(
                    div()
                        .ty(Type::Caption)
                        .text_color(theme::text_subtle())
                        .child(label),
                )
                .child(
                    div()
                        .truncate()
                        .ty(if mono { Type::Mono } else { Type::Body })
                        .text_color(theme::text())
                        .child(value),
                )
        }))
}

fn section(
    title: &'static str,
    about: &'static str,
    aside: Option<AnyElement>,
    content: impl IntoElement,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(12.))
        .child(
            div()
                .flex()
                .items_end()
                .gap(crate::ui::sp(12.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(crate::ui::sp(2.))
                        .child(ui::heading(title))
                        .child(
                            div()
                                .ty(Type::Caption)
                                .text_color(theme::text_subtle())
                                .child(about),
                        ),
                )
                .children(aside),
        )
        .child(content)
}

fn switch_or_spinner(on: bool, busy: bool, locked: bool) -> AnyElement {
    if busy {
        div()
            .w(px(30.))
            .flex()
            .justify_center()
            .child(ui::spinner(14., theme::text_faint()))
            .into_any_element()
    } else {
        div()
            .when(locked, |d| d.opacity(0.4))
            .child(ui::switch(on))
            .into_any_element()
    }
}

/// Where each agent reads an item from, for the Agents card.
const CLAUDE_MCP: &str = "~/.claude.json or .mcp.json";
const CLAUDE_SETTINGS: &str = "~/.claude/settings.json";
const DROID_MCP: &str = "~/.factory/mcp.json";
const DROID_PLUGINS: &str = "~/.factory/plugins";

/// Why Droid can't use an item Claude Code has.
const CONNECTOR_NOT_FOR_DROID: &str = "A claude.ai connector: only Claude Code signs in to it";
const RULES_NOT_FOR_DROID: &str = "Droid keeps its own allow and deny lists";

/// A permission rule is Claude Code's alone.
const RULE_USES: Uses = [Use::On, Use::Cannot(RULES_NOT_FOR_DROID)];

/// What a search for Claude SEO's row matches.
const SEO_WORDS: &str =
    "claude-seo Claude SEO audits schema core web vitals geo sitemap backlinks local";

fn claude_server_file(scope: &Scope) -> String {
    match scope {
        Scope::User | Scope::Local => "~/.claude.json".into(),
        Scope::Project => ".mcp.json".into(),
        Scope::Plugin(p) => format!("the {p} plugin"),
        Scope::ClaudeAi => "your claude.ai account".into(),
    }
}

fn droid_server_file(source: &str) -> String {
    match source {
        "project" => ".factory/mcp.json".into(),
        "user" | "" => DROID_MCP.into(),
        other => other.to_string(),
    }
}

/// How Claude Code and Droid stand with an MCP server Claude Code has.
fn server_uses(s: &Server, droid: Option<&McpServer>) -> Uses {
    let claude = Use::on(s.health != Health::Off);
    let droid = match droid {
        Some(d) => Use::on(d.enabled()),
        None if s.scope == Scope::ClaudeAi => Use::Cannot(CONNECTOR_NOT_FOR_DROID),
        None => Use::Missing,
    };
    [claude, droid]
}

fn droid_server_state(s: &McpServer) -> (&'static str, Tone) {
    match s.status.as_str() {
        "disabled" => ("Off", Tone::Neutral),
        "failed" => ("Failing", Tone::Danger),
        "connected" | "running" | "ready" => ("Connected", Tone::Success),
        _ if s.tools > 0 => ("Connected", Tone::Success),
        _ => ("On", Tone::Info),
    }
}

/// `claude-seo@claude-seo` → `claude-seo`.
fn droid_short(id: &str) -> &str {
    id.split('@').next().unwrap_or(id)
}

/// `claude-seo:seo-audit` → `seo-audit`: a plugin's skill and Droid's of the
/// same name are one row.
fn skill_short(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn is_seo_claude(p: &claude_ext::Plugin) -> bool {
    p.id == plugins::SEO_CLAUDE
}

fn is_seo_droid(i: &Installed) -> bool {
    i.id == plugins::SEO_DROID
}

/// Claude SEO's status line and button label (none when it can't run).
fn seo_state(p: &Plugins) -> (String, Option<&'static str>) {
    if !p.python {
        return ("Needs Python 3.10 or newer".into(), None);
    }
    let Some((droid, droid_rt)) = &p.seo_droid else {
        return ("Not installed".into(), Some("Install"));
    };
    let ready = |rt: &Option<Runtime>| rt.as_ref().is_some_and(|r| r.ready && r.browser);
    if !ready(droid_rt) || p.seo_claude.as_ref().is_some_and(|(_, rt)| !ready(rt)) {
        return ("Installed · runtime needs setup".into(), Some("Set up"));
    }
    let mut status = format!("Ready · Droid {}", droid.version);
    match &p.seo_claude {
        Some((c, _)) => status.push_str(&format!(" · Claude Code {}", c.version)),
        None if p.claude_cli => status.push_str(" · not in Claude Code"),
        None => {}
    }
    (status, Some("Update"))
}

fn health_label(h: &Health) -> &str {
    match h {
        Health::Connected => "connected",
        Health::NeedsAuth => "needs sign-in",
        Health::Pending => "pending approval",
        Health::Failed(_) => "failing",
        Health::Off => "off",
        Health::Unknown => "unknown",
    }
}

fn health_badge(h: &Health) -> (&'static str, Tone) {
    match h {
        Health::Connected => ("Connected", Tone::Success),
        Health::NeedsAuth => ("Needs sign-in", Tone::Warning),
        Health::Pending => ("Pending approval", Tone::Info),
        Health::Failed(_) => ("Failing", Tone::Danger),
        Health::Off => ("Off here", Tone::Neutral),
        Health::Unknown => ("Not checked", Tone::Neutral),
    }
}

/// `plugin:posthog:posthog` → `posthog`, `claude.ai Gmail` → `Gmail`.
fn display_name(name: &str) -> String {
    let name = name.strip_prefix("claude.ai ").unwrap_or(name);
    name.rsplit(':').next().unwrap_or(name).to_string()
}

fn rule_tone(kind: RuleKind) -> Tone {
    match kind {
        RuleKind::Allow => Tone::Success,
        RuleKind::Ask => Tone::Warning,
        RuleKind::Deny => Tone::Danger,
    }
}

fn rule_chip(r: &claude_ext::Rule) -> Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .max_w(px(420.))
        .h(px(24.))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .border_1()
        .border_color(theme::border())
        .bg(theme::background())
        .child(
            div()
                .flex_none()
                .size(px(6.))
                .rounded_full()
                .bg(rule_tone(r.kind).color()),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .ty(Type::Mono)
                .text_color(theme::text_muted())
                .child(r.rule.clone()),
        )
        .child(
            div()
                .flex_none()
                .ty(Type::Caption)
                .text_color(theme::text_faint())
                .child(r.source),
        )
}

/// `Bash(git push:*)` → (`Bash`, `git push:*`); `mcp__posthog__*` → (`mcp__posthog__*`, ``).
fn split_rule(rule: &str) -> (&str, &str) {
    match rule.split_once('(') {
        Some((tool, rest)) => (tool, rest.strip_suffix(')').unwrap_or(rest)),
        None => (rule, ""),
    }
}

fn tool_icon(tool: &str) -> &'static str {
    match tool {
        "Bash" | "PowerShell" => "computer-terminal01",
        "Read" | "Glob" | "Grep" => "file01",
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => "pencil-edit01",
        "WebFetch" | "WebSearch" => "globe",
        "Task" | "Agent" => "bot",
        "Skill" => "sparkles",
        _ => "wrench01",
    }
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

fn note(text: impl Into<SharedString>, tone: Tone) -> Div {
    div()
        .px(crate::ui::sp(12.))
        .py(crate::ui::sp(10.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .bg(tone.color().opacity(0.1))
        .ty(Type::Body)
        .text_color(tone.color())
        .child(text.into())
}

/// `~/.claude/settings.json` rather than the whole home path.
fn short_path(p: &std::path::Path) -> String {
    let home = crate::platform::home();
    match p.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

impl ExtensionsView {
    /// Picks whose state the cells say. The sections stay as they are.
    fn set_show(&mut self, show: Show, cx: &mut Context<Self>) {
        self.show = show;
        self.refresh_if_stale(cx);
        cx.notify();
    }

    /// Show for, in the header: All agents, then one segment per agent.
    fn render_show_for(&self, cx: &mut Context<Self>) -> Div {
        let mut seg = ui::segmented();
        for (i, show) in Show::every().enumerate() {
            seg = seg.child(
                ui::segment(("ext-show", i), None, show.label(), self.show == show)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_show(show, cx))),
            );
        }
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .child(
                div()
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child("Show for"),
            )
            .child(seg)
    }

    /// Shows one list of Installed, or Discover, with the search box worded for it.
    fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        self.kind = kind;
        self.open = None;
        let hint = match kind {
            Kind::Marketplace => "Search plugins, MCP servers and skills…",
            Kind::Servers => "Filter MCP servers…",
            Kind::Plugins => "Filter plugins…",
            Kind::Skills => "Filter skills…",
            Kind::Permissions => "Filter permission rules…",
            Kind::Arakun => "Filter extensions…",
        };
        self.query.update(cx, |q, cx| q.set_placeholder(hint, cx));
        cx.notify();
    }

    /// The pages, as tabs over the view (`crate::subnav`): Arakun's own
    /// extensions, then whose things, what there is to add and what is
    /// installed. Where to install and the categories are `filters`.
    pub fn nav(&self) -> Vec<Line> {
        let mut out = vec![
            Line::Entry(
                Entry::new("arakun", Lead::Icon("dashboard-square01"), "Extensions")
                    .count(Some(self.views.on.len()))
                    .on(self.kind == Kind::Arakun),
            ),
            Line::Heading("Discover".into()),
        ];
        for (m, key, label, icon_name) in [
            (Market::Plugins, "market:plugins", "Plugins", "cube"),
            (Market::Registry, "market:registry", "MCP servers", "link01"),
            (Market::Skills, "market:skills", "Skills", "sparkles"),
            (
                Market::Widgets,
                "market:widgets",
                "Widgets",
                "dashboard-square01",
            ),
        ] {
            let on = self.kind == Kind::Marketplace && self.market == m;
            out.push(Line::Entry(
                Entry::new(key, Lead::Icon(icon_name), label)
                    .count(self.discover_count(m))
                    .on(on),
            ));
        }

        // The same pages for every agent: Show for changes the words only.
        out.push(Line::Heading("Installed".into()));
        let problems = match self.show {
            Show::Only(Agent::Droid) => 0,
            _ => self.problems(),
        };
        let [servers, plugins, skills, rules] = self.installed_counts();
        let rows: [(Kind, &str, &str, Option<usize>); 4] = [
            (Kind::Servers, "MCP servers", "link01", servers),
            (Kind::Plugins, "Plugins", "cube", plugins),
            (Kind::Skills, "Skills", "sparkles", skills),
            (Kind::Permissions, "Permissions", "shield-alert", rules),
        ];
        for (k, label, icon_name, n) in rows {
            let entry = Entry::new(kind_key(k), Lead::Icon(icon_name), label).on(self.kind == k);
            // Servers that need you say how many, not how many there are.
            let entry = if k == Kind::Servers && problems > 0 {
                entry.count(Some(problems)).tone(Some(Tone::Warning))
            } else {
                entry.count(n)
            };
            out.push(Line::Entry(entry));
        }

        out
    }

    /// How many rows each Installed page has, every agent's items once.
    fn installed_counts(&self) -> [Option<usize>; 4] {
        let Some(x) = self.claude.data.as_ref() else {
            return [None; 4];
        };
        let droid = self.droid.as_ref();
        let used: Vec<&Server> = x.servers.iter().filter(|s| !s.unused_connector()).collect();
        let droid_servers: Vec<&McpServer> =
            droid.map_or(Vec::new(), |d| d.servers.iter().collect());
        let servers = agents::merge(
            &used,
            |s| display_name(&s.name),
            &droid_servers,
            |d| d.name.clone(),
        )
        .len();
        let plugins = agents::merge(
            &x.plugins,
            |p| p.short().to_string(),
            self.plugins.as_ref().map_or(&[][..], |p| &p.droid),
            |i| droid_short(&i.id).to_string(),
        )
        .len();
        let skills = agents::merge(
            &x.skills,
            |s| skill_short(&s.name).to_string(),
            droid.map_or(&[][..], |d| &d.skills),
            |s| s.name.clone(),
        )
        .len();
        [
            Some(servers),
            Some(plugins),
            Some(skills),
            Some(x.permissions.rules.len()),
        ]
    }

    /// What narrows the page, in a quieter row under the tabs: where to
    /// install and, in Discover, its categories.
    pub fn filters(&self) -> Vec<Line> {
        let mut out = self.target_lines();
        if self.kind == Kind::Marketplace {
            out.extend(self.category_lines());
        }
        out
    }

    /// Opens the page `key` of `nav` names.
    pub fn pick_nav(&mut self, key: &str, cx: &mut Context<Self>) {
        match key {
            "arakun" => self.set_kind(Kind::Arakun, cx),
            "market:plugins" => self.show_market(Market::Plugins, cx),
            "market:registry" => self.show_market(Market::Registry, cx),
            "market:skills" => self.show_market(Market::Skills, cx),
            "market:widgets" => self.show_market(Market::Widgets, cx),
            "target:" => self.pick_target(None, cx),
            "cat:" => self.pick_category(None, cx),
            _ => {
                if let Some(id) = key.strip_prefix("target:") {
                    self.pick_target(Some(id.to_string()), cx);
                } else if let Some(g) = key.strip_prefix("cat:") {
                    self.pick_category(Some(g.to_string()), cx);
                } else if let Some(k) = [
                    Kind::Servers,
                    Kind::Plugins,
                    Kind::Skills,
                    Kind::Permissions,
                ]
                .into_iter()
                .find(|k| kind_key(*k) == key)
                {
                    self.set_kind(k, cx);
                }
            }
        }
    }

    /// Arakun's extensions, each with a switch. On, it shows in the sidebar.
    fn render_views(&self, cx: &mut Context<Self>) -> Div {
        let words = self.words(cx);
        let rows: Vec<AnyElement> = View::EXTENSIONS
            .iter()
            .copied()
            .filter(|v| {
                let hay = format!("{} {}", v.label(), v.blurb()).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .map(|v| self.view_row(v, cx).into_any_element())
            .collect();
        let rows = if rows.is_empty() {
            vec![empty_row("No extension matches.").into_any_element()]
        } else {
            rows
        };
        let on = self.views.on.len();
        section(
            "Extensions",
            "Tabs you can add to the sidebar. Turned off, they stay out of it.",
            Some(
                div()
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child(format!("{on} of {} on", View::EXTENSIONS.len()))
                    .into_any_element(),
            ),
            table(&VIEW_COLS, rows),
        )
    }

    fn view_row(&self, view: View, cx: &mut Context<Self>) -> Stateful<Div> {
        let on = self.views.shows(view);
        row_base(
            SharedString::from(format!("ext-view-{}", view.label())),
            ROW_H,
        )
        .map(|d| ui::as_switch(d, view.label(), on))
        .aria_description(view.blurb())
        .child(
            cell(None)
                .gap(crate::ui::sp(12.))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(32.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::fill_raised())
                        .when(!on, |d| d.opacity(0.55))
                        .child(icon(view.icon(), 16.).text_color(theme::text_subtle())),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .gap(crate::ui::sp(1.))
                        .child(
                            div()
                                .truncate()
                                .ty(Type::Body)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if on {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .child(view.label()),
                        )
                        .child(
                            div()
                                .truncate()
                                .ty(Type::Caption)
                                .text_color(theme::text_subtle())
                                .child(view.blurb()),
                        ),
                ),
        )
        .child(cell(Some(STATUS_W)).child(if on {
            status_pill("On", Tone::Success)
        } else {
            status_pill("Off", Tone::Neutral)
        }))
        .child(cell(Some(ACTIONS_W)).justify_end().child(ui::switch(on)))
        .on_click(cx.listener(move |this, _, _, cx| this.set_view(view, !on, cx)))
    }

    /// The last error and what the last action did.
    fn render_notes(&self) -> Option<Div> {
        (self.error.is_some() || self.notice.is_some()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
                .when_some(self.error.clone(), |d, e| d.child(note(e, Tone::Danger)))
                .when_some(self.notice.clone(), |d, n| d.child(note(n, Tone::Success)))
        })
    }
}

/// The key of an Installed page in `nav`.
fn kind_key(k: Kind) -> &'static str {
    match k {
        Kind::Servers => "kind:servers",
        Kind::Plugins => "kind:plugins",
        Kind::Skills => "kind:skills",
        Kind::Permissions => "kind:permissions",
        Kind::Arakun => "arakun",
        Kind::Marketplace => "market",
    }
}

impl Render for ExtensionsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.claude.loading()
            || self.plugin_busy.is_some()
            || self._droid_task.is_some()
            || self.plugins.is_none() && self.droid_at.is_some();
        // The older of the two reads: what the page says is no newer than that.
        let updated = match (self.claude_at, self.droid_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
        .map(|t| ago(t.elapsed()));
        let place = self
            .cwd
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let market = self.kind == Kind::Marketplace;
        let body: AnyElement = match &self.claude.data {
            _ if self.kind == Kind::Arakun => self.render_views(cx).into_any_element(),
            _ if market => div().into_any_element(),
            Some(x) => self.render_installed(x, cx).into_any_element(),
            None => ui::loading("Reading Claude Code and Droid, and checking their MCP servers…")
                .into_any_element(),
        };

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(12.))
            .h(px(64.))
            .px(crate::ui::sp(ui::PAGE_PAD))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_raised())
                    .child(icon("store01", 16.).text_color(theme::text())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .ty(Type::Subtitle)
                            .text_color(theme::text())
                            .child("Marketplace"),
                    )
                    .child(
                        div()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(if self.kind == Kind::Arakun {
                                "Tabs for the sidebar, on every project".to_string()
                            } else {
                                format!(
                                    "Plugins, MCP servers and skills for your agents in {place}"
                                )
                            }),
                    ),
            )
            .when(self.kind != Kind::Arakun, |d| {
                d.child(self.render_show_for(cx))
            })
            .child(
                div()
                    .flex()
                    .flex_shrink(1.)
                    .w(px(300.))
                    .min_w(px(160.))
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(ui::BUTTON_H + 4.))
                    .px(crate::ui::sp(10.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_raised())
                    .ty(Type::Body)
                    .child(icon("search01", 14.).text_color(theme::text_faint()))
                    .child(div().flex_1().min_w_0().child(self.query.clone())),
            )
            .child(ui::refresh_control(
                "ext-refresh",
                busy,
                updated,
                cx.listener(|this, _, _, cx| this.refresh(cx)),
            ));

        let notes = self.render_notes();

        // The Marketplace scrolls its own virtual list and fills the height.
        let content: AnyElement = if market {
            self.render_market(cx).into_any_element()
        } else {
            div()
                .id("extensions-list")
                .track_scroll(&self.scroll)
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_y_scroll()
                .px(crate::ui::sp(ui::PAGE_PAD))
                .pt(crate::ui::sp(20.))
                .pb(crate::ui::sp(32.))
                .gap(crate::ui::sp(16.))
                .children(notes)
                .child(body)
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(header)
            .child(div().flex().flex_1().min_h_0().child(content))
    }
}

fn ago(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        "just now".into()
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else {
        format!("{}h ago", s / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::plugins::Installed;

    fn seo(version: &str, ready: bool) -> (Installed, Option<Runtime>) {
        let installed = Installed {
            id: "x".into(),
            version: version.into(),
            path: "/x".into(),
            enabled: true,
        };
        (
            installed,
            Some(Runtime {
                ready,
                browser: true,
                version: "2.4.2".into(),
            }),
        )
    }

    #[test]
    fn seo_card_states() {
        let mut p = Plugins {
            python: true,
            claude_cli: true,
            ..Default::default()
        };
        assert_eq!(seo_state(&p), ("Not installed".into(), Some("Install")));
        p.seo_droid = Some(seo("4b99de2", true));
        assert_eq!(
            seo_state(&p),
            (
                "Ready · Droid 4b99de2 · not in Claude Code".into(),
                Some("Update")
            )
        );
        p.seo_claude = Some(seo("2.4.2", false));
        assert_eq!(
            seo_state(&p),
            ("Installed · runtime needs setup".into(), Some("Set up"))
        );
        p.seo_claude = Some(seo("2.4.2", true));
        assert_eq!(
            seo_state(&p),
            (
                "Ready · Droid 4b99de2 · Claude Code 2.4.2".into(),
                Some("Update")
            )
        );
        p.python = false;
        assert_eq!(seo_state(&p).1, None);
    }

    #[test]
    fn splits_rules() {
        assert_eq!(split_rule("Bash(git push:*)"), ("Bash", "git push:*"));
        assert_eq!(split_rule("mcp__posthog__*"), ("mcp__posthog__*", ""));
        assert_eq!(display_name("plugin:posthog:posthog"), "posthog");
        assert_eq!(display_name("claude.ai Google Drive"), "Google Drive");
    }
}
