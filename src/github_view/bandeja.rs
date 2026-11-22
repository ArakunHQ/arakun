//! The Bandeja: the verdict, then one quiet row per pull request grouped by
//! where it stands, and what was merged today folded at the bottom. Drawn
//! from pilot's board; without one, from the review queue.

use std::collections::HashSet;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::GitHubView;
use crate::github::board::{self, Board, BoardProblem, Estado, Tipo};
use crate::github::pr::ItemRef;
use crate::github::queue::{Entry, Group, Lane};
use crate::github::{Problem, pilot};
use crate::theme;
use crate::ui::{self, Type, Typography as _, icon};

/// The list's widest column: past it the rows would be hard to follow.
const LIST_MAX_W: f32 = 1180.;
/// The most rows a folded group shows once open: "Done today" runs to dozens.
const FOLD_MAX: usize = 20;

/// An estado's color: amber only for what needs the user, blue for work in
/// progress, green for done, red for a failure, gray for the rest.
pub(super) fn estado_color(e: Estado) -> Hsla {
    match e {
        Estado::TeNecesita => theme::needs_you(),
        Estado::PilotTrabajando | Estado::EsperandoCi => theme::working(),
        Estado::Listo => theme::success().into(),
        Estado::Atorado => theme::error().into(),
        Estado::Esperando | Estado::Otro => theme::text_subtle(),
    }
}

/// Who has a row, for its face.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Who {
    pub login: String,
    pub tipo: Tipo,
}

/// One row of the list.
#[derive(Clone, Debug)]
pub(super) struct Row {
    /// None for a notice about no pull request or issue (a release, a run).
    pub item: Option<ItemRef>,
    /// The queue's entry behind the row; board rows find theirs when opened.
    pub entry: Option<Entry>,
    pub title: String,
    /// What comes next, or the last thing that happened.
    pub line: String,
    pub who: Option<Who>,
    /// How long it has been like this ("12 min").
    pub since: String,
    /// Drawn under the pull request it is stacked on.
    pub nested: bool,
    pub color: Hsla,
    /// A failure: its dot says so in red.
    pub broken: bool,
    /// Its notification is unread: the row the alert or the island was about.
    pub unread: bool,
}

impl Row {
    /// The key the picked entry is matched on.
    pub(super) fn key(&self) -> String {
        match (&self.entry, &self.item) {
            (Some(e), _) => e.key(),
            (None, Some(i)) => crate::github::queue::key_of(i),
            (None, None) => String::new(),
        }
    }

    /// "#148 Title", or the title alone for a notice.
    fn hint(&self) -> String {
        match &self.item {
            Some(i) => format!("#{} {}", i.number, self.title),
            None => self.title.clone(),
        }
    }
}

/// A group of rows under its heading. Folded ones show one line.
pub(super) struct Section {
    pub id: &'static str,
    pub label: String,
    pub color: Hsla,
    pub rows: Vec<Row>,
    /// None: always open. Some(open) folds behind its line.
    pub fold: Option<bool>,
    /// The folded line's words after the count ("last: #148 …").
    pub hint: String,
}

/// The groups of a board, in order: every estado that has a pull request.
/// `unread` holds the keys (`queue::key_of`) with an unread notification.
fn board_sections(b: &Board, now_open: bool, unread: &HashSet<String>) -> Vec<Section> {
    let mut out = Vec::new();
    for estado in Estado::ALL {
        let rows: Vec<Row> = b
            .group(estado)
            .into_iter()
            .filter_map(|(p, nested)| {
                let item = p.item_ref()?;
                Some(Row {
                    unread: unread.contains(&crate::github::queue::key_of(&item)),
                    item: Some(item),
                    entry: None,
                    title: p.title.clone(),
                    line: p.line(),
                    who: (!p.lo_tiene.login.is_empty() || p.lo_tiene.tipo != Tipo::Otro).then(
                        || Who {
                            login: p.lo_tiene.login.clone(),
                            tipo: p.lo_tiene.tipo,
                        },
                    ),
                    since: board::since(&p.desde),
                    nested,
                    color: estado_color(estado),
                    // What needs the user stays amber even when CI broke.
                    broken: estado == Estado::Atorado
                        || (estado != Estado::TeNecesita && !p.checks.fallando.is_empty()),
                })
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push(Section {
            id: estado_id(estado),
            label: estado.group_label().to_string(),
            color: estado_color(estado),
            rows,
            fold: None,
            hint: String::new(),
        });
    }
    let done: Vec<Row> = b
        .hecho_hoy
        .iter()
        .filter_map(|h| {
            Some(Row {
                item: Some(h.item_ref()?),
                entry: None,
                title: h.title.clone(),
                line: String::new(),
                who: None,
                since: board::since(&h.merged_at),
                nested: false,
                color: theme::success().into(),
                broken: false,
                unread: false,
            })
        })
        .collect();
    if !done.is_empty() {
        let hint = done
            .first()
            .map(|r| format!("last: {}", r.hint()))
            .unwrap_or_default();
        out.push(Section {
            id: "hecho",
            label: "Done today".into(),
            color: theme::success().into(),
            rows: done,
            fold: Some(now_open),
            hint,
        });
    }
    out
}

fn estado_id(e: Estado) -> &'static str {
    match e {
        Estado::TeNecesita => "te-necesita",
        Estado::PilotTrabajando => "pilot",
        Estado::Esperando => "esperando",
        Estado::EsperandoCi => "ci",
        Estado::Listo => "listo",
        Estado::Atorado => "atorado",
        Estado::Otro => "otro",
    }
}

/// A queue row as a Bandeja row.
fn queue_row(e: &Entry, p: &crate::github::queue::Placed) -> Row {
    let color = match (p.group, p.lane) {
        (Group::NeedsReview | Group::Mentions, _) | (_, Lane::OnYou) => theme::needs_you(),
        (Group::Ready, _) => theme::success().into(),
        (Group::Done, _) => theme::success().into(),
        _ => theme::text_subtle(),
    };
    let who = if p.author.is_empty() {
        e.author.clone()
    } else {
        p.author.clone()
    };
    Row {
        item: e.item.clone(),
        entry: Some(e.clone()),
        title: e.title.clone(),
        line: p.sub.clone(),
        who: (!who.is_empty()).then_some(Who {
            login: who,
            tipo: Tipo::Persona,
        }),
        since: board::span(crate::model::unix_now().saturating_sub(e.updated())),
        nested: false,
        color,
        broken: p.checks == Some(crate::github::pr::CheckState::Failed),
        unread: e.unread(),
    }
}

impl GitHubView {
    /// The board, when pilot has one.
    pub(super) fn board<'a>(&self, cx: &'a App) -> Option<&'a Board> {
        self.pilot.read(cx).board()
    }

    /// The groups the list shows, in order.
    pub(super) fn sections(&self, cx: &App) -> Vec<Section> {
        let q = self.github.read(cx).queue();
        let Some(b) = self.board(cx) else {
            // No board: the queue's groups, done today folded at the end.
            let mut out = Vec::new();
            for group in Group::ALL {
                let rows: Vec<Row> = q.group(group).map(|(e, p)| queue_row(e, p)).collect();
                if rows.is_empty() {
                    continue;
                }
                let done = group == Group::Done;
                let hint = rows
                    .first()
                    .filter(|_| done)
                    .map(|r| format!("last: {}", r.hint()))
                    .unwrap_or_default();
                out.push(Section {
                    id: group_id(group),
                    label: group.label().to_string(),
                    color: match group {
                        Group::NeedsReview | Group::Mentions | Group::Issues => theme::needs_you(),
                        Group::Ready | Group::Done => theme::success().into(),
                        _ => theme::text_subtle(),
                    },
                    rows,
                    fold: done.then_some(self.done_open),
                    hint,
                });
            }
            return out;
        };
        // One pass over the queue, not one per board row.
        let unread: HashSet<String> = q
            .rows
            .iter()
            .filter(|(e, _)| e.unread())
            .map(|(e, _)| e.key())
            .collect();
        let mut out = board_sections(b, self.done_open, &unread);
        // What the queue holds that pilot doesn't follow: review requests
        // elsewhere, mentions, issues. Folded: pilot covers the rest.
        let more: Vec<Row> = board::outside(b, q).map(|(e, p)| queue_row(e, p)).collect();
        if !more.is_empty() {
            let at = out
                .iter()
                .position(|s| s.id == "hecho")
                .unwrap_or(out.len());
            let hint = more.first().map(Row::hint).unwrap_or_default();
            out.insert(
                at,
                Section {
                    id: "mas",
                    label: "More on GitHub".into(),
                    color: theme::text_subtle(),
                    rows: more,
                    fold: Some(self.more_open),
                    hint,
                },
            );
        }
        out
    }

    /// The rows J and K walk: every open group's that opens something. Only
    /// the one stepped to looks its entry up.
    pub(super) fn walkable(&self, cx: &App) -> Vec<Row> {
        self.sections(cx)
            .into_iter()
            .filter(|s| s.fold != Some(false))
            .flat_map(|s| {
                let cap = self.fold_cap(&s);
                s.rows.into_iter().take(cap)
            })
            .filter(|r| r.entry.is_some() || r.item.is_some())
            .collect()
    }

    /// How many rows a section shows: a folded one stops at FOLD_MAX until
    /// the user asks for the rest.
    fn fold_cap(&self, s: &Section) -> usize {
        if s.fold.is_some() && !self.fold_full.contains(s.id) {
            FOLD_MAX
        } else {
            usize::MAX
        }
    }

    /// The entry a row opens.
    pub(super) fn row_entry(&self, r: &Row, cx: &App) -> Option<Entry> {
        r.entry
            .clone()
            .or_else(|| Some(self.entry_for(r.item.as_ref()?, &r.title, cx)))
    }

    /// The queue's entry for `item`, or one made for it: a pull request the
    /// queue doesn't hold (pilot follows it, the palette found it).
    pub(super) fn entry_for(&self, item: &ItemRef, title: &str, cx: &App) -> Entry {
        let key = crate::github::queue::key_of(item);
        self.github
            .read(cx)
            .queue()
            .rows
            .iter()
            .find(|(e, _)| e.key() == key)
            .map(|(e, _)| e.clone())
            .unwrap_or_else(|| Entry {
                item: Some(item.clone()),
                title: if title.is_empty() {
                    item.label()
                } else {
                    title.to_string()
                },
                author: String::new(),
                updated_at: String::new(),
                closed_at: String::new(),
                state: String::new(),
                why: Vec::new(),
                notification: None,
            })
    }

    pub(super) fn open_row(&mut self, item: &ItemRef, title: &str, cx: &mut Context<Self>) {
        let entry = self.entry_for(item, title, cx);
        self.select(entry, true, cx);
    }

    pub(super) fn render_bandeja(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let sections = self.sections(cx);
        // J and K move the pick with the sheet closed: the row shows what
        // Enter opens.
        let picked = self.picked.as_ref().map(Entry::key);
        let mut list = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(LIST_MAX_W))
            .px(crate::ui::sp(40.))
            .pt(crate::ui::sp(28.))
            .pb(crate::ui::sp(40.))
            .gap(crate::ui::sp(28.))
            .child(self.render_verdict(cx));
        let problems = self.render_problems(cx);
        let has_problems = !problems.is_empty();
        list = list.children(problems);
        // pilot's cards: all of them without a board; with one, those no
        // sheet shows ("From pilot").
        let cards = self.pilot_section(cx);
        let has_cards = !cards.is_empty();
        if has_cards {
            list = list.child(div().flex().flex_col().children(cards));
        }
        let mut ix = 0;
        let empty = sections.is_empty() && !has_cards;
        for s in sections {
            list = list.child(self.render_section(s, &mut ix, picked.as_deref(), cx));
        }
        if empty && !has_problems {
            list = list.child(self.render_empty(cx));
        }
        div()
            .id("gh-bandeja")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .overflow_y_scroll()
            .track_scroll(&self.queue_scroll)
            .child(list)
            .into_any_element()
    }

    /// "1 thing needs you · pilot has 4 PRs", over where it is read from.
    fn render_verdict(&self, cx: &mut Context<Self>) -> AnyElement {
        let g = self.github.read(cx);
        let (need, rest, texto) = match self.board(cx) {
            Some(b) => {
                // What waits in "More on GitHub" and pilot's cards need the
                // user too: the same count as the badge.
                let cards = board::visible_cards(b, self.pilot.read(cx).shown(), g.focus());
                let need = board::waiting(b, g.queue(), cards);
                let lleva = b.verdict.pilot_lleva as usize;
                let rest = match lleva {
                    0 => String::new(),
                    1 => "pilot has 1 PR".to_string(),
                    n => format!("pilot has {n} PRs"),
                };
                // pilot's words count only its pull requests: with more
                // waiting elsewhere, the count speaks instead.
                let texto = if need > b.verdict.te_necesita as usize {
                    String::new()
                } else {
                    b.verdict.texto.trim().to_string()
                };
                (need, rest, texto)
            }
            None => {
                let q = g.queue();
                let rest = match q.tracked() {
                    0 => String::new(),
                    n => format!("{n} on GitHub"),
                };
                // The badge counts pilot's cards too.
                (
                    board::waiting_without_board(q, self.pilot.read(cx).shown_in(g.focus())),
                    rest,
                    String::new(),
                )
            }
        };
        let counted = match need {
            0 => "Nothing needs you".to_string(),
            1 => "1 thing needs you".to_string(),
            n => format!("{n} things need you"),
        };
        // pilot's own words win: "Nada te necesita · pilot lleva 2 PRs".
        let (head, rest, texto) = verdict_parts(&texto, counted, rest);
        let place = match g.focus() {
            _ if self.board(cx).is_some() => pilot::REPO.to_string(),
            [] => "All repositories".to_string(),
            [one] => one.clone(),
            many => format!("{} places", many.len()),
        };
        let busy = g.busy();
        let on_board = self.board(cx).is_some();
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(icon("github", 14.).text_color(theme::text_faint()))
                    .child(
                        div()
                            .id("gh-focus")
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(4.))
                            .ty(Type::Label)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme::text_subtle())
                            .child(place)
                            // The focus filters the review queue, not the board
                            // (one repository): with a board, what shows under
                            // "More on GitHub".
                            .map(|d| {
                                d.cursor_pointer()
                                    .hover(|s| s.text_color(theme::text()))
                                    .tooltip(ui::tip(if on_board {
                                        "Choose which organizations and repositories show under More on GitHub"
                                    } else {
                                        "Choose which organizations and repositories show"
                                    }))
                                    .child(
                                        icon("arrow-down01", 11.).text_color(theme::text_subtle()),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.focus_at = if this.focus_at.is_some() {
                                                None
                                            } else {
                                                Some(ev.position + gpui::point(px(-12.), px(14.)))
                                            };
                                            this.menu_at = None;
                                            cx.notify();
                                        }),
                                    )
                            }),
                    )
                    .child(div().flex_1())
                    .child(if busy {
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(ui::ICON_BUTTON))
                            .child(ui::spinner(13., theme::text_faint()))
                            .into_any_element()
                    } else {
                        ui::icon_button("gh-menu", "more-horizontal", 15., "Refresh, mark read, notices")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.menu_at = if this.menu_at.is_some() {
                                        None
                                    } else {
                                        Some(ev.position + gpui::point(px(-240.), px(14.)))
                                    };
                                    this.confirm_all = None;
                                    cx.notify();
                                }),
                            )
                            // Enter or Space only fires clicks, without a
                            // pointer: open under the button, where a mouse
                            // press on its middle would. A mouse click was
                            // already handled on the press.
                            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                                if !ev.is_keyboard() {
                                    return;
                                }
                                let half = ui::ICON_BUTTON / 2.;
                                this.menu_at = if this.menu_at.is_some() {
                                    None
                                } else {
                                    Some(ev.position() + gpui::point(px(half - 240.), px(14. - half)))
                                };
                                this.confirm_all = None;
                                cx.notify();
                            }))
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .when(need > 0, |d| {
                        d.child(
                            div()
                                .size(px(10.))
                                .flex_none()
                                .rounded_full()
                                .bg(theme::needs_you()),
                        )
                    })
                    .child(
                        div()
                            .ty(Type::Display)
                            .text_color(theme::text())
                            .child(head),
                    )
                    .when(!rest.is_empty(), |d| {
                        d.child(
                            div()
                                .ty(Type::Display)
                                .font_weight(FontWeight::NORMAL)
                                .text_color(theme::text_faint())
                                .child(format!("· {rest}")),
                        )
                    }),
            )
            .when(!texto.is_empty(), |d| {
                d.child(ui::text(Type::Body, theme::text_muted(), texto))
            })
            .into_any_element()
    }

    /// Why the list may be short: GitHub or pilot couldn't be read.
    fn render_problems(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let line = |id: &'static str, text: String| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .ty(Type::Label)
                .font_weight(FontWeight::NORMAL)
                .text_color(theme::text_muted())
                .child(
                    div()
                        .size(px(6.))
                        .flex_none()
                        .rounded_full()
                        .bg(theme::error()),
                )
                .child(div().min_w_0().line_clamp(2).child(text))
                .into_any_element()
        };
        if let Some(BoardProblem::Failed(e)) = self.pilot.read(cx).board_problem() {
            let text = format!("Couldn't read pilot's board: {e}");
            out.push(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(line("gh-board-problem", text))
                    .child(super::pilot::settings_link("gh-board-fix", cx))
                    .into_any_element(),
            );
        }
        let g = self.github.read(cx);
        match g.problem().or(g.search_problem()) {
            Some(p @ (Problem::NoGh | Problem::SignedOut)) => {
                out.push(line("gh-problem", p.text()))
            }
            Some(Problem::Other(e)) => {
                out.push(line("gh-problem", format!("Couldn't read GitHub: {e}")))
            }
            None => {}
        }
        out
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let g = self.github.read(cx);
        let loading = (!g.loaded(false) || !g.searched())
            || (self.board(cx).is_none()
                && self.pilot.read(cx).board_problem().is_none()
                && self.pilot.read(cx).config().is_some());
        if loading {
            return ui::loading("Reading your queue…").into_any_element();
        }
        let connected = self.pilot.read(cx).config().is_some();
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap(crate::ui::sp(10.))
            .child(ui::text(
                Type::Body,
                theme::text_subtle(),
                "Nothing going on. What pilot works on and what people ask of you shows up here.",
            ))
            // Without pilot the queue is GitHub's alone: say how to add it.
            .when(!connected, |d| {
                d.child(
                    ui::button(
                        "gh-pilot-connect",
                        "Connect arakun-pilot…",
                        ui::ButtonKind::Outline,
                        true,
                    )
                    .on_click(
                        cx.listener(|_, _, _, cx| cx.emit(super::GitHubViewEvent::ConnectPilot)),
                    ),
                )
            })
            .into_any_element()
    }

    fn render_section(
        &self,
        s: Section,
        ix: &mut usize,
        picked: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = s.rows.len();
        let mut col = div().flex().flex_col().gap(crate::ui::sp(2.));
        match s.fold {
            None => {
                col = col.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .px(crate::ui::sp(12.))
                        .pb(crate::ui::sp(6.))
                        .child(div().size(px(6.)).flex_none().rounded_full().bg(s.color))
                        .child(
                            div()
                                .ty(Type::Label)
                                .text_color(theme::text_muted())
                                .child(s.label.clone()),
                        )
                        .child(super::mono(count.to_string(), theme::text_faint())),
                );
            }
            Some(open) => {
                let id = s.id;
                col = col.child(
                    div()
                        .id(("gh-fold", *ix))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .px(crate::ui::sp(12.))
                        .py(crate::ui::sp(6.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .cursor_pointer()
                        .hover(|st| st.bg(theme::fill_hover()))
                        .child(
                            icon(
                                if open {
                                    "arrow-down01"
                                } else {
                                    "arrow-right01"
                                },
                                11.,
                            )
                            .text_color(theme::text_faint()),
                        )
                        .child(div().size(px(6.)).flex_none().rounded_full().bg(s.color))
                        .child(
                            div()
                                .flex_none()
                                .ty(Type::Label)
                                .text_color(theme::text_muted())
                                .child(s.label.clone()),
                        )
                        .child(super::mono(count.to_string(), theme::text_faint()).flex_none())
                        .when(!open && !s.hint.is_empty(), |d| {
                            d.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_faint())
                                    .child(s.hint.clone()),
                            )
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if id == "mas" {
                                this.more_open = !this.more_open;
                            } else {
                                this.done_open = !this.done_open;
                            }
                            cx.notify();
                        })),
                );
                if !open {
                    *ix += 1;
                    return col.into_any_element();
                }
            }
        }
        let cap = self.fold_cap(&s);
        let past = count.saturating_sub(cap);
        let id = s.id;
        for r in s.rows.into_iter().take(cap) {
            let selected = picked == Some(r.key().as_str());
            col = col.child(self.render_bandeja_row(*ix, r, selected, cx));
            *ix += 1;
        }
        if past > 0 {
            col = col.child(
                div()
                    .id(("gh-fold-all", *ix))
                    .px(crate::ui::sp(12.))
                    .pt(crate::ui::sp(4.))
                    .ty(Type::Caption)
                    .text_color(theme::text_faint())
                    .cursor_pointer()
                    .hover(|st| st.text_color(theme::text()))
                    .child(format!("Show {past} more"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.fold_full.insert(id);
                        cx.notify();
                    })),
            );
        }
        col.into_any_element()
    }

    fn render_bandeja_row(
        &self,
        ix: usize,
        r: Row,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let face = r.who.as_ref().map(|w| self.who_face(w, 20., cx));
        let number = r
            .item
            .as_ref()
            .map(|i| format!("#{}", i.number))
            .unwrap_or_default();
        div()
            .id(("gh-bandeja-row", ix))
            .flex()
            .items_center()
            .gap(crate::ui::sp(14.))
            .px(crate::ui::sp(12.))
            .h(px(40.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .cursor_pointer()
            .when(selected, |d| d.bg(theme::fill_selected()))
            // After J or K: once the row is laid out, the list scrolls just
            // enough to show it.
            .when(selected, |d| {
                let (flag, list) = (self.reveal_pick.clone(), self.queue_scroll.clone());
                d.relative().child(
                    gpui::canvas(
                        move |row, window, _| {
                            if !flag.replace(false) {
                                return;
                            }
                            let view = list.bounds();
                            let mut off = list.offset();
                            if let Some(y) = reveal_offset(
                                (row.top().as_f32(), row.bottom().as_f32()),
                                (view.top().as_f32(), view.bottom().as_f32()),
                                off.y.as_f32(),
                            ) {
                                off.y = px(y);
                                list.set_offset(off);
                                window.refresh();
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .when(!selected, |d| d.hover(|st| st.bg(theme::fill_hover())))
            // A stacked pull request sits under its base; the columns stay put.
            .when(r.nested, |d| d.child(div().w(px(18.)).flex_none()))
            .child(
                div()
                    .size(px(6.))
                    .flex_none()
                    .rounded_full()
                    .bg(if r.broken {
                        theme::error().into()
                    } else {
                        r.color
                    }),
            )
            .child(
                super::mono(number, theme::text_faint())
                    .flex_none()
                    .w(px(44.)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if selected || r.unread {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .font_weight(if r.unread {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .child(r.title.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .w(relative_w(0.34))
                    .min_w_0()
                    .truncate()
                    .ty(Type::Label)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_subtle())
                    .child(r.line.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(20.))
                    .justify_center()
                    .children(face),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(72.))
                    .items_center()
                    .justify_end()
                    .gap(crate::ui::sp(6.))
                    .when(r.unread, |d| {
                        d.child(div().size(px(6.)).rounded_full().bg(theme::link()))
                    })
                    .child(super::mono(r.since.clone(), theme::text_faint())),
            )
            // The entry is looked up on the click, not on every frame: a
            // board row's means a walk through the whole queue.
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(e) = this.row_entry(&r, cx) {
                    this.select(e, true, cx);
                }
                window.focus(&this.focus, cx);
            }))
            .into_any_element()
    }

    /// The face of who has a pull request: a picture, or pilot's and CI's marks.
    pub(super) fn who_face(&self, w: &Who, size: f32, cx: &App) -> AnyElement {
        match w.tipo {
            Tipo::Pilot => {
                ui::icon_chip("sparkles", size, Some(theme::working())).into_any_element()
            }
            Tipo::Ci => {
                ui::icon_chip("activity01", size, Some(theme::text_subtle())).into_any_element()
            }
            Tipo::Tu => {
                let me = self.github.read(cx).me().unwrap_or_default().to_string();
                let login = if me.is_empty() { w.login.clone() } else { me };
                self.face(&login, size, cx).into_any_element()
            }
            _ if w.login.is_empty() => ui::icon_chip("user", size, None).into_any_element(),
            _ => self.face(&w.login, size, cx).into_any_element(),
        }
    }
}

/// The verdict's big part, its quiet part and the sentence under them.
/// pilot's `texto` splits at its first " · "; a longer one goes under the
/// counted heading instead.
fn verdict_parts(texto: &str, counted: String, rest: String) -> (String, String, String) {
    let t = texto.trim();
    if t.is_empty() {
        return (counted, rest, String::new());
    }
    match t.split_once(" · ") {
        Some((a, b)) if a.chars().count() <= 48 => {
            (a.trim().to_string(), b.trim().to_string(), String::new())
        }
        _ if t.chars().count() <= 48 => (t.to_string(), String::new(), String::new()),
        _ => (counted, rest, t.to_string()),
    }
}

/// The list's new vertical offset that brings a row (`top`, `bottom`) into
/// the view (`top`, `bottom`), moving as little as it can; None when it
/// already shows. Offsets grow negative as the list scrolls down.
fn reveal_offset(row: (f32, f32), view: (f32, f32), off: f32) -> Option<f32> {
    if row.0 < view.0 {
        Some(off + (view.0 - row.0))
    } else if row.1 > view.1 {
        Some(off - (row.1 - view.1))
    } else {
        None
    }
}

/// A share of the row's width.
fn relative_w(f: f32) -> gpui::DefiniteLength {
    gpui::relative(f)
}

fn group_id(g: Group) -> &'static str {
    match g {
        Group::NeedsReview => "q-review",
        Group::Mentions => "q-mentions",
        Group::Yours => "q-yours",
        Group::Issues => "q-issues",
        Group::Ready => "q-ready",
        Group::Other => "q-other",
        Group::Done => "hecho",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_groups_in_order_with_done_folded() {
        let b = board::parse_board(board::FIXTURE.as_bytes()).unwrap();
        let first = b.group(Estado::TeNecesita)[0].0.item_ref().unwrap();
        let unread = HashSet::from([crate::github::queue::key_of(&first)]);
        let s = board_sections(&b, false, &unread);
        let ids: Vec<&str> = s.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            [
                "te-necesita",
                "atorado",
                "pilot",
                "ci",
                "esperando",
                "listo",
                "hecho"
            ]
        );
        assert_eq!(s.last().unwrap().fold, Some(false));
        assert!(s.last().unwrap().hint.starts_with("last: #183"));
        // Only what needs the user is amber.
        assert_eq!(s[0].color, theme::needs_you());
        assert!(s[1..].iter().all(|s| s.color != theme::needs_you()));
        // A failing check turns the dot red, except where amber says it needs you.
        assert!(!s[0].rows[0].broken);
        assert!(s[1].rows[0].broken);
        assert!(!s[2].rows[0].broken);
        // A row with an unread notification says so; the rest don't.
        assert!(s[0].rows[0].unread);
        assert_eq!(
            s.iter().flat_map(|s| &s.rows).filter(|r| r.unread).count(),
            1
        );
    }

    #[test]
    fn a_picked_row_scrolls_into_view() {
        // View from 100 to 500, list at the top.
        assert_eq!(reveal_offset((200., 240.), (100., 500.), 0.), None);
        // Below: scroll down just enough.
        assert_eq!(reveal_offset((480., 520.), (100., 500.), 0.), Some(-20.));
        // Above: scroll back up just enough.
        assert_eq!(reveal_offset((60., 100.), (100., 500.), -300.), Some(-260.));
    }

    #[test]
    fn the_verdict_takes_pilots_words() {
        let c = || {
            (
                "1 thing needs you".to_string(),
                "pilot has 2 PRs".to_string(),
            )
        };
        let (a, b) = c();
        assert_eq!(
            verdict_parts("Nada te necesita · pilot lleva 2 PRs", a, b),
            (
                "Nada te necesita".into(),
                "pilot lleva 2 PRs".into(),
                String::new()
            )
        );
        let (a, b) = c();
        assert_eq!(verdict_parts("", a, b), (c().0, c().1, String::new()));
        let long =
            "Una decisión en #151 y lo demás va solo, sin prisa: pilot lleva cuatro PRs hoy.";
        let (a, b) = c();
        assert_eq!(verdict_parts(long, a, b), (c().0, c().1, long.to_string()));
    }
}
