//! The PostHog tab: the PostHog project of the open Arakun project, read
//! with whichever saved sign-in sees it (`posthog`). Every account stays
//! signed in at once; the header names the account, organization and project
//! and switches between them. The overview is 24 hours of events and
//! visitors, the week's error tracking issues and the feature flags.
//!
//! PostHog's hourly query budget: an answer is kept on disk and shown at
//! once, read again only when the tab opens and it is older than `STALE`
//! (or by the refresh button, at most every `GAP`), and after a 429 the tab
//! waits out the Retry-After with a calm note instead of retrying.

mod accounts;

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

/// An answer older than this is read again when the tab opens.
const STALE: u64 = 15 * 60;
/// The refresh button reads at most this often.
const GAP: u64 = 60;
/// The project list is read again when the tab opens after this long.
const DIRECTORY_STALE: u64 = 6 * 3600;

pub enum PostHogEvent {
    /// Open a PostHog page in a browser tab.
    Open(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    Accounts,
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
            menu: None,
            key_input,
            region: None,
            connecting: false,
            connect_note: None,
            chart: cx.new(|_| AreaChart::new(180.)),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _load: None,
            _dir: None,
            _scan: None,
            _connect: None,
            _input: sub,
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
                .background_spawn(async move { posthog::scan::keys(&scanned) })
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
        cx.notify();
    }

    fn select(&mut self, p: Project, cx: &mut Context<Self>) {
        if self.current.as_ref() == Some(&p) {
            return;
        }
        self.overview = posthog::cached(&p);
        self.error = None;
        self.current = Some(p);
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
                .background_spawn(
                    async move { posthog::load_overview(s, &project, previous.as_ref()) },
                )
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
                .background_spawn(async move { posthog::load_directory(&sessions) })
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
        let series = vec![
            Series {
                name: "Events".into(),
                color: Tone::Info.color(),
                values: o.hourly.iter().map(|h| h.0 as f64).collect(),
            },
            Series {
                name: "Visitors".into(),
                color: Tone::Accent.color(),
                values: o.hourly.iter().map(|h| h.1 as f64).collect(),
            },
        ];
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
        .child(ui::text(Type::Body, theme::text_muted(), text))
        .into_any_element()
}

/// A titled block of the overview.
fn section(title: &str, meta: Option<String>) -> gpui::Div {
    div().flex().flex_col().gap(crate::ui::sp(8.)).child(
        div()
            .flex()
            .items_baseline()
            .gap(crate::ui::sp(8.))
            .child(ui::heading(title.to_string()))
            .when_some(meta, |d, m| {
                d.child(ui::text(Type::Caption, theme::text_faint(), m))
            }),
    )
}

fn quiet(text: impl Into<SharedString>) -> gpui::Div {
    ui::text(Type::Body, theme::text_subtle(), text).py(crate::ui::sp(8.))
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
                    format!("{} · {} · ", p.email, p.org),
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
        div()
            .flex()
            .flex_none()
            .items_end()
            .justify_between()
            .gap(crate::ui::sp(16.))
            .px(crate::ui::sp(ui::PAGE_PAD))
            .pt(crate::ui::sp(ui::PAGE_PAD))
            .pb(crate::ui::sp(18.))
            .child(ui::page_title("PostHog", Some(subtitle)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .children(open)
                    .when(!self.sessions.is_empty(), |d| d.child(accounts))
                    .child(refresh),
            )
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
                rows.push(ui::menu_heading(&g).into_any_element());
                group = g;
            }
            let on = self.current.as_ref() == Some(p);
            let matched = self.candidates.contains(p)
                || self.keys.as_ref().is_some_and(|k| k.contains(&p.api_token));
            let pick = p.clone();
            rows.push(
                ui::choice_row((id, i), on)
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
            return ui::loading("ph-scan", "Looking for this project's PostHog key…")
                .into_any_element();
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
            None => return ui::loading("ph-loading", "Reading PostHog…").into_any_element(),
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

    fn render_stats(&self, o: &Overview) -> AnyElement {
        let cell = |body: gpui::Div, last: bool| {
            div()
                .flex_1()
                .min_w(px(170.))
                .px(crate::ui::sp(16.))
                .py(crate::ui::sp(14.))
                .when(!last, |d| {
                    d.border_r_1().border_color(theme::border_subtle())
                })
                .child(body)
        };
        let live_flags = o.flags.iter().filter(|f| f.active).count();
        div()
            .flex()
            .flex_wrap()
            .border_t_1()
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(cell(
                ui::stat(
                    "Events · 24 h",
                    count(o.events),
                    format!("{} pageviews", count(o.pageviews)),
                ),
                false,
            ))
            .child(cell(
                ui::stat(
                    "Unique visitors · 24 h",
                    count(o.visitors),
                    "persons with an event",
                ),
                false,
            ))
            .child(cell(
                ui::stat(
                    "Error issues · 7 days",
                    o.issues.len().to_string(),
                    format!(
                        "{} exceptions",
                        count(o.issues.iter().map(|i| i.count).sum())
                    ),
                ),
                false,
            ))
            .child(cell(
                ui::stat(
                    "Feature flags",
                    o.flags.len().to_string(),
                    format!("{live_flags} on"),
                ),
                true,
            ))
            .into_any_element()
    }

    fn render_issues(&self, o: &Overview, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let mut list = section("Error tracking", Some("last 7 days · newest first".into()));
        if o.issues.is_empty() {
            list = list.child(quiet("No exceptions this week."));
        }
        for (i, issue) in o.issues.iter().enumerate() {
            let path = format!("/error_tracking/{}", issue.id);
            let title = if issue.kind.is_empty() {
                "Exception".to_string()
            } else {
                issue.kind.clone()
            };
            list = list.child(
                div()
                    .id(("ph-issue", i))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::fill_hover()))
                    .child(icon("alert-circle", 14.).text_color(theme::error()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(2.))
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Body)
                                    .text_color(theme::text())
                                    .child(title),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Mono)
                                    .text_color(theme::text_subtle())
                                    .child(issue.message.clone()),
                            ),
                    )
                    .child(ui::text(Type::Label, theme::text(), count(issue.count)))
                    .child(
                        ui::text(
                            Type::Caption,
                            theme::text_faint(),
                            format!("{} users", count(issue.users)),
                        )
                        .w(px(70.)),
                    )
                    .child(
                        ui::text(
                            Type::Caption,
                            theme::text_subtle(),
                            if issue.last_seen > 0 {
                                crate::memory::ago(issue.last_seen, now)
                            } else {
                                String::new()
                            },
                        )
                        .w(px(80.)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open(&path, cx))),
            );
        }
        list.flex_1().min_w(px(360.)).into_any_element()
    }

    fn render_flags(&self, o: &Overview, cx: &mut Context<Self>) -> AnyElement {
        let mut list = section(
            "Feature flags",
            Some(format!("{} · live first", o.flags.len())),
        );
        if o.flags.is_empty() {
            list = list.child(quiet("No feature flags in this project."));
        }
        for (i, f) in o.flags.iter().take(30).enumerate() {
            let path = format!("/feature_flags/{}", f.id);
            list = list.child(
                div()
                    .id(("ph-flag", i))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(6.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::fill_hover()))
                    .when(!f.active, |d| d.opacity(0.6))
                    .child(
                        ui::status_dot(
                            if f.active { "On" } else { "Off" },
                            if f.active {
                                Tone::Success
                            } else {
                                Tone::Neutral
                            },
                        )
                        .w(px(44.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Mono)
                                    .text_color(theme::text())
                                    .child(f.key.clone()),
                            )
                            .when(!f.name.is_empty() && f.name != f.key, |d| {
                                d.child(
                                    div()
                                        .truncate()
                                        .ty(Type::Caption)
                                        .text_color(theme::text_faint())
                                        .child(f.name.clone()),
                                )
                            }),
                    )
                    .child(ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        f.rollout.clone(),
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| this.open(&path, cx))),
            );
        }
        if o.flags.len() > 30 {
            list = list.child(quiet(format!("{} more in PostHog.", o.flags.len() - 30)));
        }
        list.w(px(380.)).flex_none().into_any_element()
    }

    fn render_overview(&self, o: &Overview, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let mut notes: Vec<AnyElement> = Vec::new();
        if let Some(until) = o.limited_until.filter(|t| *t > now) {
            notes.push(notice(
                "clock01",
                Tone::Info,
                format!(
                    "PostHog's hourly query budget for this account is spent. These are the numbers from {}; the tab reads again after {}.",
                    posthog::clock(o.fetched_at),
                    posthog::clock(until)
                ),
            ));
        }
        match &self.error {
            Some(ApiError::RateLimited(until)) if *until > now => notes.push(notice(
                "clock01",
                Tone::Info,
                format!("PostHog asked to wait for its query budget. Showing the saved numbers; the tab reads again after {}.", posthog::clock(*until)),
            )),
            Some(ApiError::RateLimited(_)) | None => {}
            Some(e) => notes.push(notice("alert-circle", Tone::Warning, format!("Showing the last answer. {}", e.message()))),
        }
        for p in &o.problems {
            notes.push(notice("alert-circle", Tone::Warning, p.clone()));
        }
        let traffic = section("Traffic", Some("last 24 hours, per hour".into()))
            .child(
                div()
                    .flex()
                    .gap(crate::ui::sp(16.))
                    .child(ui::legend("Events", Tone::Info.color()))
                    .child(ui::legend("Visitors", Tone::Accent.color())),
            )
            .child(if o.hourly.len() == posthog::HOURS {
                self.chart.clone().into_any_element()
            } else {
                quiet("No hourly numbers yet.").into_any_element()
            });
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(28.))
            .children(notes)
            .child(self.render_stats(o))
            .child(traffic)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap(crate::ui::sp(40.))
                    .child(self.render_issues(o, cx))
                    .child(self.render_flags(o, cx)),
            )
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.sessions.is_empty() {
            return div()
                .flex()
                .justify_center()
                .pt(crate::ui::sp(24.))
                .child(self.render_connect(true, cx))
                .into_any_element();
        }
        if self.page == Page::Accounts {
            return self.render_accounts(cx);
        }
        match (&self.current, &self.overview) {
            (None, _) => self.render_picker(cx),
            (Some(_), Some(o)) => self.render_overview(o, cx),
            (Some(_), None) => div()
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
            .child(
                div()
                    .id("ph-body")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .pb(crate::ui::sp(32.))
                    .child(div().w_full().max_w(px(1320.)).mx_auto().child(body)),
            )
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
    fn counts_read_short() {
        assert_eq!(count(950), "950");
        assert_eq!(count(12_345), "12.3k");
        assert_eq!(count(2_000), "2k");
        assert_eq!(count(4_100_000), "4.1M");
    }
}
