//! The Usage tab: what agents used, from their own logs
//! (`usage_history`), so sessions run outside arakun count too. Amounts are
//! what the tokens would cost at API prices: the plans don't charge them, but
//! it is the one measure both agents share. Next to it, the plans' limits
//! (OpenCode's too, for its Go plan).

use crate::ui::Typography as _;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px, relative,
};

use crate::account::{Account, UsageMode};
use crate::account_view;
use crate::agent::claude_auth;
use crate::agent::cliproxy;
use crate::agent::plan::{self, PlanUsage, Seats};
use crate::agent::seats;
use crate::agent::{AgentKind, fmt_cost};
use crate::agent::{devin_plan, opencode_plan};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind};
use crate::usage_history::{self, Record};

/// The logs are stat-ed this often; only changed files are read again.
const POLL: Duration = Duration::from_secs(30);
/// The plan panel re-reads its cache this often; `plan::refresh` limits the
/// actual network fetches on its own.
const PLAN_POLL: Duration = Duration::from_secs(30);
const DAY: u64 = 24 * 60 * 60;
/// Rows the breakdown shows before "Show all".
const ROWS: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Range {
    Week,
    Month,
    Quarter,
}

impl Range {
    fn days(self) -> u64 {
        match self {
            Range::Week => 7,
            Range::Month => 30,
            Range::Quarter => 90,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Range::Week => "7 days",
            Range::Month => "30 days",
            Range::Quarter => "90 days",
        }
    }
}

/// What the chart plots.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Measure {
    Cost,
    Tokens,
}

/// How the breakdown table groups.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Model,
    Day,
    Project,
    Session,
}

/// What a group of records used.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tally {
    /// At API prices, for the records whose model has a price.
    pub cost: f64,
    pub tokens: u64,
    /// Tokens of models without a known price (Kimi, GPT through Droid).
    pub unpriced: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub credits: u64,
    pub savings: f64,
}

impl Tally {
    fn add(&mut self, r: &Record) {
        match r.cost() {
            Some(c) => self.cost += c,
            None => self.unpriced += r.tokens(),
        }
        self.tokens += r.tokens();
        self.input += r.input;
        self.output += r.output;
        self.cache_read += r.cache_read;
        self.cache_write += r.cache_write();
        self.credits += r.credits;
        self.savings += r.cache_savings();
    }

    fn value(&self, m: Measure) -> f64 {
        match m {
            Measure::Cost => self.cost,
            Measure::Tokens => self.tokens as f64,
        }
    }
}

/// ((agent, session id), (cwd, title), what it used).
pub type SessionTally = ((AgentKind, String), (PathBuf, String), Tally);

/// The records of a range, added up the ways the tab shows them.
#[derive(Debug, Default)]
pub struct Summary {
    pub total: Tally,
    /// The cost of the same number of days just before the range.
    pub prev_cost: f64,
    /// First and last day of the range, as local midnights.
    pub first: u64,
    pub last: u64,
    /// Largest first.
    pub agents: Vec<(AgentKind, Tally)>,
    /// One per day of the range, oldest first, split by agent.
    pub days: Vec<(u64, HashMap<AgentKind, Tally>)>,
    /// (agent, model), largest first.
    pub models: Vec<((AgentKind, String), Tally)>,
    pub projects: Vec<(String, Tally)>,
    /// Largest first.
    pub sessions_top: Vec<SessionTally>,
    pub sessions: usize,
    pub active_days: usize,
}

/// Midnight (local time) of the day `at` falls in, as unix seconds.
fn day_start(at: u64) -> u64 {
    let Some(t) = crate::platform::local_time(at) else {
        return at - at % DAY;
    };
    at.saturating_sub((t.hour as u64) * 3600 + (t.minute as u64) * 60 + at % 60)
}

fn sorted<K>(map: HashMap<K, Tally>) -> Vec<(K, Tally)> {
    let mut v: Vec<(K, Tally)> = map.into_iter().collect();
    v.sort_by(|a, b| {
        b.1.cost
            .total_cmp(&a.1.cost)
            .then(b.1.tokens.cmp(&a.1.tokens))
    });
    v
}

impl Summary {
    /// `project_of` names the project a working directory belongs to.
    pub fn of(
        records: &[Record],
        range: Range,
        now: u64,
        project_of: impl Fn(&Path) -> String,
    ) -> Self {
        let today = day_start(now);
        let since = today.saturating_sub((range.days() - 1) * DAY);
        let prev_since = since.saturating_sub(range.days() * DAY);
        let mut s = Summary {
            first: since,
            last: today,
            ..Default::default()
        };
        let mut days: HashMap<u64, HashMap<AgentKind, Tally>> = HashMap::new();
        let mut agents: HashMap<AgentKind, Tally> = HashMap::new();
        let mut models: HashMap<(AgentKind, String), Tally> = HashMap::new();
        let mut projects: HashMap<String, Tally> = HashMap::new();
        let mut sessions: HashMap<(AgentKind, String), ((PathBuf, String), Tally)> = HashMap::new();
        for r in records {
            if r.at < since {
                if r.at >= prev_since {
                    s.prev_cost += r.cost().unwrap_or(0.);
                }
                continue;
            }
            s.total.add(r);
            days.entry(day_start(r.at))
                .or_default()
                .entry(r.agent())
                .or_default()
                .add(r);
            agents.entry(r.agent()).or_default().add(r);
            models
                .entry((r.agent(), r.model.clone()))
                .or_default()
                .add(r);
            if !r.cwd.as_os_str().is_empty() {
                projects.entry(project_of(&r.cwd)).or_default().add(r);
            }
            sessions
                .entry((r.agent(), r.session.clone()))
                .or_insert_with(|| ((r.cwd.clone(), r.title.clone()), Tally::default()))
                .1
                .add(r);
        }
        s.sessions = sessions.len();
        s.active_days = days.len();
        // Every day of the range, so quiet days show as gaps. Days are
        // stepped by their midnights, which DST can move by an hour.
        let mut d = since;
        while d <= today {
            let start = day_start(d + 2 * 3600);
            s.days
                .push((start, days.remove(&start).unwrap_or_default()));
            d = start + DAY;
        }
        s.agents = sorted(agents);
        s.models = sorted(models);
        s.projects = sorted(projects);
        let mut top: Vec<_> = sessions
            .into_iter()
            .map(|(k, (cwd, t))| (k, cwd, t))
            .collect();
        top.sort_by(|a, b| {
            b.2.cost
                .total_cmp(&a.2.cost)
                .then(b.2.tokens.cmp(&a.2.tokens))
        });
        s.sessions_top = top;
        s
    }
}

/// What the tab asks the shell to do (its buttons reach the dialogs and
/// terminals only the shell can open).
#[derive(Clone, Copy)]
pub enum UsageEvent {
    /// Open the Factory account dialog (connect or manage the API key).
    OpenAccount,
    /// Run `claude /login` in a terminal.
    ClaudeSignIn,
    /// The plan windows changed; the rail card shows them too.
    PlanUpdated,
}

pub struct UsageView {
    /// The claude.ai plan's windows, when Claude Code is signed in.
    plan: Option<PlanUsage>,
    /// The API key or endpoint Claude Code runs on instead of a plan, if any.
    access: Option<claude_auth::Access>,
    /// Another seat's windows (a borrowed `claude setup-token`), when this
    /// machine has one; shown beside the sign-in's.
    #[cfg(feature = "coderlabs")]
    borrowed: Option<PlanUsage>,
    /// Every Claude seat sessions can run on; shown once a named seat is saved.
    seats: SeatsInfo,
    /// The OpenCode Go plan's windows, when a Go key is connected.
    go: Option<PlanUsage>,
    /// The Devin plan's quota windows, on a plan that has a quota.
    devin: Option<PlanUsage>,
    /// The accounts behind CLIProxyAPI, when Claude Code runs on another endpoint.
    proxy: Option<cliproxy::Proxy>,
    /// CLIProxyAPI's management key, typed in once.
    proxy_key: Entity<TextInput>,
    /// The proxy's address, typed when Claude Code does not run on it.
    proxy_address: Entity<TextInput>,
    /// What the proxy's controls are doing ("Checking…"), while they work.
    proxy_busy: Option<&'static str>,
    /// Why the last save, refresh or forget failed.
    proxy_note: Option<String>,
    /// Counts when the proxy's controls' jobs start and end, so a background
    /// read that overlapped one (say, Forget key) does not put back what it
    /// changed.
    proxy_jobs: u64,
    /// The key field shows although a key is saved (Change key).
    proxy_key_open: bool,
    /// The address field shows (Add CLIProxyAPI, Change address).
    proxy_address_open: bool,
    _proxy_subs: [Subscription; 2],
    /// The Factory account, shared by the shell once it exists.
    account: Option<Entity<Account>>,
    _account_sub: Option<Subscription>,
    /// None until the first scan of the logs is done.
    records: Option<Vec<Record>>,
    scanning: bool,
    range: Range,
    measure: Measure,
    group: Group,
    show_all: bool,
    /// Only this agent's records, picked from the chart's legend.
    agent_filter: Option<AgentKind>,
    /// The daily chart; it keeps its own hover.
    chart: Entity<ui::AreaChart>,
    /// Agent session id → title, from the shell's session list.
    titles: HashMap<String, String>,
    /// Environment paths and the name of their project, longest path first.
    projects: Vec<(PathBuf, String)>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// Whether the view is on screen, for the poll's background task.
    shown: std::sync::Arc<Shown>,
    _poll: [Task<()>; 3],
}

impl EventEmitter<UsageEvent> for UsageView {}

/// Whether the Usage view is on screen, readable off the UI thread.
#[derive(Default)]
struct Shown {
    /// Whether the shell's last frame showed the view. A window that is not
    /// drawn keeps the last value, so `drawn` counts too.
    on_screen: AtomicBool,
    /// When the view was last drawn. Its own poll redraws it every [`POLL`]
    /// while it is on screen, so an older time means it is not.
    drawn: std::sync::Mutex<Option<Instant>>,
}

impl Shown {
    fn now(&self) -> bool {
        self.on_screen.load(Ordering::Relaxed)
            && self
                .drawn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|at| at.elapsed() < 2 * POLL)
    }
}

/// What the seats block shows (see `agent::seats`).
#[derive(Clone, Default, PartialEq)]
struct SeatsInfo {
    seats: Seats,
    /// Claude Code has a claude.ai sign-in: the "own" seat.
    own: bool,
    /// A borrowed token is set.
    borrowed: bool,
    /// Live sessions on each seat.
    live: HashMap<String, usize>,
    /// Why sessions can't get a seat each, if they can't.
    blocked: Option<seats::Blocked>,
}

impl SeatsInfo {
    /// Reads files and maybe the network; keep it off the UI thread.
    fn read(all: Seats) -> Self {
        SeatsInfo {
            seats: all,
            own: plan::own_sign_in(),
            borrowed: plan::borrowed_seat_token().is_some(),
            live: seats::live(),
            blocked: seats::blocked(),
        }
    }
}

impl UsageView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let shown = std::sync::Arc::new(Shown::default());
        let probe_ok = shown.clone();
        let poll = crate::poll::poll_every(cx, Duration::ZERO, POLL, Self::rescan);
        let plan_poll = cx.spawn(async move |this, cx| {
            loop {
                // Reading a named seat sends it a message, which can start its
                // 5-hour window: only while someone looks at them.
                let probe_ok = probe_ok.clone();
                let (info, go, devin, access) = cx
                    .background_spawn(crate::guard::task_or_default(async move {
                        // Asked once the refresh has its turn: the tab may
                        // have closed while another refresh ran.
                        let all = plan::seats_probing_if(&seats::Store::open().names(), || {
                            probe_ok.now()
                        });
                        (
                            SeatsInfo::read(all),
                            opencode_plan::refresh(),
                            devin_plan::refresh(),
                            claude_auth::access(),
                        )
                    }))
                    .await;
                let seats = &info.seats;
                let plan = seats.active().cloned();
                #[cfg(feature = "coderlabs")]
                let borrowed = seats.borrowed.clone();
                if this
                    .update(cx, |v, cx| {
                        #[cfg(feature = "coderlabs")]
                        let changed = v.borrowed != borrowed;
                        #[cfg(not(feature = "coderlabs"))]
                        let changed = false;
                        // The phone's Claude card names the key or endpoint too.
                        let changed = changed || v.access != access;
                        if v.seats != info {
                            v.seats = info;
                            cx.notify();
                        }
                        if changed || v.plan != plan || v.go != go || v.devin != devin {
                            v.access = access;
                            v.plan = plan;
                            #[cfg(feature = "coderlabs")]
                            {
                                v.borrowed = borrowed;
                            }
                            v.go = go;
                            v.devin = devin;
                            cx.emit(UsageEvent::PlanUpdated);
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(PLAN_POLL).await;
            }
        });
        // Its own task: one read per account can take a while, and the
        // other plans should not wait for it.
        let proxy_poll = cx.spawn(async move |this, cx| {
            loop {
                let Ok(jobs) = this.update(cx, |v, _| v.proxy_jobs) else {
                    break;
                };
                let proxy = cx
                    .background_spawn(crate::guard::task(async move { cliproxy::refresh(false) }))
                    .await;
                if this
                    .update(cx, |v, cx| {
                        // A control ran meanwhile: its own read is newer.
                        if v.proxy_busy.is_some() || v.proxy_jobs != jobs {
                            return;
                        }
                        if v.proxy != proxy {
                            // A key typed for the old proxy must not be saved for the new one.
                            if proxy_switched(v.proxy.as_ref(), proxy.as_ref()) {
                                v.proxy_key.update(cx, |i, cx| i.set_text("", cx));
                                v.proxy_key_open = false;
                            }
                            v.proxy = proxy;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(PLAN_POLL).await;
            }
        });
        let proxy_key = cx.new(|cx| TextInput::new("Management key", cx).mono().secret());
        let proxy_key_sub = cx.subscribe(&proxy_key, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Submit) {
                if this.proxy_address_open {
                    this.save_proxy_address(cx);
                } else {
                    this.save_proxy_key(cx);
                }
            }
        });
        let proxy_address = cx.new(|cx| TextInput::new("Address, e.g. 127.0.0.1:8317", cx).mono());
        let proxy_address_sub =
            cx.subscribe(&proxy_address, |this: &mut Self, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Submit) {
                    this.save_proxy_address(cx);
                }
            });
        Self {
            plan: None,
            access: None,
            #[cfg(feature = "coderlabs")]
            borrowed: None,
            seats: SeatsInfo::default(),
            go: None,
            devin: None,
            proxy: None,
            proxy_key,
            proxy_address,
            proxy_busy: None,
            proxy_note: None,
            proxy_jobs: 0,
            proxy_key_open: false,
            proxy_address_open: false,
            _proxy_subs: [proxy_key_sub, proxy_address_sub],
            account: None,
            _account_sub: None,
            records: None,
            scanning: false,
            range: Range::Month,
            measure: Measure::Cost,
            group: Group::Model,
            show_all: false,
            agent_filter: None,
            chart: cx.new(|_| ui::AreaChart::new(230.)),
            titles: HashMap::new(),
            projects: Vec::new(),
            focus: cx.focus_handle(),
            shown,
            scroll: ScrollHandle::new(),
            _poll: [poll, plan_poll, proxy_poll],
        }
    }

    /// Reads the agents' logs again off the main thread; unchanged files come
    /// from the cache.
    fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let records = cx
                .background_spawn(crate::guard::task(async move { usage_history::scan() }))
                .await;
            let _ = this.update(cx, |v, cx| {
                v.scanning = false;
                v.records = Some(records);
                cx.notify();
            });
        })
        .detach();
    }

    /// What the shell knows: session titles and where its projects live.
    pub fn set_context(
        &mut self,
        titles: HashMap<String, String>,
        mut projects: Vec<(PathBuf, String)>,
        cx: &mut Context<Self>,
    ) {
        projects.sort_by_key(|p| std::cmp::Reverse(p.0.as_os_str().len()));
        if titles != self.titles || projects != self.projects {
            self.titles = titles;
            self.projects = projects;
            cx.notify();
        }
    }

    /// Tells the view whether the shell's frame shows it. Named seats are
    /// read only while it does: a read sends the seat a message.
    pub fn set_on_screen(&self, shown: bool) {
        self.shown.on_screen.store(shown, Ordering::Relaxed);
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    /// The Factory account, once the shell shares it: Droid's plan usage joins
    /// the tab next to Claude's.
    pub fn set_account(&mut self, account: Entity<Account>, cx: &mut Context<Self>) {
        if self.account.is_some() {
            return;
        }
        account.update(cx, |a, cx| a.refresh_if_stale(Duration::from_secs(60), cx));
        self._account_sub = Some(cx.observe(&account, |_, _, cx| cx.notify()));
        self.account = Some(account);
    }

    fn project_of(&self, cwd: &Path) -> String {
        self.projects
            .iter()
            .find(|(p, _)| cwd.starts_with(p))
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| {
                cwd.file_name().map_or_else(
                    || cwd.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                )
            })
    }

    // ---- plan limits -----------------------------------------------------------

    /// Claude's limits: one line per active window, the idle ones folded into
    /// a single line, or the sign-in prompt.
    fn claude_limits(&self, cx: &mut Context<Self>) -> AnyElement {
        let label = match (&self.plan, &self.access) {
            (Some(p), Some(a)) if !p.plan_label().is_empty() => {
                format!("{} · runs on {}", p.plan_label(), a.label())
            }
            (Some(p), None) => p.plan_label(),
            (_, Some(a)) => a.label(),
            (None, None) => String::new(),
        };
        let at = now();
        let body = match &self.plan {
            Some(plan) => {
                let mut rows = div().flex().flex_col().gap(crate::ui::sp(8.));
                let mut idle = Vec::new();
                for w in &plan.windows {
                    if w.active || w.percent > 0 {
                        rows = rows.child(ui::limit_line(
                            &w.label,
                            w.percent as f32,
                            w.reset_text(at),
                        ));
                    } else {
                        idle.push(w.label.clone());
                    }
                }
                if let Some(spend) = &plan.spend
                    && (spend.used > 0. || spend.limit.is_some())
                {
                    let detail = match spend.limit {
                        Some(limit) => format!("of {}", fmt_cost(limit, &spend.currency, false)),
                        None => format!(
                            "{} this month",
                            fmt_cost(spend.used, &spend.currency, false)
                        ),
                    };
                    rows = rows.child(ui::limit_line(
                        "Extra usage",
                        spend.percent as f32,
                        Some(detail),
                    ));
                }
                rows.children(idle_line(&idle))
            }
            None if self.access.is_some() => connect_hint(
                "usage-claude-sign-in",
                "Claude Code runs on an API key or another endpoint, which has no claude.ai plan limits. Sign in to see a plan's 5-hour and weekly limits.",
                "Sign in",
                UsageEvent::ClaudeSignIn,
                cx,
            ),
            None => connect_hint(
                "usage-claude-sign-in",
                "Sign in with your claude.ai account to see your 5-hour and weekly limits.",
                "Sign in",
                UsageEvent::ClaudeSignIn,
                cx,
            ),
        };
        limits_block(
            AgentKind::Claude,
            "Claude Code",
            (!label.is_empty()).then(|| label.into_any_element()),
            body,
        )
    }

    /// Runs one of the proxy's controls off the UI thread: `busy` labels it
    /// meanwhile, and what `job` reads is what the tab shows after.
    fn proxy_job(
        &mut self,
        busy: &'static str,
        job: impl FnOnce() -> anyhow::Result<Option<cliproxy::Proxy>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.proxy_busy.is_some() {
            return;
        }
        self.proxy_busy = Some(busy);
        self.proxy_jobs += 1;
        self.proxy_note = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(crate::guard::task(async move { job() }))
                .await;
            let _ = this.update(cx, |v, cx| {
                v.proxy_busy = None;
                // A poll that began while this ran may have read before it
                // wrote: count the end too, so that poll's result is dropped.
                v.proxy_jobs += 1;
                match got {
                    Ok(proxy) => {
                        v.proxy = proxy;
                        v.proxy_key_open = false;
                        v.proxy_address_open = false;
                        v.proxy_key.update(cx, |i, cx| i.set_text("", cx));
                        v.proxy_address.update(cx, |i, cx| i.set_text("", cx));
                    }
                    Err(e) => v.proxy_note = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the address form or the key field. The key field is shared, and
    /// a key typed for one proxy must not be saved for another, so it starts
    /// empty whenever the form changes.
    fn open_proxy_form(&mut self, address: bool, cx: &mut Context<Self>) {
        if self.proxy_address_open != address {
            self.proxy_key.update(cx, |i, cx| i.set_text("", cx));
        }
        self.proxy_address_open = address;
        self.proxy_key_open = !address;
        cx.notify();
    }

    /// Saves the typed management key and reads the proxy's accounts with it.
    fn save_proxy_key(&mut self, cx: &mut Context<Self>) {
        let key = self.proxy_key.read(cx).text().trim().to_string();
        // The proxy on screen when the key was typed is the one it is for.
        let Some(base) = self.proxy.as_ref().map(|p| p.base.clone()) else {
            return;
        };
        if key.is_empty() {
            return;
        }
        self.proxy_job(
            "Checking…",
            move || {
                cliproxy::save_key(&key, &base)?;
                Ok(cliproxy::refresh(true))
            },
            cx,
        );
    }

    /// Saves the typed address, and the key typed with it, then reads.
    fn save_proxy_address(&mut self, cx: &mut Context<Self>) {
        let address = self.proxy_address.read(cx).text().trim().to_string();
        let key = self.proxy_key.read(cx).text().trim().to_string();
        self.proxy_job(
            "Checking…",
            move || {
                let base = cliproxy::save_address(&address)?;
                if !key.is_empty() {
                    cliproxy::save_key(&key, &base)?;
                }
                Ok(cliproxy::refresh(true))
            },
            cx,
        );
    }

    /// The claude.ai and ChatGPT accounts behind CLIProxyAPI, with how the
    /// pool is doing, each account's health and windows, and the controls for
    /// the key and address. Without a proxy, a quiet way to add one.
    fn proxy_limits(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(proxy) = self.proxy.as_ref() else {
            if !self.proxy_address_open {
                return Some(
                    div()
                        .flex()
                        .child(
                            ui::button(
                                "usage-cliproxy-add",
                                "Add CLIProxyAPI",
                                ButtonKind::Ghost,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_proxy_form(true, cx);
                            })),
                        )
                        .into_any_element(),
                );
            }
            let rows = div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(10.))
                .child(proxy_note(
                    "Add the address and management key of a CLIProxyAPI to see the 5-hour and weekly limits of the Claude and Codex accounts behind it.".into(),
                    theme::text_subtle(),
                ))
                .when_some(self.proxy_note.as_ref(), |d, note| {
                    d.child(proxy_note(
                        crate::redact::text(note).into_owned(),
                        theme::error().into(),
                    ))
                })
                .child(self.proxy_address_form(cx));
            return Some(limits_block(AgentKind::Claude, "CLIProxyAPI", None, rows));
        };
        let at = now();
        let mut rows = div().flex().flex_col().gap(crate::ui::sp(14.));
        let mut providers: Vec<cliproxy::Provider> =
            proxy.accounts.iter().map(|a| a.provider).collect();
        providers.dedup();
        let several = providers.len() > 1;
        for provider in providers {
            let mut group = div().flex().flex_col().gap(crate::ui::sp(12.));
            if several {
                let kind = match provider {
                    cliproxy::Provider::Claude => AgentKind::Claude,
                    cliproxy::Provider::Codex => AgentKind::Codex,
                };
                group = group.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .child(kind.icon(12.))
                        .child(
                            div()
                                .text_size(px(ui::TEXT_SM))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::content())
                                .child(provider.name()),
                        ),
                );
            }
            if let Some(summary) = cliproxy::summary(&proxy.accounts, provider, at) {
                group = group.child(proxy_note(summary, theme::text_subtle()));
            }
            for a in proxy.accounts.iter().filter(|a| a.provider == provider) {
                group = group.child(proxy_account(a, at));
            }
            rows = rows.child(group);
        }
        let host = crate::redact::value(proxy.host()).to_string();
        if let Some(e) = &proxy.error {
            rows = rows.child(proxy_note(
                crate::redact::text(e).into_owned(),
                theme::error().into(),
            ));
        } else if proxy.has_key && proxy.accounts.is_empty() && proxy.at > 0 {
            rows = rows.child(proxy_note(
                "CLIProxyAPI has no Claude or Codex accounts signed in.".into(),
                theme::text_subtle(),
            ));
        }
        if let Some(note) = &self.proxy_note {
            rows = rows.child(proxy_note(
                crate::redact::text(note).into_owned(),
                theme::error().into(),
            ));
        }
        let busy = self.proxy_busy;
        if self.proxy_address_open {
            rows = rows.child(self.proxy_address_form(cx));
        } else if !proxy.has_key
            || proxy.key_refused()
            || self.proxy_key_open
            || (proxy.accounts.is_empty() && proxy.error.is_some())
        {
            if !proxy.has_key {
                let source = if proxy.typed {
                    format!("CLIProxyAPI at {host}. Add")
                } else {
                    format!("Claude Code runs on {host}. If that is CLIProxyAPI, add")
                };
                rows = rows.child(proxy_note(
                    format!("{source} its management key to see each account's 5-hour and weekly limits."),
                    theme::text_subtle(),
                ));
            }
            rows = rows.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(ui::field().flex_1().min_w_0().child(self.proxy_key.clone()))
                    .child(
                        ui::button(
                            "usage-cliproxy-key",
                            busy.unwrap_or("Save"),
                            ButtonKind::Outline,
                            busy.is_none(),
                        )
                        .when(busy.is_none(), |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.save_proxy_key(cx)))
                        }),
                    ),
            );
        }
        rows = rows.child(self.proxy_controls(proxy, at, cx));
        Some(limits_block(
            AgentKind::Claude,
            "CLIProxyAPI",
            Some(host.into_any_element()),
            rows,
        ))
    }

    /// The address and key fields, with Save and Cancel.
    fn proxy_address_form(&self, cx: &mut Context<Self>) -> gpui::Div {
        let busy = self.proxy_busy;
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .child(ui::field().child(self.proxy_address.clone()))
            .child(ui::field().child(self.proxy_key.clone()))
            .child(
                div()
                    .flex()
                    .gap(crate::ui::sp(8.))
                    .child(
                        ui::button(
                            "usage-cliproxy-address-save",
                            busy.unwrap_or("Save"),
                            ButtonKind::Outline,
                            busy.is_none(),
                        )
                        .when(busy.is_none(), |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.save_proxy_address(cx)))
                        }),
                    )
                    .child(
                        ui::button(
                            "usage-cliproxy-address-cancel",
                            "Cancel",
                            ButtonKind::Ghost,
                            busy.is_none(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.proxy_address_open = false;
                            this.proxy_note = None;
                            this.proxy_key.update(cx, |i, cx| i.set_text("", cx));
                            cx.notify();
                        })),
                    ),
            )
    }

    /// When the accounts were read, and Refresh, Change key, Forget key and
    /// the address controls.
    fn proxy_controls(
        &self,
        proxy: &cliproxy::Proxy,
        at: u64,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let idle = self.proxy_busy.is_none();
        let ghost =
            |id: &'static str, label: &'static str| ui::button(id, label, ButtonKind::Ghost, idle);
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(4.))
            .when(proxy.has_key && proxy.at > 0, |d| {
                d.child(
                    div()
                        .mr(crate::ui::sp(4.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(match self.proxy_busy {
                            Some("Reading…") => "Reading…".to_string(),
                            _ => format!("Read {}", crate::account::ago(proxy.at, at)),
                        }),
                )
            })
            .when(proxy.has_key && !proxy.key_refused(), |d| {
                d.child(ghost("usage-cliproxy-refresh", "Refresh").when(idle, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| {
                        this.proxy_job("Reading…", || Ok(cliproxy::refresh(true)), cx);
                    }))
                }))
            })
            .when(
                proxy.has_key && !proxy.key_refused() && !self.proxy_key_open,
                |d| {
                    d.child(
                        ghost("usage-cliproxy-change-key", "Change key").on_click(
                            cx.listener(|this, _, _, cx| this.open_proxy_form(false, cx)),
                        ),
                    )
                },
            )
            .when(self.proxy_key_open, |d| {
                d.child(
                    ghost("usage-cliproxy-cancel-key", "Cancel").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.proxy_key_open = false;
                            this.proxy_key.update(cx, |i, cx| i.set_text("", cx));
                            cx.notify();
                        },
                    )),
                )
            })
            .when(proxy.has_key, |d| {
                d.child(
                    ghost("usage-cliproxy-forget-key", "Forget key").when(idle, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| {
                            this.proxy_job(
                                "Forgetting…",
                                || {
                                    cliproxy::forget_key()?;
                                    Ok(cliproxy::refresh(false))
                                },
                                cx,
                            );
                        }))
                    }),
                )
            })
            .when(!self.proxy_address_open, |d| {
                d.child(
                    ghost("usage-cliproxy-change-address", "Change address")
                        .on_click(cx.listener(|this, _, _, cx| this.open_proxy_form(true, cx))),
                )
            })
            .when(proxy.typed, |d| {
                d.child(
                    ghost("usage-cliproxy-forget-address", "Forget address").when(idle, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| {
                            this.proxy_job(
                                "Forgetting…",
                                || {
                                    cliproxy::forget_address()?;
                                    Ok(cliproxy::refresh(false))
                                },
                                cx,
                            );
                        }))
                    }),
                )
            })
    }

    /// A second seat's windows: only some builds read one.
    #[cfg(not(feature = "coderlabs"))]
    fn borrowed_limits(&self) -> Option<AnyElement> {
        None
    }

    /// The borrowed seat's windows: another account's long-lived token (the
    /// lab's, a teammate's), which only answers with rate-limit headers.
    #[cfg(feature = "coderlabs")]
    fn borrowed_limits(&self) -> Option<AnyElement> {
        let b = self.borrowed.as_ref()?;
        let at = now();
        let mut rows = div().flex().flex_col().gap(crate::ui::sp(8.));
        for w in &b.windows {
            rows = rows.child(ui::limit_line(&w.label, w.percent as f32, w.reset_text(at)));
        }
        let name = if plan::active_is_borrowed() {
            "Borrowed seat · in use"
        } else {
            "Borrowed seat"
        };
        Some(limits_block(AgentKind::Claude, name, None, rows))
    }

    /// Every Claude seat with its 5-hour and weekly windows and the live
    /// sessions on it, once a named seat is saved (`arakun seat add`). Says so
    /// when sessions can't get a seat each.
    fn seat_limits(&self) -> Option<AnyElement> {
        let info = &self.seats;
        if info.seats.named.is_empty() {
            return None;
        }
        let at = now();
        let mut rows = div().flex().flex_col().gap(crate::ui::sp(14.));
        if let Some(blocked) = &info.blocked {
            rows = rows.child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .line_height(px(17.))
                    .text_color(theme::warn())
                    .child(blocked.notice()),
            );
        }
        let mut list: Vec<(&str, Option<&PlanUsage>, Option<&str>)> = Vec::new();
        if info.own {
            list.push((seats::OWN, info.seats.own.as_ref(), None));
        }
        if info.borrowed {
            list.push((seats::BORROWED, info.seats.borrowed.as_ref(), None));
        }
        for s in &info.seats.named {
            list.push((&s.name, s.usage.as_ref(), s.error.as_deref()));
        }
        for (id, usage, error) in list {
            let live = info.live.get(id).copied().unwrap_or(0);
            let name = match id {
                seats::OWN | seats::BORROWED => seats::label(id),
                name => crate::redact::value(name),
            };
            let mut seat = div().flex().flex_col().gap(crate::ui::sp(8.)).child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(ui::TEXT_SM))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_muted())
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(if live > 0 {
                                theme::text_muted()
                            } else {
                                theme::text_faint()
                            })
                            .child(match live {
                                0 => "No live sessions".to_string(),
                                1 => "1 live session".to_string(),
                                n => format!("{n} live sessions"),
                            }),
                    ),
            );
            let windows: Vec<_> = usage
                .map(|u| {
                    [u.five_hour_window(), u.weekly()]
                        .into_iter()
                        .flatten()
                        .collect()
                })
                .unwrap_or_default();
            for w in &windows {
                seat = seat.child(ui::limit_line(&w.label, w.percent as f32, w.reset_text(at)));
            }
            let quiet = match (windows.is_empty(), error) {
                (true, Some(e)) => Some((
                    format!("Couldn't read it: {}", crate::redact::text(e)),
                    theme::error(),
                )),
                (true, None) => Some(("Not read yet.".to_string(), theme::text_subtle().into())),
                // The windows above are the last good read; say why this one
                // failed, or a refused seat would look like any other.
                (false, Some(e)) => Some((
                    format!("Couldn't refresh it: {}", crate::redact::text(e)),
                    theme::error(),
                )),
                (false, None) => None,
            };
            if let Some((text, color)) = quiet {
                seat = seat.child(
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(color)
                        .child(text),
                );
            }
            rows = rows.child(seat);
        }
        let total: usize = info.live.values().sum();
        Some(limits_block(
            AgentKind::Claude,
            "Claude seats",
            (total > 0).then(|| format!("{total} live").into_any_element()),
            rows,
        ))
    }

    /// OpenCode Go's limits: its three windows, each with the endpoint's
    /// status when it isn't "ok". Nothing without a Go key.
    fn opencode_limits(&self) -> Option<AnyElement> {
        let go = self.go.as_ref()?;
        let at = now();
        let mut rows = div().flex().flex_col().gap(crate::ui::sp(8.));
        for w in &go.windows {
            let detail = [
                opencode_plan::status(w).map(str::to_string),
                w.reset_text(at),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            rows = rows.child(ui::limit_line(
                &w.label,
                w.percent as f32,
                (!detail.is_empty()).then(|| detail.join(" · ")),
            ));
        }
        if go.windows.is_empty() {
            rows = rows.child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child("No usage windows reported yet."),
            );
        }
        Some(limits_block(AgentKind::OpenCode, "OpenCode Go", None, rows))
    }

    /// Devin's quota windows, as `/usage` in the devin CLI shows them.
    /// Nothing when signed out or on a plan billed without a quota.
    fn devin_limits(&self) -> Option<AnyElement> {
        let devin = self.devin.as_ref()?;
        let at = now();
        let mut rows = div().flex().flex_col().gap(crate::ui::sp(8.));
        for w in &devin.windows {
            rows = rows.child(ui::limit_line(&w.label, w.percent as f32, w.reset_text(at)));
        }
        let name = match devin.subscription.as_str() {
            "" => "Devin".to_string(),
            plan => format!("Devin · {plan}"),
        };
        Some(limits_block(AgentKind::Devin, &name, None, rows))
    }

    /// Droid's limits: the pool in use in full, the other one in a line, or
    /// the connect prompt.
    fn droid_limits(&self, cx: &mut Context<Self>) -> AnyElement {
        let prompt = |cx: &mut Context<Self>| {
            connect_hint(
                "usage-droid-connect",
                "Sign in to droid or connect a Factory API key to see Droid's plan limits.",
                "Connect",
                UsageEvent::OpenAccount,
                cx,
            )
        };
        let Some(account) = &self.account else {
            return limits_block(AgentKind::Droid, "Droid", None, prompt(cx));
        };
        let account = account.read(cx);
        if !account.connected() {
            return limits_block(AgentKind::Droid, "Droid", None, prompt(cx));
        }
        let at = now();
        let manage = div()
            .id("usage-droid-manage")
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .cursor_pointer()
            .hover(|s| s.text_color(theme::content()))
            .child("Manage")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(UsageEvent::OpenAccount)));
        let body = match account.limits() {
            Some(limits) => {
                let mode = limits.mode(at);
                let core_first = mode == UsageMode::DroidCore && limits.core.is_some();
                let (main, other) = if core_first {
                    (
                        limits.core.as_ref().map(|p| ("Core", p)),
                        limits.standard.as_ref().map(|p| ("Standard", p)),
                    )
                } else {
                    (
                        limits.standard.as_ref().map(|p| ("Standard", p)),
                        limits.core.as_ref().map(|p| ("Core", p)),
                    )
                };
                let mut col = div().flex().flex_col().gap(crate::ui::sp(8.));
                let mut idle = Vec::new();
                if let Some((name, pool)) = main {
                    for (label, b) in pool.buckets() {
                        if b.active(at) {
                            col = col.child(ui::limit_line(
                                &format!("{name} · {label}"),
                                b.current(at),
                                Some(account_view::reset_label(&b, at)),
                            ));
                        } else {
                            idle.push(format!("{name} · {label}"));
                        }
                    }
                }
                // The pool not in use: its fullest window, in one line.
                if let Some((name, pool)) = other {
                    let fullest = pool
                        .buckets()
                        .into_iter()
                        .filter(|(_, b)| b.active(at))
                        .max_by(|a, b| a.1.used_percent.total_cmp(&b.1.used_percent));
                    if let Some((label, b)) = fullest {
                        col = col.child(ui::limit_line(
                            &format!("{name} · {label}"),
                            b.current(at),
                            Some(account_view::reset_label(&b, at)),
                        ));
                    }
                }
                if limits.extra_usage_cents > 0 {
                    col = col.child(
                        div()
                            .flex()
                            .text_size(px(ui::TEXT_SM))
                            .child(
                                div()
                                    .flex_1()
                                    .text_color(theme::text_subtle())
                                    .child("Extra usage balance"),
                            )
                            .child(format!("${:.2}", limits.extra_usage_cents as f64 / 100.)),
                    );
                }
                col.children(idle_line(&idle)).child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .line_height(px(16.))
                        .text_color(match mode {
                            UsageMode::Blocked => theme::danger().into(),
                            UsageMode::Standard => theme::mix(0.45),
                            _ => theme::warn().into(),
                        })
                        .child(limits.summary(at)),
                )
            }
            None => div()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(match account.error() {
                    Some(e) => e.to_string(),
                    None => "Reading usage…".to_string(),
                }),
        };
        limits_block(
            AgentKind::Droid,
            "Droid",
            Some(manage.into_any_element()),
            body,
        )
    }

    // ---- breakdown ---------------------------------------------------------------

    /// Rows of the breakdown table: (icon, name, tally).
    fn breakdown_rows(&self, s: &Summary) -> Vec<(Option<AgentKind>, String, Tally)> {
        match self.group {
            Group::Model => s
                .models
                .iter()
                .map(|((agent, model), t)| (Some(*agent), model_name(model), t.clone()))
                .collect(),
            Group::Project => s
                .projects
                .iter()
                .map(|(p, t)| (None, p.clone(), t.clone()))
                .collect(),
            Group::Session => s
                .sessions_top
                .iter()
                .map(|((agent, id), (cwd, title), t)| {
                    let own = (!title.is_empty()).then(|| title.clone());
                    let name = self
                        .titles
                        .get(id)
                        .filter(|n| !n.is_empty())
                        .cloned()
                        .or(own)
                        .unwrap_or_else(|| {
                            let project = if cwd.as_os_str().is_empty() {
                                "session".to_string()
                            } else {
                                self.project_of(cwd)
                            };
                            format!("{project} · {}", id.get(..8).unwrap_or(id))
                        });
                    (Some(*agent), name, t.clone())
                })
                .collect(),
            Group::Day => s
                .days
                .iter()
                .rev()
                .filter(|(_, by)| !by.is_empty())
                .map(|(day, by)| {
                    let mut t = Tally::default();
                    for a in by.values() {
                        merge(&mut t, a);
                    }
                    (None, long_date(*day), t)
                })
                .collect(),
        }
    }

    // ---- chart -----------------------------------------------------------------------

    /// Hands the range's days to the chart: one series per agent, the day's
    /// name, and what the other measure says about it.
    fn feed_chart(&self, s: &Summary, cx: &mut Context<Self>) {
        let measure = self.measure;
        let series = s
            .agents
            .iter()
            .map(|(agent, _)| ui::Series {
                name: agent.label().into(),
                color: agent_color(*agent),
                values: s
                    .days
                    .iter()
                    .map(|(_, by)| by.get(agent).map_or(0., |t| t.value(measure)))
                    .collect(),
            })
            .collect();
        let labels = s.days.iter().map(|(d, _)| long_date(*d).into()).collect();
        let notes = s
            .days
            .iter()
            .map(|(_, by)| {
                let mut t = Tally::default();
                for a in by.values() {
                    merge(&mut t, a);
                }
                Some(match measure {
                    _ if t.tokens == 0 => "No usage".into(),
                    Measure::Cost if t.credits > 0 => {
                        format!("{} tokens · {} credits", count(t.tokens), count(t.credits)).into()
                    }
                    Measure::Cost => format!("{} tokens", count(t.tokens)).into(),
                    Measure::Tokens => format!("{} at API prices", usd(t.cost)).into(),
                })
            })
            .collect();
        let n = s.days.len();
        let ticks = if n > 0 {
            vec![
                short_date(s.days[0].0).into(),
                short_date(s.days[n / 2].0).into(),
                short_date(s.days[n - 1].0).into(),
            ]
        } else {
            Vec::new()
        };
        let (format, axis): (ui::Format, ui::Format) = match measure {
            Measure::Cost => (Rc::new(usd), Rc::new(usd_short)),
            Measure::Tokens => (
                Rc::new(|v| format!("{} tok", count(v as u64))),
                Rc::new(|v| count(v as u64)),
            ),
        };
        self.chart.update(cx, |c, cx| {
            c.set(series, labels, ticks, notes, format, axis, cx)
        });
    }
}

fn merge(into: &mut Tally, t: &Tally) {
    into.cost += t.cost;
    into.tokens += t.tokens;
    into.unpriced += t.unpriced;
    into.input += t.input;
    into.output += t.output;
    into.cache_read += t.cache_read;
    into.cache_write += t.cache_write;
    into.credits += t.credits;
    into.savings += t.savings;
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---- formatting ------------------------------------------------------------------

/// "$7,576.97".
fn usd(x: f64) -> String {
    let cents = (x * 100.).round() as u64;
    format!("${}.{:02}", thousands(cents / 100), cents % 100)
}

/// "$800", for the axis.
fn usd_short(x: f64) -> String {
    if x >= 1000. {
        format!("${}k", trim(x / 1000.))
    } else if x >= 10. || x.fract() == 0. {
        format!("${}", thousands(x.round() as u64))
    } else {
        format!("${x:.2}")
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn trim(x: f64) -> String {
    let s = if x >= 100. {
        format!("{x:.0}")
    } else if x >= 10. {
        format!("{x:.1}")
    } else {
        format!("{x:.2}")
    };
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// "4.11B", "181M", "450k".
fn count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{}k", trim(n as f64 / 1e3)),
        1_000_000..1_000_000_000 => format!("{}M", trim(n as f64 / 1e6)),
        _ => format!("{}B", trim(n as f64 / 1e9)),
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn pct(part: f64, whole: f64) -> String {
    if whole <= 0. {
        return "—".into();
    }
    format!("{:.1}%", part * 100. / whole)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// "Oct 25".
fn short_date(t: u64) -> String {
    crate::platform::local_time(t)
        .map(|l| format!("{} {}", MONTHS[l.month.clamp(0, 11) as usize], l.day))
        .unwrap_or_default()
}

/// "Sat, Oct 25".
fn long_date(t: u64) -> String {
    crate::platform::local_time(t)
        .map(|l| {
            format!(
                "{}, {} {}",
                WEEKDAYS[l.wday.clamp(0, 6) as usize],
                MONTHS[l.month.clamp(0, 11) as usize],
                l.day
            )
        })
        .unwrap_or_default()
}

/// How a model reads in the table: `claude-haiku-4-5-20251001` → `Haiku 4.5`,
/// `kimi-k3` → `Kimi K3`; ids of other shapes stay as they are.
fn model_name(id: &str) -> String {
    let id = match id.rsplit_once('-') {
        Some((base, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => id,
    };
    let cap = |w: &str| {
        let mut c = w.chars();
        c.next()
            .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
    };
    if let Some(rest) = id.strip_prefix("claude-") {
        let mut parts = rest.split('-');
        let family = parts.next().map(cap).unwrap_or_default();
        let version: Vec<&str> = parts.collect();
        if !version.is_empty()
            && version
                .iter()
                .all(|p| p.bytes().all(|b| b.is_ascii_digit()))
        {
            return format!("{family} {}", version.join("."));
        }
    }
    if let Some((name, rest)) = id.split_once('-')
        && !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphabetic())
        && !rest.contains('-')
        && name != "gpt"
    {
        return format!("{} {}", cap(name), rest.to_uppercase());
    }
    id.to_string()
}

/// Which hue each agent's series and share bar wear: Anthropic's clay for
/// Claude Code, a quiet light gray for Droid. Darker grays keep Cursor,
/// Antigravity, Devin, OpenCode and Codex apart.
fn agent_color(agent: AgentKind) -> Hsla {
    match agent {
        AgentKind::Claude => gpui::rgb(0xd97757).into(),
        AgentKind::Droid => theme::mix(0.72),
        AgentKind::OpenCode => theme::mix(0.45),
        AgentKind::Codex => theme::mix(0.3),
        AgentKind::Devin => theme::mix(0.2),
        AgentKind::Cursor => theme::mix(0.58),
        AgentKind::Antigravity => theme::mix(0.38),
        AgentKind::WorkersAi => gpui::rgb(0xf38020).into(),
    }
}

// ---- pieces ------------------------------------------------------------------------

/// One agent in the hero: name and amount, its share as a bar, and the detail.
fn agent_share(agent: AgentKind, t: &Tally, total: &Tally) -> AnyElement {
    let by_cost = total.cost > 0.;
    let share = if by_cost {
        t.cost / total.cost
    } else {
        t.tokens as f64 / total.tokens.max(1) as f64
    };
    let mut detail = vec![
        format!(
            "{} of {}",
            pct(share, 1.),
            if by_cost { "cost" } else { "tokens" }
        ),
        format!("{} tokens", count(t.tokens)),
    ];
    if t.credits > 0 {
        detail.push(format!("{} credits", count(t.credits)));
    }
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(7.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .text_size(px(ui::TEXT_LG))
                .child(agent.icon(15.))
                .child(
                    div()
                        .flex_1()
                        .text_color(theme::content())
                        .child(agent.label()),
                )
                .child(div().text_color(theme::content()).child(if by_cost {
                    usd(t.cost)
                } else {
                    count(t.tokens)
                })),
        )
        .child(
            div()
                .h(px(4.))
                .rounded_full()
                .bg(theme::fill_hover())
                .child(
                    div()
                        .h_full()
                        .rounded_full()
                        .bg(agent_color(agent))
                        .w(relative((share as f32).clamp(0., 1.)))
                        .when(share > 0., |d| d.min_w(px(4.))),
                ),
        )
        .child(
            div()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(detail.join(" · ")),
        )
        .into_any_element()
}

/// One cell of the stat strip.
/// One cell of the stat strip: the kit's stat and the range's trend beside it.
fn stat(label: &str, value: String, sub: String, trend: Vec<f64>, last: bool) -> AnyElement {
    let moving = trend.iter().filter(|v| **v > 0.).count() >= 2;
    div()
        .flex_1()
        .min_w(px(160.))
        .flex()
        .items_end()
        .justify_between()
        .gap(crate::ui::sp(8.))
        .px(crate::ui::sp(16.))
        .py(crate::ui::sp(14.))
        .when(!last, |d| {
            d.border_r_1().border_color(theme::border_subtle())
        })
        .child(ui::stat(label, value, sub))
        .when(moving, |d| {
            d.child(ui::sparkline(trend, theme::accent(), 56., 24.))
        })
        .into_any_element()
}

/// A plan's block in the limits column: brand, name, plan badge, its rows.
fn limits_block(
    agent: AgentKind,
    title: &str,
    right: Option<AnyElement>,
    body: gpui::Div,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(14.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .child(agent.icon(14.))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(ui::TEXT_MD))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::content())
                        .child(title.to_string()),
                )
                .children(right.map(|r| {
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_subtle())
                        .child(r)
                })),
        )
        .child(body)
        .into_any_element()
}

/// A provider that is not connected yet: what it would show, and the button.
fn connect_hint(
    id: &'static str,
    hint: &str,
    button: &str,
    event: UsageEvent,
    cx: &mut Context<UsageView>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap(crate::ui::sp(10.))
        .child(
            div()
                .text_size(px(ui::TEXT_SM))
                .line_height(px(17.))
                .text_color(theme::text_subtle())
                .child(hint.to_string()),
        )
        .child(
            ui::button(id, button, ButtonKind::Outline, true)
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(event))),
        )
}

/// The windows that are not running, folded into one quiet line.
fn idle_line(labels: &[String]) -> Option<AnyElement> {
    (!labels.is_empty()).then(|| {
        div()
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_faint())
            .child(format!("Not active: {}", labels.join(", ")))
            .into_any_element()
    })
}

/// A line of small text in the proxy's block.
fn proxy_note(text: String, color: Hsla) -> gpui::Div {
    div()
        .text_size(px(ui::TEXT_SM))
        .line_height(px(17.))
        .text_color(color)
        .child(text)
}

/// One account behind CLIProxyAPI: its name and plan, whether the proxy
/// routes to it, and its windows.
fn proxy_account(a: &cliproxy::Account, at: u64) -> AnyElement {
    let mut account = div().flex().flex_col().gap(crate::ui::sp(8.)).child(
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ui::TEXT_SM))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_muted())
                    .child(crate::redact::value(&a.name).to_string()),
            )
            .when(!a.plan.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(proxy_plan(&a.plan)),
                )
            }),
    );
    let state = match &a.state {
        cliproxy::State::Ready => None,
        cliproxy::State::Cooling { until, why } => {
            let mut text = match until.and_then(|t| plan::when_text(t, at)) {
                Some(when) => format!("Cooling down, back {when}"),
                None => "Cooling down".to_string(),
            };
            if !why.is_empty() {
                text.push_str(&format!(" · {why}"));
            }
            Some((text, theme::warn().into()))
        }
        cliproxy::State::SignIn => Some((
            "Signed out on the proxy. Sign in to this account there again.".to_string(),
            theme::error().into(),
        )),
        cliproxy::State::Failing { why } => Some((why.clone(), theme::warn().into())),
    };
    if let Some((text, color)) = state {
        account = account.child(proxy_note(crate::redact::text(&text).into_owned(), color));
    }
    match (&a.usage, &a.error) {
        (Some(u), _) => {
            let mut idle = Vec::new();
            for w in &u.windows {
                if w.active || w.percent > 0 {
                    account =
                        account.child(ui::limit_line(&w.label, w.percent as f32, w.reset_text(at)));
                } else {
                    idle.push(w.label.clone());
                }
            }
            account = account.children(idle_line(&idle));
        }
        (None, Some(e)) => {
            account = account.child(proxy_note(
                crate::redact::text(e).into_owned(),
                theme::text_subtle(),
            ));
        }
        // Signed out: the state line says it all.
        (None, None) if a.state == cliproxy::State::SignIn => {}
        (None, None) => {
            account = account.child(proxy_note(
                "No windows reported.".into(),
                theme::text_subtle(),
            ));
        }
    }
    account.into_any_element()
}

/// "Pro", "Plus", "Team": a plan id as the proxy reports it, for a badge.
fn proxy_plan(plan: &str) -> String {
    let mut words = plan.split('_').filter(|w| !w.is_empty());
    let Some(first) = words.next() else {
        return String::new();
    };
    let mut c = first.chars();
    let first: String = c
        .next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default();
    std::iter::once(first)
        .chain(words.map(String::from))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The breakdown table: name, cost, share of cost and tokens, in the quiet
/// column rhythm of a ledger.
fn breakdown_table(
    rows: &[(Option<AgentKind>, String, Tally)],
    total: &Tally,
    header: &str,
) -> AnyElement {
    let cell = |w: f32| div().w(px(w)).flex_none().flex().justify_end();
    let mut list = div().flex().flex_col().child(
        div()
            .flex()
            .items_center()
            .h(px(32.))
            .border_b_1()
            .border_color(theme::border())
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .child(div().flex_1().child(header.to_string()))
            .child(cell(96.).child("Cost"))
            .child(cell(72.).child("Share"))
            .child(cell(84.).child("Tokens")),
    );
    for (agent, name, t) in rows {
        let priced = t.cost > 0. || t.unpriced == 0;
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .h(px(40.))
                .px(crate::ui::sp(6.))
                .mx(px(-6.))
                .rounded(crate::ui::rad(ui::RADIUS_SM))
                .border_b_1()
                .border_color(theme::border_subtle())
                .hover(|d| d.bg(theme::fill_raised()))
                .text_size(px(ui::TEXT_MD))
                .children(agent.map(|a| a.icon(13.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::content())
                        .child(name.clone()),
                )
                .child(
                    cell(96.)
                        .text_color(if priced {
                            theme::content()
                        } else {
                            theme::mix(0.35)
                        })
                        .child(if priced {
                            usd(t.cost)
                        } else if t.credits > 0 {
                            format!("{} credits", count(t.credits))
                        } else {
                            "unpriced".into()
                        }),
                )
                .child(cell(72.).text_color(theme::text_subtle()).child(if priced {
                    pct(t.cost, total.cost)
                } else {
                    "—".into()
                }))
                .child(
                    cell(84.)
                        .text_color(theme::text_subtle())
                        .child(count(t.tokens)),
                ),
        );
    }
    list.into_any_element()
}

impl gpui::Focusable for UsageView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for UsageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        *self.shown.drawn.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        let all: &[Record] = self.records.as_deref().unwrap_or_default();
        let filtered: Vec<Record>;
        let records = match self.agent_filter {
            Some(agent) => {
                filtered = all.iter().filter(|r| r.agent() == agent).cloned().collect();
                &filtered[..]
            }
            None => all,
        };
        let s = Summary::of(records, self.range, now(), |p| self.project_of(p));
        let dates = format!("{} to {}", short_date(s.first), short_date(s.last));

        let mut ranges = ui::segmented();
        for r in [Range::Week, Range::Month, Range::Quarter] {
            ranges = ranges.child(
                ui::segment(
                    ("usage-range", r as usize),
                    None,
                    r.label(),
                    self.range == r,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.range = r;
                    cx.notify();
                })),
            );
        }
        let refresh = ui::refresh_control(
            "usage-refresh",
            self.scanning,
            None,
            cx.listener(|this, _, _, cx| this.rescan(cx)),
        );
        let header = div()
            .flex()
            .flex_none()
            .items_end()
            .justify_between()
            .gap(crate::ui::sp(16.))
            .px(crate::ui::sp(ui::PAGE_PAD))
            .pt(crate::ui::sp(ui::PAGE_PAD))
            .pb(crate::ui::sp(18.))
            .child(ui::page_title("Usage", Some(dates.into_any_element())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(ranges)
                    .child(refresh),
            );

        let body: AnyElement = match &self.records {
            None => ui::loading("Reading agent usage history…").into_any_element(),
            Some(_) => self.render_body(&s, cx),
        };
        div()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(header)
            .child(
                div()
                    .id("usage-body")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .pb(crate::ui::sp(32.))
                    .child(body),
            )
    }
}

impl UsageView {
    fn render_body(&self, s: &Summary, cx: &mut Context<Self>) -> AnyElement {
        self.feed_chart(s, cx);
        let total = &s.total;
        // The hero: the range's cost at API prices, how it moved against the
        // days before, and how it splits between the agents.
        let delta = (s.prev_cost > 0.01).then(|| (total.cost - s.prev_cost) / s.prev_cost);
        let per_day = total.cost / s.active_days.max(1) as f64;
        let mut trend = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .text_size(px(ui::TEXT_SM));
        if let Some(d) = delta {
            let up = d >= 0.;
            trend = trend.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(3.))
                    .px(crate::ui::sp(6.))
                    .h(px(20.))
                    .rounded(crate::ui::rad(ui::RADIUS_SM))
                    .bg(theme::fill_hover())
                    .text_color(if up {
                        theme::mix(0.85)
                    } else {
                        theme::diff_add().into()
                    })
                    .child(format!(
                        "{} {:.0}%",
                        if up { "↑" } else { "↓" },
                        d.abs() * 100.
                    )),
            );
            trend = trend.child(
                div()
                    .text_color(theme::text_subtle())
                    .child(format!("vs previous {} days", self.range.days())),
            );
        }
        if total.cost > 0. {
            let sep = if delta.is_some() { "· " } else { "" };
            trend = trend.child(
                div()
                    .text_color(theme::text_subtle())
                    .child(format!("{sep}{}/active day", usd(per_day))),
            );
        }
        let mut hero = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(18.))
            .w(px(300.))
            .flex_none()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(6.))
                    .child(ui::overline("Raw token cost"))
                    .child(
                        div()
                            .ty(ui::Type::Display)
                            .text_color(theme::text())
                            .child(format!("{}*", usd(total.cost))),
                    )
                    .child(trend)
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(if self.access.is_some() {
                                // A key, a gateway or a cloud provider may bill these tokens.
                                "* at full API rate; a key or endpoint may bill it"
                            } else {
                                "* if billed at full API rate; your plans don't charge it"
                            }),
                    ),
            );
        for (agent, t) in &s.agents {
            hero = hero.child(agent_share(*agent, t, total));
        }
        if s.agents.is_empty() {
            hero = hero.child(
                div()
                    .text_size(px(ui::TEXT_MD))
                    .text_color(theme::text_subtle())
                    .child("Nothing in this range yet."),
            );
        }

        // The legend doubles as a filter for the whole page.
        let mut legend = div().flex().items_center().gap(crate::ui::sp(2.));
        let shown: Vec<AgentKind> = match self.agent_filter {
            Some(a) => vec![a],
            None => s.agents.iter().map(|(a, _)| *a).collect(),
        };
        let mut known: Vec<AgentKind> = vec![AgentKind::Droid, AgentKind::Claude, AgentKind::Devin];
        known.retain(|a| shown.contains(a) || self.agent_filter.is_some());
        for agent in known {
            let on = self.agent_filter.is_none_or(|f| f == agent);
            legend = legend.child(
                div()
                    .id(("usage-legend", agent as usize))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .px(crate::ui::sp(6.))
                    .h(px(22.))
                    .rounded(crate::ui::rad(ui::RADIUS_SM))
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::fill_hover()))
                    .when(self.agent_filter == Some(agent), |d| {
                        d.bg(theme::fill_hover())
                    })
                    .text_size(px(ui::TEXT_SM))
                    .text_color(if on {
                        theme::mix(0.7)
                    } else {
                        theme::mix(0.35)
                    })
                    .child(div().size(px(8.)).rounded_full().bg(if on {
                        agent_color(agent)
                    } else {
                        theme::mix(0.2)
                    }))
                    .child(agent.label())
                    .tooltip(ui::tip("Show only this agent; click again for all"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.agent_filter = if this.agent_filter == Some(agent) {
                            None
                        } else {
                            Some(agent)
                        };
                        this.show_all = false;
                        cx.notify();
                    })),
            );
        }
        let chart = div()
            .flex_1()
            .min_w(px(360.))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .child(
                        ui::heading(if self.measure == Measure::Cost {
                            "Daily cost"
                        } else {
                            "Daily tokens"
                        })
                        .flex_1(),
                    )
                    .child(legend)
                    .child(ui::toggle_group(
                        "usage-measure",
                        &[(Measure::Cost, "Cost"), (Measure::Tokens, "Tokens")],
                        self.measure,
                        |seg, m| {
                            seg.on_click(cx.listener(move |this, _, _, cx| {
                                this.measure = m;
                                cx.notify();
                            }))
                        },
                    )),
            )
            .child(self.chart.clone());

        // The stat strip, each with its trend over the range.
        // The last two weeks: over a whole month the line is mostly spikes.
        let trend = |f: fn(&Tally) -> f64| -> Vec<f64> {
            s.days
                .iter()
                .skip(s.days.len().saturating_sub(14))
                .map(|(_, by)| by.values().map(f).sum())
                .collect()
        };
        let observed = total.input + total.cache_read + total.cache_write;
        let tokens_per_day = total.tokens / s.active_days.max(1) as u64;
        let strip = div()
            .flex()
            .flex_wrap()
            .border_t_1()
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(stat(
                "Processed tokens",
                count(total.tokens),
                format!("{}/active day", count(tokens_per_day)),
                trend(|t| t.tokens as f64),
                false,
            ))
            .child(stat(
                "Cached input",
                count(total.cache_read),
                format!("{} of input", pct(total.cache_read as f64, observed as f64)),
                trend(|t| t.cache_read as f64),
                false,
            ))
            .child(stat(
                "Uncached input",
                count(total.input + total.cache_write),
                format!("{} cache writes", count(total.cache_write)),
                trend(|t| (t.input + t.cache_write) as f64),
                false,
            ))
            .child(stat(
                "Output",
                count(total.output),
                format!(
                    "{} · {}",
                    plural(s.sessions, "session"),
                    plural(s.active_days, "day")
                ),
                trend(|t| t.output as f64),
                false,
            ))
            .child(stat(
                "Cache savings",
                usd(total.savings),
                if total.cost > 0. {
                    format!("{:.1}× the raw cost", total.savings / total.cost)
                } else {
                    "—".into()
                },
                trend(|t| t.savings),
                true,
            ));

        // The breakdown and the side column.
        let rows = self.breakdown_rows(s);
        let hidden = rows.len().saturating_sub(ROWS);
        let shown_rows: Vec<_> = if self.show_all {
            rows
        } else {
            rows.into_iter().take(ROWS).collect()
        };
        let header = match self.group {
            Group::Model => "Model",
            Group::Day => "Day",
            Group::Project => "Project",
            Group::Session => "Session",
        };
        let breakdown = div()
            .flex_1()
            .min_w(px(380.))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(ui::heading("Breakdown").flex_1())
                    .child(ui::toggle_group(
                        "usage-group",
                        &[
                            (Group::Model, "Model"),
                            (Group::Project, "Project"),
                            (Group::Session, "Session"),
                            (Group::Day, "Day"),
                        ],
                        self.group,
                        |seg, g| {
                            seg.on_click(cx.listener(move |this, _, _, cx| {
                                this.group = g;
                                this.show_all = false;
                                cx.notify();
                            }))
                        },
                    )),
            )
            .child(breakdown_table(&shown_rows, total, header))
            .when(hidden > 0, |d| {
                let label = if self.show_all {
                    "Show less".to_string()
                } else {
                    format!("Show {hidden} more")
                };
                d.child(
                    div()
                        .id("usage-show-all")
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_subtle())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::content()))
                        .child(label)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_all = !this.show_all;
                            cx.notify();
                        })),
                )
            });

        let priced = total.tokens.saturating_sub(total.unpriced);
        // With a proxy key and no sign-in, the proxy's accounts are Claude's
        // limits; without a key, Claude's block still offers the sign-in.
        let claude_shown = self.plan.is_some() || self.proxy.as_ref().is_none_or(|p| !p.has_key);
        let side = div()
            .w(px(320.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(28.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(16.))
                    .child(ui::heading("Plan limits"))
                    .when(claude_shown, |d| d.child(self.claude_limits(cx)))
                    .when_some(self.proxy_limits(cx), |d, p| {
                        d.when(claude_shown, |d| {
                            d.child(div().h(px(1.)).bg(theme::fill_hover()))
                        })
                        .child(p)
                    })
                    .when_some(self.borrowed_limits(), |d, b| {
                        d.child(div().h(px(1.)).bg(theme::fill_hover())).child(b)
                    })
                    .when_some(self.seat_limits(), |d, s| {
                        d.child(div().h(px(1.)).bg(theme::fill_hover())).child(s)
                    })
                    .child(div().h(px(1.)).bg(theme::fill_hover()))
                    .child(self.droid_limits(cx))
                    .when_some(self.opencode_limits(), |d, go| {
                        d.child(div().h(px(1.)).bg(theme::fill_hover())).child(go)
                    })
                    .when_some(self.devin_limits(), |d, devin| {
                        d.child(div().h(px(1.)).bg(theme::fill_hover()))
                            .child(devin)
                    })
                    // Limits that show up without a sign-in prompt look
                    // alarming; say where they come from.
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .line_height(px(15.))
                            .text_color(theme::text_subtle())
                            .child(crate::agent::cli_login::USAGE_NOTE),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(4.))
                    .child(ui::heading("Cost quality"))
                    .child(ui::kv(
                        "Model priced",
                        pct(priced as f64, total.tokens as f64),
                    ))
                    .child(ui::kv(
                        "Unpriced",
                        pct(total.unpriced as f64, total.tokens as f64),
                    ))
                    .when(total.credits > 0, |d| {
                        d.child(ui::kv("Droid credits", count(total.credits)))
                    })
                    .child(ui::kv("Cache savings", usd(total.savings))),
            );

        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(28.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(crate::ui::sp(40.))
                    .child(hero)
                    .child(chart),
            )
            .child(strip)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap(crate::ui::sp(40.))
                    .child(breakdown)
                    .child(side),
            )
            .into_any_element()
    }
}

/// Whether the proxy on screen is now another one (or gone): a key typed in
/// the field belonged to the old one.
fn proxy_switched(old: Option<&cliproxy::Proxy>, new: Option<&cliproxy::Proxy>) -> bool {
    old.is_some_and(|o| new.is_none_or(|n| n.base != o.base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_key_is_dropped_when_the_proxy_changes() {
        let at = |base: &str| {
            let mut p = cliproxy::Proxy::default();
            p.base = base.into();
            p
        };
        let a = at("http://127.0.0.1:8317/v0/management");
        let b = at("https://gw.example.com/v0/management");
        assert!(proxy_switched(Some(&a), Some(&b)));
        assert!(proxy_switched(Some(&a), None));
        // A first read, or a new read of the same proxy, keeps what was typed.
        assert!(!proxy_switched(None, Some(&a)));
        let mut a2 = a.clone();
        a2.at = 99;
        assert!(!proxy_switched(Some(&a), Some(&a2)));
    }

    fn record(at: u64, agent: AgentKind, cwd: &str, model: &str, input: u64) -> Record {
        Record {
            at,
            agent: Some(agent),
            session: cwd.into(),
            cwd: cwd.into(),
            model: model.into(),
            input,
            cache_read: 1000,
            ..Default::default()
        }
    }

    #[test]
    fn summary_adds_up_the_range_by_day_model_and_project() {
        let now = 1_800_000_000;
        let records = vec![
            record(
                now - 40 * DAY,
                AgentKind::Claude,
                "/old",
                "claude-opus-5-5",
                900,
            ),
            record(
                now - DAY,
                AgentKind::Claude,
                "/a",
                "claude-opus-5-5",
                1_000_000,
            ),
            record(now, AgentKind::Droid, "/a", "kimi-k3", 50),
            record(now, AgentKind::Claude, "/b", "claude-sonnet-5-5", 10),
        ];
        let name = |p: &Path| p.display().to_string();
        let s = Summary::of(&records, Range::Week, now, name);
        assert_eq!(s.days.len(), 7);
        assert_eq!(s.active_days, 2);
        assert_eq!(s.sessions, 3);
        assert_eq!(
            s.models[0].0,
            (AgentKind::Claude, "claude-opus-5-5".to_string())
        );
        assert_eq!(s.projects[0].0, "/a");
        assert_eq!(s.total.unpriced, 1050);
        assert!(s.total.cost > 4.);
    }

    #[test]
    fn numbers_read_like_the_reference() {
        assert_eq!(usd(7576.966), "$7,576.97");
        assert_eq!(usd(0.), "$0.00");
        assert_eq!(count(4_110_000_000), "4.11B");
        assert_eq!(count(181_000_000), "181M");
        assert_eq!(count(450_000), "450k");
        assert_eq!(usd_short(800.), "$800");
        assert_eq!(usd_short(5.), "$5");
        assert_eq!(usd_short(0.5), "$0.50");
        assert_eq!(usd_short(2500.), "$2.5k");
        assert_eq!(model_name("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name("claude-sonnet-4"), "Sonnet 4");
        assert_eq!(model_name("kimi-k3"), "Kimi K3");
        assert_eq!(model_name("gpt-6.1-sol"), "gpt-6.1-sol");
        assert_eq!(model_name("gpt-5.4-mini"), "gpt-5.4-mini");
    }
}
