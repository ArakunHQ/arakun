//! The rail's segments and pieces (`crate::sidebar`): drawing each one as the
//! user arranged it, in full or narrowed to icons.

use std::sync::OnceLock;

use gpui::Point;

use super::*;
use crate::sidebar::{BadgeMode, Collapse, Item, Mode, Piece, Pin, Section, TitleAction, View};

/// A count on a rail row. Most counts are plain gray numbers; a failure or
/// something waiting on the user keeps its color, with a dot.
#[derive(Clone, Copy)]
pub(super) struct Badge {
    count: usize,
    /// The color of a count that matters; `None` draws it gray.
    color: Option<Hsla>,
    /// A failure, or something waiting on the user.
    urgent: bool,
}

impl Badge {
    pub(super) fn count(self) -> usize {
        self.count
    }

    fn color(self) -> Hsla {
        self.color.unwrap_or_else(theme::text_faint)
    }
}

/// Width of the rail narrowed to icons.
pub(super) const RAIL_ICONS_W: f32 = 52.;

fn badge_pill(b: Badge, mode: BadgeMode) -> Option<gpui::Div> {
    let dot = || div().flex_none().size(px(6.)).rounded_full().bg(b.color());
    match mode {
        BadgeMode::Off => None,
        BadgeMode::Urgent if !b.urgent => None,
        BadgeMode::Dot => Some(dot()),
        _ => Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(5.))
                .when(b.color.is_some(), |d| d.child(dot()))
                .font_family(theme::FONT_MONO)
                .text_size(px(ui::TEXT_XS))
                .text_color(b.color())
                .child(b.count.to_string()),
        ),
    }
}

/// The number on the Inbox button. Two characters at most fit its corner.
fn inbox_count_label(n: usize) -> String {
    if n > 9 { "9+".into() } else { n.to_string() }
}

pub(super) fn view_id(view: View) -> &'static str {
    match view {
        View::Inbox => "nav-inbox",
        #[cfg(feature = "coderlabs")]
        View::Tasks => "nav-tasks",
        View::Runs => "nav-runs",
        View::Branches => "nav-branches",
        View::GitHub => "nav-github",
        View::Notes => "nav-notes",
        View::Memory => "nav-memory",
        View::Usage => "nav-usage",
        View::Extensions => "nav-extensions",
        View::Cloudflare => "nav-cloudflare",
        View::PostHog => "nav-posthog",
        #[cfg(feature = "coderlabs")]
        View::Site => "nav-site",
        View::Isometric => "nav-isometric",
        #[cfg(feature = "coderlabs")]
        View::Canvas => "nav-canvas",
        View::Phone => "nav-phone",
        View::Servers => "nav-servers",
        View::Automations => "nav-automations",
        #[cfg(feature = "team")]
        View::Team => "nav-team",
    }
}

/// The shortcut a piece answers to, as written for Windows and Linux.
fn piece_shortcut(piece: &Piece) -> Option<&'static str> {
    match piece {
        Piece::Search => Some("Ctrl+K"),
        Piece::Settings => Some("Ctrl+,"),
        _ => None,
    }
}

/// A short title for a pinned link: its host, without `www.`.
pub(super) fn url_title(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.trim_start_matches("www.").to_string()
}

/// Who commits here, from git's global config: the User piece's name and email.
pub(super) fn git_identity() -> &'static (String, String) {
    static ID: OnceLock<(String, String)> = OnceLock::new();
    ID.get_or_init(|| {
        let get = |key: &str| {
            crate::platform::command("git")
                .args(["config", "--global", key])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        };
        (get("user.name"), get("user.email"))
    })
}

/// A round avatar with the first letter of the name.
pub(super) fn avatar(name: &str, size: f32) -> gpui::Div {
    let letter = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full()
        .bg(theme::fill_selected())
        .text_size(px(size * 0.45))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .child(letter)
}

impl Shell {
    /// Narrowed to icons: the rail's collapse is Icons and it is closed.
    pub(super) fn rail_narrow(&self) -> bool {
        self.sidebar.style.collapse == Collapse::Icons && !self.rail_open && self.has_projects()
    }

    pub(super) fn view_badge(&self, view: View, cx: &mut Context<Self>) -> Option<Badge> {
        let quiet = |count| Badge {
            count,
            color: None,
            urgent: false,
        };
        let loud = |count, color: Hsla| Badge {
            count,
            color: Some(color),
            urgent: true,
        };
        match view {
            View::Inbox => {
                let items = self.inbox_items();
                let count = |s| items.iter().filter(|i| i.status == s).count();
                let (waiting, failed) = (count(Status::NeedsInput), count(Status::Error));
                // A session waiting on the user outranks notices read later.
                // A failed one doesn't wait on the user, so it isn't amber.
                let n = waiting + failed + self.unseen_notices();
                if waiting > 0 {
                    Some(loud(n, theme::needs_you()))
                } else if failed > 0 {
                    Some(loud(n, theme::error().into()))
                } else {
                    (n > 0).then(|| quiet(n))
                }
            }
            #[cfg(feature = "coderlabs")]
            View::Tasks => self.tasks_badge(cx).map(|(n, overdue)| {
                if overdue {
                    loud(n, theme::error().into())
                } else {
                    quiet(n)
                }
            }),
            View::Runs => self.runs_badge(cx).map(|(n, failing)| {
                if failing {
                    loud(n, theme::error().into())
                } else {
                    quiet(n)
                }
            }),
            View::Branches => Some(self.branches_badge(cx))
                .filter(|n| *n > 0)
                .map(|n| loud(n, theme::error().into())),
            View::GitHub => self.github_badge(cx).map(|(n, wanted)| {
                if wanted {
                    loud(n, theme::needs_you())
                } else {
                    quiet(n)
                }
            }),
            View::Extensions => Some(self.extensions_view.read(cx).problems())
                .filter(|n| *n > 0)
                .map(|n| loud(n, theme::attention().into())),
            #[cfg(feature = "team")]
            View::Team => Some(self.team_view.read(cx).unread())
                .filter(|n| *n > 0)
                .map(|n| loud(n, theme::accent())),
            _ => None,
        }
    }

    pub(super) fn open_view(
        &mut self,
        view: View,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match view {
            View::Inbox => {
                self.menu = Some(ContextMenu {
                    target: MenuTarget::Inbox,
                    at: at + point(px(8.), px(0.)),
                });
                cx.notify();
            }
            #[cfg(feature = "coderlabs")]
            View::Tasks => self.open_tasks_tab(window, cx),
            View::Runs => self.open_runs_tab(None, window, cx),
            View::Branches => self.open_branches_tab(window, cx),
            View::GitHub => self.open_github_tab(window, cx),
            View::Notes => self.open_notes_tab(window, cx),
            View::Memory => self.open_memory_tab(window, cx),
            View::Usage => self.open_usage_tab(window, cx),
            View::Extensions => self.open_extensions_tab(window, cx),
            View::Cloudflare => self.open_cloudflare_tab(window, cx),
            View::PostHog => self.open_posthog_tab(window, cx),
            #[cfg(feature = "coderlabs")]
            View::Site => self.open_site_tab(window, cx),
            #[cfg(feature = "team")]
            View::Team => self.open_team_tab(window, cx),
            View::Isometric => self.open_iso_tab(window, cx),
            #[cfg(feature = "coderlabs")]
            View::Canvas => self.open_canvas_tab(None, window, cx),
            View::Phone => self.open_phone_tab(window, cx),
            View::Servers => self.open_servers_tab(window, cx),
            View::Automations => {
                if !self.has_projects() {
                    self.open_picker(window, cx);
                    return;
                }
                self.menu = Some(ContextMenu {
                    target: MenuTarget::Automations,
                    at: at + point(px(8.), px(0.)),
                });
                cx.notify();
            }
        }
    }

    pub(super) fn open_pin(&mut self, pin: Pin, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        match pin {
            Pin::Url { url, .. } => self.open_browser_tab(url, window, cx),
            Pin::File { path, .. } => self.open_doc_tab(TabKind::File { path }, window, cx),
            Pin::Command { command, .. } => self.spawn_terminal_command(
                Some(crate::platform::run_then_shell(&command)),
                true,
                window,
                cx,
            ),
        }
    }

    fn run_title_action(&mut self, a: TitleAction, window: &mut Window, cx: &mut Context<Self>) {
        match a {
            TitleAction::AddProject => self.open_picker(window, cx),
            TitleAction::NewSession => window.dispatch_action(Box::new(NewSession), cx),
            TitleAction::NewTerminal => window.dispatch_action(Box::new(NewTerminal), cx),
        }
    }

    /// A change from the rail itself (folding a segment, hiding a row): it
    /// can be undone in the builder like any other.
    pub(super) fn edit_sidebar(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut crate::sidebar::Sidebar),
    ) {
        self.change_sidebar(cx, f);
    }

    pub(super) fn hide_rail_item(&mut self, path: crate::sidebar::Path, cx: &mut Context<Self>) {
        self.menu = None;
        self.edit_sidebar(cx, |s| {
            s.remove(path);
        });
    }

    // ── Drawing ─────────────────────────────────────────────────────────

    /// The segments in the user's order. Fixed ones stay put; scrolling ones
    /// next to each other share one scroll between them.
    pub(super) fn render_rail_sections(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let narrow = self.rail_narrow();
        let mut out: Vec<AnyElement> = Vec::new();
        let mut run: Vec<AnyElement> = Vec::new();
        let flush = |run: &mut Vec<AnyElement>, out: &mut Vec<AnyElement>| {
            if run.is_empty() {
                return;
            }
            out.push(
                div()
                    .id(("rail-scroll", out.len()))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(std::mem::take(run))
                    .into_any_element(),
            );
        };
        for (si, section) in self.sidebar.sections.iter().enumerate() {
            if narrow && section.hide_in_icons {
                continue;
            }
            let el = self.render_section(si, section, cx);
            match section.mode {
                Mode::Scroll => run.push(el),
                Mode::Fixed => {
                    flush(&mut run, &mut out);
                    out.push(div().flex_none().child(el).into_any_element());
                }
            }
        }
        flush(&mut run, &mut out);
        // Nothing scrolls: the fixed segments still split top from bottom at
        // the first one past the middle, so the last sits at the bottom.
        if !self.sidebar.sections.iter().any(|s| s.mode == Mode::Scroll) && out.len() > 1 {
            let last = out.pop().expect("len > 1");
            out.push(div().flex_1().into_any_element());
            out.push(last);
        }
        out
    }

    fn render_section(&self, si: usize, section: &Section, cx: &mut Context<Self>) -> AnyElement {
        let narrow = self.rail_narrow();
        let folded = section.collapsible && section.collapsed && !narrow;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(1.))
            .px(crate::ui::sp(8.));
        if !folded {
            // Narrowed, every piece is one icon in a column, so Inbox keeps its row.
            let pair = if narrow {
                None
            } else {
                section.inbox_by_search()
            };
            for (ii, item) in section.items.iter().enumerate() {
                let el = match pair {
                    Some((_, inbox)) if inbox == ii => continue,
                    Some((search, inbox)) if search == ii => {
                        self.render_item((si, ii), item, cx).map(|search| {
                            self.render_search_with_inbox(
                                search,
                                (si, inbox),
                                &section.items[inbox],
                                cx,
                            )
                        })
                    }
                    _ => self.render_item((si, ii), item, cx),
                };
                if let Some(el) = el {
                    body = body.child(el);
                }
            }
        }
        // Folded, the segment still says when something inside wants the user.
        let urgent: usize = if folded {
            section
                .items
                .iter()
                .filter_map(|i| match i.piece {
                    Piece::View { view } => self
                        .view_badge(view, cx)
                        .filter(|b| b.urgent)
                        .map(|b| b.count),
                    _ => None,
                })
                .sum()
        } else {
            0
        };
        let title = (section.show_title && !narrow).then(|| {
            let collapsible = section.collapsible;
            div()
                .id(("rail-section", si))
                .group("rail-section")
                .flex()
                .items_center()
                .gap(crate::ui::sp(4.))
                .h(px(28.))
                .mx(px(8.))
                .px(crate::ui::sp(8.))
                .rounded(crate::ui::rad(8.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .when(collapsible, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(theme::hover()).text_color(theme::content()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit_sidebar(cx, |s| {
                                if let Some(sec) = s.sections.get_mut(si) {
                                    sec.collapsed = !sec.collapsed;
                                }
                            })
                        }))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(section.title.clone()),
                )
                .when(urgent > 0, |d| {
                    d.children(badge_pill(
                        Badge {
                            count: urgent,
                            color: Some(theme::error().into()),
                            urgent: true,
                        },
                        BadgeMode::Count,
                    ))
                })
                .when(collapsible, |d| {
                    d.child(
                        icon(
                            if section.collapsed {
                                "arrow-right01"
                            } else {
                                "arrow-down01"
                            },
                            12.,
                        )
                        .text_color(theme::text_faint()),
                    )
                })
                .when_some(section.action, |d, a| {
                    d.child(
                        div()
                            .id(("rail-section-action", si))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.))
                            .rounded(crate::ui::rad(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::fill_selected()))
                            .tooltip(ui::tip(a.label()))
                            .child(icon("add01", 14.).text_color(theme::text_subtle()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.run_title_action(a, window, cx)
                            })),
                    )
                })
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.menu = Some(ContextMenu {
                            target: MenuTarget::RailSection(si),
                            at: ev.position,
                        });
                        cx.notify();
                    }),
                )
        });
        div()
            .flex()
            .flex_col()
            .py(crate::ui::sp(4.))
            // Space, not a line, between sections.
            .when(section.line_above, |d| d.mt(crate::ui::sp(10.)))
            .children(title)
            .child(body)
            .into_any_element()
    }

    fn render_item(
        &self,
        path: crate::sidebar::Path,
        item: &Item,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let narrow = self.rail_narrow();
        let style = self.sidebar.style;
        let badge = match &item.piece {
            Piece::View { view } if style.badges => self.view_badge(*view, cx),
            _ => None,
        };
        if item.only_when_busy && badge.is_none() && matches!(item.piece, Piece::View { .. }) {
            return None;
        }
        let name = item.name();
        let lead: AnyElement = match &item.emoji {
            Some(e) => div()
                .w(px(16.))
                .text_size(px(ui::TEXT_MD))
                .child(e.clone())
                .into_any_element(),
            // The chrome stays a step under the content: small, faint icons.
            None => icon(item.piece.icon(), 15.)
                .text_color(theme::text_faint())
                .into_any_element(),
        };
        let row_id = SharedString::from(match &item.piece {
            Piece::View { view } => view_id(*view).to_string(),
            _ => format!("rail-item-{}-{}", path.0, path.1),
        });
        // A plain row: icon, name, then what goes at its end.
        let row = |end: Option<AnyElement>| {
            div()
                .id(gpui::ElementId::Name(row_id.clone()))
                .group("rail-row")
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .h(px(style.density.row_h()))
                .px(crate::ui::sp(8.))
                .rounded(crate::ui::rad(8.))
                .w_full()
                .text_size(px(ui::TEXT_MD))
                .text_color(theme::text_subtle())
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                .when(narrow, |d| {
                    d.justify_center()
                        .px(crate::ui::sp(0.))
                        .tooltip(ui::tip_text(name.clone()))
                })
                .child(lead)
                .when(!narrow, |d| {
                    d.child(div().flex_1().min_w_0().truncate().child(name.clone()))
                        .children(end)
                })
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.menu = Some(ContextMenu {
                            target: MenuTarget::RailBlock(path.0, Some(path.1)),
                            at: ev.position,
                        });
                        cx.notify();
                    }),
                )
        };
        let shortcut = |p: &Piece| {
            piece_shortcut(p).filter(|_| style.shortcuts).map(|s| {
                div()
                    .text_size(px(ui::TEXT_XS))
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_faint())
                    .child(crate::platform::shortcut_label(s))
                    .into_any_element()
            })
        };
        let el = match &item.piece {
            Piece::View { view } => {
                let view = *view;
                let pill = badge.and_then(|b| badge_pill(b, item.badge));
                row(pill.map(|p| p.into_any_element()))
                    // Narrowed, a count shows as a dot on the icon.
                    .when(narrow && badge.is_some(), |d| {
                        d.relative().child(
                            div()
                                .absolute()
                                .top(px(5.))
                                .right(px(10.))
                                .size(px(7.))
                                .rounded_full()
                                .bg(badge.map_or(theme::text_faint(), |b| b.color())),
                        )
                    })
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                        this.open_view(view, ev.position(), window, cx)
                    }))
                    .into_any_element()
            }
            Piece::Pin { pin } => {
                let open = pin.clone();
                row(None)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_pin(open.clone(), window, cx)
                    }))
                    .into_any_element()
            }
            Piece::Settings => row(shortcut(&item.piece))
                .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
                .into_any_element(),
            Piece::Search => {
                if narrow {
                    row(None)
                        .on_click(cx.listener(|this, _, window, cx| this.open_switcher(window, cx)))
                        .into_any_element()
                } else {
                    row(shortcut(&item.piece))
                        .bg(theme::fill_hover())
                        .my(px(2.))
                        .tooltip(ui::tip("Search everywhere (Ctrl+K)"))
                        .on_click(cx.listener(|this, _, window, cx| this.open_switcher(window, cx)))
                        .into_any_element()
                }
            }
            Piece::User => {
                let (git_name, email) = git_identity();
                let email = crate::redact::value(email);
                // The avatar's letter would give the name away too.
                let shown: String = if crate::redact::on() {
                    "You".into()
                } else {
                    item.label
                        .clone()
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| {
                            if git_name.is_empty() {
                                "You".into()
                            } else {
                                git_name.clone()
                            }
                        })
                };
                div()
                    .id(gpui::ElementId::Name(row_id.clone()))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .p(crate::ui::sp(6.))
                    .rounded(crate::ui::rad(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_selected()))
                    .when(narrow, |d| {
                        d.justify_center().tooltip(ui::tip_text(shown.clone()))
                    })
                    .child(avatar(&shown, if narrow { 26. } else { 28. }))
                    .when(!narrow, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(ui::TEXT_MD))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(shown.clone()),
                                )
                                .when(!email.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .truncate()
                                            .text_size(px(ui::TEXT_XS))
                                            .text_color(theme::text_faint())
                                            .child(email.clone()),
                                    )
                                }),
                        )
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.menu = Some(ContextMenu {
                                target: MenuTarget::RailBlock(path.0, Some(path.1)),
                                at: ev.position,
                            });
                            cx.notify();
                        }),
                    )
                    .into_any_element()
            }
            Piece::Usage => {
                if narrow {
                    return None;
                }
                div()
                    .mt(px(4.))
                    .children(self.render_usage_block(cx))
                    .into_any_element()
            }
            Piece::Projects {
                envs,
                changes,
                only_open,
            } => {
                if narrow {
                    self.render_projects_narrow(cx).into_any_element()
                } else {
                    self.render_projects_block(*envs, *changes, *only_open, cx)
                        .into_any_element()
                }
            }
            Piece::Mascot => div()
                .flex()
                .items_center()
                .justify_center()
                .h(px(40.))
                // Keeps its place, empty while the main view shows a raccoon.
                .when(self.rail_shows_raccoon(&self.inbox_items(), cx), |d| {
                    d.child(self.rail_mascot.clone())
                })
                .into_any_element(),
            Piece::Separator => div()
                .my(px(5.))
                .mx(px(8.))
                .h(px(1.))
                .bg(theme::stroke())
                .into_any_element(),
            Piece::Title { text } => {
                if narrow {
                    return None;
                }
                div()
                    .px(crate::ui::sp(8.))
                    .pt(crate::ui::sp(6.))
                    .pb(crate::ui::sp(2.))
                    .text_size(px(ui::TEXT_XS))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_faint())
                    .child(text.to_uppercase())
                    .into_any_element()
            }
            Piece::Spacer => div().h(px(16.)).into_any_element(),
        };
        Some(el)
    }

    /// Search with Inbox as a square button at its end, for when the user
    /// put the two next to each other.
    fn render_search_with_inbox(
        &self,
        search: AnyElement,
        path: crate::sidebar::Path,
        inbox: &Item,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .child(div().flex_1().min_w_0().child(search))
            .children(self.render_inbox_button(path, inbox, cx))
            .into_any_element()
    }

    /// The Inbox as a button the height of a rail row, with its count on
    /// the corner.
    fn render_inbox_button(
        &self,
        path: crate::sidebar::Path,
        item: &Item,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let style = self.sidebar.style;
        let badge = if style.badges {
            self.view_badge(View::Inbox, cx)
        } else {
            None
        };
        if item.only_when_busy && badge.is_none() {
            return None;
        }
        let open = matches!(
            self.menu,
            Some(ContextMenu {
                target: MenuTarget::Inbox,
                ..
            })
        );
        let lead: AnyElement = match &item.emoji {
            Some(e) => div()
                .text_size(px(ui::TEXT_MD))
                .child(e.clone())
                .into_any_element(),
            None => icon(item.piece.icon(), 15.)
                .text_color(if open {
                    theme::content()
                } else {
                    theme::text_subtle()
                })
                .into_any_element(),
        };
        let mark = badge.and_then(|b| self.inbox_mark(b, item.badge));
        Some(
            div()
                .id(view_id(View::Inbox))
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(style.density.row_h()))
                .rounded(crate::ui::rad(8.))
                .bg(if open {
                    theme::fill_selected()
                } else {
                    theme::fill_hover()
                })
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_selected()))
                // Open, the tooltip would sit over the Inbox's own title.
                .when(!open, |d| d.tooltip(ui::tip_text(item.name())))
                .child(lead)
                .children(mark)
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, window, cx| {
                    this.open_view(View::Inbox, ev.position(), window, cx)
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.menu = Some(ContextMenu {
                            target: MenuTarget::RailBlock(path.0, Some(path.1)),
                            at: ev.position,
                        });
                        cx.notify();
                    }),
                )
                .into_any_element(),
        )
    }

    /// The Inbox button's badge: a filled count on its top-right corner,
    /// ringed in the rail's ground so it stands off the button.
    fn inbox_mark(&self, b: Badge, mode: BadgeMode) -> Option<gpui::Div> {
        let ground = match self.sidebar.style.variant {
            crate::sidebar::Variant::Flush => theme::window_bg(),
            _ => theme::sidebar(),
        };
        let mark = div()
            .absolute()
            .rounded_full()
            .border_2()
            .border_color(ground)
            .bg(b.color());
        match mode {
            BadgeMode::Off => None,
            BadgeMode::Urgent if !b.urgent => None,
            BadgeMode::Dot => Some(mark.top(px(-2.)).right(px(-2.)).size(px(10.))),
            _ => Some(
                mark.top(px(-5.))
                    .right(px(-5.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(17.))
                    .min_w(px(17.))
                    .px(crate::ui::sp(3.))
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_XS))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if b.color == Some(theme::needs_you()) {
                        theme::on_accent()
                    } else {
                        theme::background()
                    })
                    .child(inbox_count_label(b.count)),
            ),
        }
    }

    pub(super) fn render_usage_block(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let seats = crate::agent::plan::latest_seats();
        account_view::rail_card(
            self.account.read(cx),
            seats.as_ref(),
            crate::agent::opencode_plan::latest().as_ref(),
            cx.listener(|this, _, window, cx| this.open_usage_tab(window, cx)),
            {
                let shell = cx.entity().downgrade();
                move |at, _, cx| {
                    let _ = shell.update(cx, |this, cx| {
                        this.open_composer_menu(MenuTarget::RailUsage, at, cx)
                    });
                }
            },
        )
    }

    /// Projects narrowed to icons: each project's art, the open one lit.
    fn render_projects_narrow(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_col().gap(crate::ui::sp(2.)).children(
            self.projects.iter().enumerate().map(|(pi, project)| {
                let selected = pi == self.active_project;
                div()
                    .id(("rail-project-icon", pi))
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(32.))
                    .rounded(crate::ui::rad(8.))
                    .cursor_pointer()
                    .tooltip(ui::tip_text(project.name.clone()))
                    .when(selected, |d| d.bg(theme::selection_strong()))
                    .when(!selected, |d| {
                        d.opacity(0.65).hover(|s| s.bg(theme::hover()).opacity(1.))
                    })
                    .child(ui::pixel_art(
                        ui::project_art(project.art),
                        1.5,
                        gpui::rgb(project.logo).into(),
                    ))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_project(pi, window, cx)),
                    )
            }),
        )
    }

    /// The right-click menu of a rail item.
    pub(super) fn render_rail_block_menu(
        &self,
        s: usize,
        i: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut panel = ui::menu_panel("rail-block-menu", 220.);
        if let Some(i) = i {
            panel = panel.child(
                ui::menu_item("rbm-hide", "view-off", "Hide from sidebar", false, true)
                    .on_click(cx.listener(move |this, _, _, cx| this.hide_rail_item((s, i), cx))),
            );
        }
        panel
            .child(ui::menu_divider())
            .child(
                ui::menu_item(
                    "rbm-customize",
                    "paint-board",
                    "Customize sidebar…",
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.menu = None;
                    this.open_sidebar_builder(window, cx)
                })),
            )
            .into_any_element()
    }

    /// The right-click menu of a segment's title.
    pub(super) fn render_rail_section_menu(&self, s: usize, cx: &mut Context<Self>) -> AnyElement {
        let collapsible = self
            .sidebar
            .sections
            .get(s)
            .is_some_and(|sec| sec.collapsible);
        ui::menu_panel("rail-section-menu", 220.)
            .child(
                ui::menu_item(
                    "rsm-fold",
                    "arrow-down01",
                    if collapsible {
                        "Always open"
                    } else {
                        "Make collapsible"
                    },
                    false,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.edit_sidebar(cx, |sb| {
                        if let Some(sec) = sb.sections.get_mut(s) {
                            sec.collapsible = !sec.collapsible;
                            sec.collapsed = false;
                        }
                    })
                })),
            )
            .child(
                ui::menu_item("rsm-hide", "view-off", "Remove segment", true, true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.edit_sidebar(cx, |sb| {
                            sb.remove_section(s);
                        })
                    }),
                ),
            )
            .child(ui::menu_divider())
            .child(
                ui::menu_item(
                    "rsm-customize",
                    "paint-board",
                    "Customize sidebar…",
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.menu = None;
                    this.open_sidebar_builder(window, cx)
                })),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inbox_count_stops_at_nine() {
        assert_eq!(inbox_count_label(1), "1");
        assert_eq!(inbox_count_label(9), "9");
        assert_eq!(inbox_count_label(10), "9+");
    }
}
