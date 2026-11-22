//! The PostHog tab: the PostHog project of the open Arakun project, read
//! with whichever saved sign-in sees it (`posthog`). Every account stays
//! signed in at once; the header names the account, organization and project
//! and switches between them.
//!
//! A strip of tabs under the header, each in its own module: Overview (24
//! hours of events and visitors, flags, experiments, errors), Events and
//! People (their own views, reading pages through `posthog::query`), Flags,
//! Experiments and Errors (lists whose rows open in `detail`), and Ask
//! (PostHog AI).
//!
//! PostHog's hourly query budget: an answer is kept on disk and shown at
//! once, read again only when the tab opens and it is older than `STALE`
//! (or by the refresh button, at most every `GAP`), and after a 429 the tab
//! waits out the Retry-After with a calm note instead of retrying.

mod accounts;
mod ask;
mod bars;
mod detail;
mod errors;
mod events;
mod experiments;
mod flags;
mod overview;
mod people;
mod scope;

use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    Window, div, px,
};

use crate::input::{InputEvent, TextInput};
use crate::posthog::{self, ApiError, Directory, Overview, Project, Region, Resolved, Session};
use crate::theme;
use crate::ui::{self, AreaChart, ButtonKind, Format, Series, Tone, Type, Typography as _, icon};
use detail::Detail;
use events::{EventsEvent, EventsTab};
use people::{PeopleEvent, PeopleTab};

/// An answer older than this is read again when the tab opens.
const STALE: u64 = 15 * 60;
/// The refresh button reads at most this often.
const GAP: u64 = 60;
/// The project list is read again when the tab opens after this long.
const DIRECTORY_STALE: u64 = 6 * 3600;

pub enum PostHogEvent {
    /// Open a PostHog page in a browser tab.
    Open(String),
    /// Put text in the new-session composer, without sending it.
    Draft(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    Accounts,
}

/// The tabs under the header, in their order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Events,
    People,
    Flags,
    Experiments,
    Errors,
    Ask,
}

impl Tab {
    const ALL: [Tab; 7] = [
        Tab::Overview,
        Tab::Events,
        Tab::People,
        Tab::Flags,
        Tab::Experiments,
        Tab::Errors,
        Tab::Ask,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Events => "Events",
            Tab::People => "People",
            Tab::Flags => "Flags",
            Tab::Experiments => "Experiments",
            Tab::Errors => "Errors",
            Tab::Ask => "Ask",
        }
    }
}

/// The account and project a tab reads.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub session: Session,
    pub project: Project,
}

pub struct PostHogView {
    /// The Arakun project the tab follows.
    root: Option<PathBuf>,
    /// With their credentials: never rendered, only handed to `posthog`.
    sessions: Vec<Session>,
    directory: Option<Directory>,
    dir_loading: bool,
    /// The `phc_` keys in the project's files; None while scanning.
    keys: Option<Vec<String>>,
    current: Option<Project>,
    /// The current project was found by its key, not picked.
    auto: bool,
    /// Projects whose keys are all in the files.
    candidates: Vec<Project>,
    overview: Option<Overview>,
    loading: bool,
    error: Option<ApiError>,
    /// Unix seconds of the last read asked for.
    asked_at: u64,
    page: Page,
    tab: Tab,
    detail: Option<Detail>,
    inspection: Option<Result<posthog::inspection::Inspection, ApiError>>,
    _inspect: Option<Task<()>>,
    ask: Entity<ask::Ask>,
    events: Entity<EventsTab>,
    people: Entity<PeopleTab>,
    menu: Option<Point<Pixels>>,
    key_input: Entity<TextInput>,
    /// None tries both regions.
    region: Option<Region>,
    connecting: bool,
    connect_note: Option<(String, bool)>,
    chart: Entity<AreaChart>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _load: Option<Task<()>>,
    _dir: Option<Task<()>>,
    _scan: Option<Task<()>>,
    _connect: Option<Task<()>>,
    _input: Subscription,
    _tabs: [Subscription; 2],
}

impl EventEmitter<PostHogEvent> for PostHogView {}

impl PostHogView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let key_input = cx.new(|cx| TextInput::new("phx_…", cx).mono().secret());
        let sub = cx.subscribe(
            &key_input,
            |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                InputEvent::Submit => this.connect_key(cx),
                InputEvent::Changed => {
                    this.connect_note = None;
                    cx.notify();
                }
                _ => {}
            },
        );
        let events = cx.new(EventsTab::new);
        let people = cx.new(PeopleTab::new);
        let tabs = [
            cx.subscribe(&events, |this: &mut Self, _, e: &EventsEvent, cx| match e {
                EventsEvent::Open(url) => cx.emit(PostHogEvent::Open(url.clone())),
                EventsEvent::Person {
                    person_id,
                    distinct_id,
                } => {
                    this.tab = Tab::People;
                    let (p, d) = (person_id.clone(), distinct_id.clone());
                    this.people.update(cx, |t, cx| t.open_person(p, d, cx));
                    cx.notify();
                }
            }),
            cx.subscribe(&people, |_, _, e: &PeopleEvent, cx| match e {
                PeopleEvent::Open(url) => cx.emit(PostHogEvent::Open(url.clone())),
            }),
        ];
        Self {
            root: None,
            sessions: posthog::sessions(),
            directory: posthog::cached_directory(),
            dir_loading: false,
            keys: None,
            current: None,
            auto: false,
            candidates: Vec::new(),
            overview: None,
            loading: false,
            error: None,
            asked_at: 0,
            page: Page::Main,
            tab: Tab::Overview,
            detail: None,
            inspection: None,
            _inspect: None,
            ask: cx.new(ask::Ask::new),
            events,
            people,
            menu: None,
            key_input,
            region: None,
            connecting: false,
            connect_note: None,
            chart: cx.new(|_| AreaChart::new(96.)),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _load: None,
            _dir: None,
            _scan: None,
            _connect: None,
            _input: sub,
            _tabs: tabs,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut gpui::App) {
        window.focus(&self.focus, cx);
    }

    /// Follows the visible Arakun project; a new one is scanned for its keys.
    pub fn set_project(&mut self, root: &Path, cx: &mut Context<Self>) {
        if self.root.as_deref() == Some(root) {
            return;
        }
        self.close_detail();
        self.root = Some(root.to_path_buf());
        self.keys = None;
        self.current = None;
        self.candidates.clear();
        self.overview = None;
        self.error = None;
        self.menu = None;
        let root = root.to_path_buf();
        self._scan = Some(cx.spawn(async move |this, cx| {
            let scanned = root.clone();
            let keys = cx
                .background_spawn(crate::guard::task(
                    async move { posthog::scan::keys(&scanned) },
                ))
                .await;
            this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    this.keys = Some(keys);
                    this.resolve(cx);
                }
            })
            .ok();
        }));
        self.resolve(cx);
        cx.notify();
    }

    /// Opening the tab: the project list and the overview, if old.
    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        let now = crate::model::unix_now();
        if !self.sessions.is_empty()
            && self
                .directory
                .as_ref()
                .is_none_or(|d| now.saturating_sub(d.fetched_at) >= DIRECTORY_STALE)
        {
            self.refresh_directory(cx);
        }
        if self.current.is_some()
            && self
                .overview
                .as_ref()
                .is_none_or(|o| now.saturating_sub(o.fetched_at) >= STALE)
        {
            self.refresh(false, cx);
        }
    }

    /// Picks the project for `root`: its remembered link, else its keys.
    fn resolve(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.sessions.is_empty() {
            return;
        }
        let Some(dir) = self.directory.clone() else {
            self.refresh_directory(cx);
            return;
        };
        let link = posthog::link_of(&root);
        // Without a link, wait for the keys before guessing.
        if link.is_none() && self.keys.is_none() {
            return;
        }
        let keys = self.keys.clone().unwrap_or_default();
        match posthog::resolve(link.as_ref(), &keys, &dir.projects) {
            Resolved::One(p, auto) => {
                if link
                    .as_ref()
                    .is_none_or(|l| l.session != p.session || l.project != p.id)
                {
                    posthog::set_link(
                        &root,
                        Some(posthog::Link {
                            session: p.session.clone(),
                            project: p.id,
                            auto,
                        }),
                    );
                }
                self.candidates.clear();
                self.auto = auto;
                let p = p.clone();
                self.select(p, cx);
            }
            Resolved::Several(v) => {
                self.candidates = v.into_iter().cloned().collect();
                self.current = None;
            }
            Resolved::Unknown => {
                self.candidates.clear();
                self.current = None;
            }
        }
        self.sync_tabs(cx);
        cx.notify();
    }

    /// Hands the current account and project to the tabs that read on their own.
    fn sync_tabs(&mut self, cx: &mut Context<Self>) {
        let target = self.current.clone().and_then(|p| {
            Some(Target {
                session: self.session_of(&p)?,
                project: p,
            })
        });
        self.ask
            .update(cx, |a, cx| a.set_target(target.clone(), cx));
        let t = target.clone();
        self.events.update(cx, |e, cx| e.set_target(t, cx));
        self.people.update(cx, |e, cx| e.set_target(target, cx));
        self.show_current(cx);
    }

    /// The visible tab reads what it doesn't have yet.
    fn show_current(&mut self, cx: &mut Context<Self>) {
        match self.tab {
            Tab::Events => self.events.update(cx, |t, cx| t.show(cx)),
            Tab::People => self.people.update(cx, |t, cx| t.show(cx)),
            _ => {}
        }
    }

    pub(super) fn show_tab(&mut self, tab: Tab, _: &mut Window, cx: &mut Context<Self>) {
        self.close_detail();
        self.tab = tab;
        self.menu = None;
        self.show_current(cx);
        cx.notify();
    }

    fn select(&mut self, p: Project, cx: &mut Context<Self>) {
        if self.current.as_ref() == Some(&p) {
            return;
        }
        self.close_detail();
        self._load = None;
        self.loading = false;
        self.asked_at = 0;
        self.overview = posthog::cached(&p);
        self.error = None;
        self.current = Some(p);
        self.sync_tabs(cx);
        self.update_chart(cx);
        self.refresh_if_stale(cx);
        cx.notify();
    }

    /// The user's pick for this Arakun project, remembered.
    fn pick(&mut self, p: Project, cx: &mut Context<Self>) {
        self.menu = None;
        self.page = Page::Main;
        if let Some(root) = &self.root {
            posthog::set_link(
                root,
                Some(posthog::Link {
                    session: p.session.clone(),
                    project: p.id,
                    auto: false,
                }),
            );
        }
        self.auto = false;
        self.candidates.clear();
        self.select(p, cx);
    }

    fn session_of(&self, p: &Project) -> Option<Session> {
        self.sessions.iter().find(|s| s.id == p.session).cloned()
    }

    fn refresh(&mut self, manual: bool, cx: &mut Context<Self>) {
        let now = crate::model::unix_now();
        if self.loading || (manual && now.saturating_sub(self.asked_at) < GAP) {
            return;
        }
        let Some(p) = self.current.clone() else {
            return;
        };
        let Some(s) = self.session_of(&p) else {
            self.current = None;
            self.resolve(cx);
            return;
        };
        self.asked_at = now;
        self.loading = true;
        cx.notify();
        let previous = self.overview.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let project = p.clone();
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    posthog::load_overview(s, &project, previous.as_ref())
                }))
                .await;
            this.update(cx, |this, cx| {
                this.loading = false;
                if this.current.as_ref() != Some(&p) {
                    return;
                }
                match got {
                    Ok(o) => {
                        this.overview = Some(o);
                        this.error = None;
                    }
                    Err(e) => this.error = Some(e),
                }
                this.update_chart(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn refresh_directory(&mut self, cx: &mut Context<Self>) {
        if self.dir_loading || self.sessions.is_empty() {
            return;
        }
        self.dir_loading = true;
        cx.notify();
        let sessions = self.sessions.clone();
        self._dir = Some(cx.spawn(async move |this, cx| {
            let dir = cx
                .background_spawn(crate::guard::task_or_default(async move {
                    posthog::load_directory(&sessions)
                }))
                .await;
            this.update(cx, |this, cx| {
                this.dir_loading = false;
                this.directory = Some(dir);
                this.sessions = posthog::sessions();
                // A project that is gone, or a better match, shows up now.
                if this.current.as_ref().is_none_or(|c| {
                    !this
                        .directory
                        .as_ref()
                        .is_some_and(|d| d.projects.contains(c))
                }) {
                    this.current = None;
                }
                this.resolve(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn update_chart(&mut self, cx: &mut Context<Self>) {
        let Some(o) = &self.overview else { return };
        if o.hourly.len() != posthog::HOURS {
            return;
        }
        let n = o.hourly.len();
        let labels: Vec<SharedString> = (0..n)
            .map(|i| match n - 1 - i {
                0 => "This hour".into(),
                1 => "An hour ago".into(),
                h => format!("{h} hours ago").into(),
            })
            .collect();
        let ticks = vec!["24 h ago".into(), "12 h ago".into(), "Now".into()];
        let series = vec![Series {
            name: "Visitors".into(),
            color: gpui::rgb(theme::CATEGORY[5]).into(),
            values: o.hourly.iter().map(|h| h.1 as f64).collect(),
        }];
        let fmt: Format = std::rc::Rc::new(|v| count(v as u64));
        self.chart.update(cx, |c, cx| {
            c.set(series, labels, ticks, vec![None; n], fmt.clone(), fmt, cx)
        });
    }

    fn open(&self, path: &str, cx: &mut Context<Self>) {
        if let Some(p) = &self.current {
            cx.emit(PostHogEvent::Open(p.url(path)));
        }
    }
}

/// "950", "12.3k", "4.1M".
fn count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        _ => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
    }
}

/// A tinted line above the content: a calm note or a warning.
fn notice(icon_name: &str, tone: Tone, text: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .px(crate::ui::sp(12.))
        .py(crate::ui::sp(8.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .bg(tone.color().opacity(0.1))
        .child(icon(icon_name, 14.).text_color(tone.color()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(ui::text(Type::Body, theme::text_muted(), text)),
        )
        .into_any_element()
}

fn quiet(text: impl Into<SharedString>) -> gpui::Div {
    ui::text(Type::Body, theme::text_subtle(), text).py(crate::ui::sp(8.))
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// "Nov 18" in local time.
fn day(t: u64) -> String {
    match crate::platform::local_time(t) {
        Some(l) => format!("{} {}", MONTHS[l.month.clamp(0, 11) as usize], l.day),
        None => {
            let days = (t / 86_400) as i64;
            // Civil from days (Hinnant), for the UTC fallback.
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            format!("{} {d}", MONTHS[(m - 1) as usize])
        }
    }
}

/// "Today" or "Nov 18".
fn day_or_today(t: u64, now: u64) -> String {
    if day(t) == day(now) {
        "Today".into()
    } else {
        day(t)
    }
}

/// "14:02:31" today, "Nov 17 14:02" before.
fn time_of_day(t: u64, now: u64) -> String {
    match crate::platform::local_time(t) {
        Some(l) if day(t) == day(now) => format!("{:02}:{:02}:{:02}", l.hour, l.minute, t % 60),
        Some(l) => format!("{} {:02}:{:02}", day(t), l.hour, l.minute),
        None => posthog::clock(t),
    }
}

/// "now", "4 min ago", "3 h ago", "2 d ago".
fn short_ago(t: u64, now: u64) -> String {
    match now.saturating_sub(t) {
        0..60 => "now".into(),
        s @ 60..3600 => format!("{} min ago", s / 60),
        s @ 3600..86_400 => format!("{} h ago", s / 3600),
        86_400..172_800 => "yesterday".into(),
        s => format!("{} d ago", s / 86_400),
    }
}

impl PostHogView {
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let subtitle: AnyElement = match &self.current {
            Some(p) => div()
                .id("ph-switch")
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .px(crate::ui::sp(6.))
                .ml(px(-6.))
                .h(px(ui::BUTTON_H))
                .rounded(crate::ui::rad(ui::RADIUS))
                .cursor_pointer()
                .hover(|d| d.bg(theme::fill_hover()))
                .child(ui::text(
                    Type::Body,
                    theme::text_muted(),
                    format!(
                        "{} · {} · ",
                        crate::redact::value(&p.email),
                        crate::redact::value(&p.org)
                    ),
                ))
                .child(ui::text(Type::Body, theme::text(), p.name.clone()))
                .when(self.auto, |d| {
                    d.child(ui::badge("key found", Tone::Neutral))
                })
                .child(icon("unfold-more", 14.).text_color(theme::text_subtle()))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    this.menu = if this.menu.is_some() {
                        None
                    } else {
                        Some(ev.position() + gpui::point(px(-8.), px(14.)))
                    };
                    cx.notify();
                }))
                .into_any_element(),
            None => ui::text(
                Type::Body,
                theme::text_subtle(),
                "Product analytics, errors and flags of this project",
            )
            .into_any_element(),
        };
        let updated = self
            .overview
            .as_ref()
            .filter(|_| self.current.is_some())
            .map(|o| crate::memory::ago(o.fetched_at, now));
        let can_refresh = self.current.is_some() && now.saturating_sub(self.asked_at) >= GAP;
        let refresh = ui::refresh_control(
            "ph-refresh",
            self.loading || self.dir_loading,
            updated,
            cx.listener(move |this, _, _, cx| {
                if can_refresh {
                    this.refresh(true, cx);
                    match this.tab {
                        Tab::Events => this.events.update(cx, |t, cx| t.refresh(cx)),
                        Tab::People => this.people.update(cx, |t, cx| t.refresh(cx)),
                        _ => {}
                    }
                } else if this.current.is_none() {
                    this.refresh_directory(cx);
                }
            }),
        );
        let accounts = ui::button_with(
            "ph-accounts",
            Some("user"),
            if self.page == Page::Accounts {
                "Back to the project"
            } else {
                "Accounts"
            },
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.page = if this.page == Page::Accounts {
                Page::Main
            } else {
                Page::Accounts
            };
            this.menu = None;
            cx.notify();
        }));
        let open = self.current.as_ref().map(|_| {
            ui::button_with(
                "ph-open",
                Some("link-square02"),
                "Open in PostHog",
                ButtonKind::Outline,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| this.open("", cx)))
        });
        let tabs =
            (self.current.is_some() && self.page == Page::Main).then(|| self.render_tabs(cx));
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(crate::ui::sp(16.))
            .px(crate::ui::sp(ui::PAGE_PAD))
            .pt(crate::ui::sp(ui::PAGE_PAD))
            .pb(crate::ui::sp(18.))
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .gap(crate::ui::sp(16.))
                    .child(ui::page_title("PostHog", Some(subtitle)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .children(open)
                            .when(!self.sessions.is_empty(), |d| d.child(accounts))
                            .child(refresh),
                    ),
            )
            .children(tabs)
            .into_any_element()
    }

    /// Overview · Events · People · Flags 14 · Experiments · Errors 9 · Ask,
    /// and the project's host at the right.
    fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let o = self.overview.as_ref();
        let mut strip = ui::segmented();
        for (i, tab) in Tab::ALL.into_iter().enumerate() {
            let n = match tab {
                Tab::Experiments => o
                    .filter(|o| o.experiment_error.is_none())
                    .map(|o| o.experiments.len()),
                Tab::Flags => o.map(|o| o.flags.len()),
                Tab::Errors => o.map(|o| o.issues.len()),
                _ => None,
            };
            let on = self.tab == tab;
            strip = strip.child(
                ui::segment(("ph-tab", i), None, tab.label(), on)
                    .when_some(n.filter(|n| *n > 0), |d, n| {
                        d.child(
                            div()
                                .ty(Type::Caption)
                                .font_family(theme::FONT_MONO)
                                .text_color(theme::text_faint())
                                .child(n.to_string()),
                        )
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.show_tab(tab, window, cx)),
                    ),
            );
        }
        let host = self
            .current
            .as_ref()
            .map(|p| format!("{} · {}", p.name, p.host.trim_start_matches("https://")))
            .unwrap_or_default();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(crate::ui::sp(16.))
            .child(strip)
            .child(ui::text(Type::Caption, theme::text_faint(), host).truncate())
            .into_any_element()
    }

    /// Projects to pick from, grouped by account and organization.
    fn project_rows(
        &self,
        id: &'static str,
        menu: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(dir) = &self.directory else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        let mut group = String::new();
        for (i, p) in dir.projects.iter().enumerate() {
            let g = format!("{} · {}", p.email, p.org);
            if g != group {
                let shown = format!(
                    "{} · {}",
                    crate::redact::value(&p.email),
                    crate::redact::value(&p.org)
                );
                rows.push(ui::menu_heading(&shown).into_any_element());
                group = g;
            }
            let on = self.current.as_ref() == Some(p);
            let matched = self.candidates.contains(p)
                || self.keys.as_ref().is_some_and(|k| k.contains(&p.api_token));
            let pick = p.clone();
            rows.push(
                ui::choice_row((id, i), p.name.clone(), on)
                    .child(div().flex_1().min_w_0().truncate().child(p.name.clone()))
                    .when(matched, |d| {
                        d.child(ui::badge("key in project", Tone::Accent))
                    })
                    .when(!menu, |d| {
                        d.child(ui::text(
                            Type::Caption,
                            theme::text_faint(),
                            p.host.trim_start_matches("https://").to_string(),
                        ))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(pick.clone(), cx)))
                    .into_any_element(),
            );
        }
        rows
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("ph-menu-scroll")
            .max_h(px(420.))
            .overflow_y_scroll()
            .child(
                ui::menu_panel("ph-menu", 340.)
                    .children(self.project_rows("ph-menu-row", true, cx))
                    .child(ui::menu_divider())
                    .child(
                        ui::menu_item(
                            "ph-menu-reload",
                            "refresh01",
                            "Read the project list again",
                            false,
                            !self.dir_loading,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            this.refresh_directory(cx);
                        })),
                    )
                    .child(
                        ui::menu_item("ph-menu-accounts", "user", "Accounts…", false, true)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                this.page = Page::Accounts;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// No project picked yet: scanning, or asking which one this is.
    fn render_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let waiting = self.keys.is_none() || (self.directory.is_none() && self.dir_loading);
        if waiting {
            return ui::loading("Looking for this project's PostHog key…").into_any_element();
        }
        let name = self
            .root
            .as_ref()
            .and_then(|r| r.file_name())
            .map_or("this project".into(), |n| n.to_string_lossy().into_owned());
        let keys = self.keys.as_ref().map_or(0, Vec::len);
        let why = match (keys, self.candidates.len()) {
            (0, _) => format!(
                "No PostHog project key (phc_…) turned up in {name}'s files. Pick its project once; Arakun remembers it."
            ),
            (_, 0) => format!(
                "{name} has {} PostHog key{} that none of your accounts can see. Add the account that owns it, or pick a project.",
                keys,
                if keys == 1 { "" } else { "s" }
            ),
            (_, n) => format!(
                "{name} has the keys of {n} projects. Pick the one this tab shows; Arakun remembers it."
            ),
        };
        let rows = self.project_rows("ph-pick-row", false, cx);
        let empty = rows.is_empty();
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .max_w(px(640.))
            .child(ui::heading(format!("Which PostHog project is {name}?")))
            .child(ui::text(Type::Body, theme::text_muted(), why))
            .when(keys == 0, |d| d.child(self.setup_draft(cx)))
            .when(empty, |d| {
                d.child(quiet(if self.dir_loading {
                    "Reading your projects…"
                } else {
                    "Your accounts don't see any project yet."
                }))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(1.))
                    .p(crate::ui::sp(4.))
                    .rounded(crate::ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .when(empty, |d| d.hidden())
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = match &self.error {
            None => return ui::loading("Reading PostHog…").into_any_element(),
            Some(ApiError::SignedOut(why)) => ui::empty_state(
                "square-lock02",
                "PostHog refused this sign-in",
                &format!("{why} Add the account again with a key that has the read scopes."),
                Some(
                    ui::button("ph-reauth", "Accounts", ButtonKind::Primary, true)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::Accounts;
                            cx.notify();
                        }))
                        .into_any_element(),
                ),
            ),
            Some(ApiError::MissingScope(scope)) => {
                let (email, host) = self
                    .current
                    .as_ref()
                    .map(|p| (p.email.clone(), p.host.clone()))
                    .unwrap_or_default();
                let url = posthog::scope::keys_url(&host);
                return scope::missing_scope(
                    "Overview",
                    scope,
                    &email,
                    &host,
                    &[],
                    "Events and People",
                    cx.listener(move |_, _, _, cx| cx.emit(PostHogEvent::Open(url.clone()))),
                    cx.listener(|this, _, _, cx| {
                        this.asked_at = 0;
                        this.refresh(true, cx);
                    }),
                );
            }
            Some(ApiError::RateLimited(until)) => ui::empty_state(
                "clock01",
                "Waiting on PostHog's query budget",
                &format!(
                    "This account used its queries for the hour. The tab reads again after {}.",
                    posthog::clock(*until)
                ),
                None,
            ),
            Some(ApiError::Failed(e)) => ui::empty_state(
                "alert-circle",
                "Couldn't read PostHog",
                e,
                Some(
                    ui::button("ph-retry", "Try again", ButtonKind::Outline, true)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.asked_at = 0;
                            this.refresh(true, cx);
                        }))
                        .into_any_element(),
                ),
            ),
        };
        state.into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.sessions.is_empty() {
            return div()
                .flex()
                .justify_center()
                .pt(crate::ui::sp(24.))
                .flex_col()
                .items_center()
                .gap(ui::sp(12.))
                .child(self.render_connect(true, cx))
                .child(self.setup_draft(cx))
                .into_any_element();
        }
        if self.page == Page::Accounts {
            return self.render_accounts(cx);
        }
        if self.current.is_some()
            && let (Some(o), Some(d)) = (&self.overview, &self.detail)
        {
            return self.render_detail(d, o, cx);
        }
        match (&self.current, &self.overview, self.tab) {
            (None, _, _) => self.render_picker(cx),
            (Some(_), _, Tab::Events) => self.events.clone().into_any_element(),
            (Some(_), _, Tab::People) => self.people.clone().into_any_element(),
            (Some(_), Some(o), Tab::Experiments) => self.render_experiments(o, None, cx),
            (Some(_), _, Tab::Ask) => self.ask.clone().into_any_element(),
            (Some(_), Some(o), Tab::Overview) => self.render_overview(o, cx),
            (Some(_), Some(o), Tab::Flags) => self.render_flags(o, None, cx),
            (Some(_), Some(o), Tab::Errors) => self.render_issues(o, None, cx),
            (Some(_), None, _) => div()
                .flex()
                .justify_center()
                .pt(crate::ui::sp(48.))
                .child(self.render_state(cx))
                .into_any_element(),
        }
    }
}

impl gpui::Focusable for PostHogView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PostHogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let body = self.render_body(cx);
        // Events and People scroll on their own, so their drawers stay put.
        let own_scroll = !self.sessions.is_empty()
            && self.page == Page::Main
            && self.current.is_some()
            && matches!(self.tab, Tab::Events | Tab::People);
        let menu = self.menu.filter(|_| self.current.is_some());
        div()
            .track_focus(&self.focus)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::text())
            .child(header)
            .child(if own_scroll {
                div().flex_1().min_h_0().child(body).into_any_element()
            } else {
                div()
                    .id("ph-body")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .pb(crate::ui::sp(32.))
                    .child(div().w_full().max_w(px(1320.)).child(body))
                    .into_any_element()
            })
            .when_some(menu, |d, at| {
                d.child(
                    div()
                        .id("ph-menu-backdrop")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                cx.notify();
                            }),
                        ),
                )
                .child(gpui::deferred(
                    gpui::anchored()
                        .position(at)
                        .snap_to_window_with_margin(px(8.))
                        .child(self.render_menu(cx)),
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_short() {
        let now = 1_800_000_000;
        assert_eq!(short_ago(now - 10, now), "now");
        assert_eq!(short_ago(now - 4 * 60, now), "4 min ago");
        assert_eq!(short_ago(now - 3 * 3600, now), "3 h ago");
        assert_eq!(short_ago(now - 30 * 3600, now), "yesterday");
        assert_eq!(short_ago(now - 5 * 86_400, now), "5 d ago");
        assert_eq!(day_or_today(now, now), "Today");
    }

    #[test]
    fn counts_read_short() {
        assert_eq!(count(950), "950");
        assert_eq!(count(12_345), "12.3k");
        assert_eq!(count(2_000), "2k");
        assert_eq!(count(4_100_000), "4.1M");
    }
}
