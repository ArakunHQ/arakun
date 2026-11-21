//! The sheet on the right: one pull request as pilot sees it. The bar with
//! where it is and the actions, the title, where it stands (who has it, what
//! comes next, the checks, the stack), what is going on, pilot's question
//! with its own answers, then the tabs over the composer.

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, div, px, relative,
};

use super::bandeja::{Who, estado_color};
use super::item::{Prop, prop_line, prop_row};
use super::{Confirm, GitHubView, GitHubViewEvent, Note, Tab};
use crate::github::board::{self, BoardPr, Estado, PasoTipo, Tipo};
use crate::github::pilot::{self, Answer, Opcion, TaskRequest, TaskTarget};
use crate::github::pr::{self, ItemKind, ItemRef};
use crate::github::queue::Details;
use crate::theme;
use crate::ui::{self, PillKind, Type, Typography as _, icon};

/// Below this height the sheet folds its details to their key line.
const SHEET_TALL: f32 = 1000.;

/// Properties one per row, or only the key ones on one line when folded.
fn prop_list(props: Vec<Prop>, folded: bool) -> gpui::Div {
    if folded {
        return prop_line(props);
    }
    let mut list = div().flex().flex_col().gap(crate::ui::sp(8.));
    for p in props {
        list = list.child(prop_row(p));
    }
    list
}

/// What the composer's "Ask pilot" does with `text` on pull request
/// `number`, or why it can't: pilot takes tasks only on the user's own pull
/// requests in its repository, only while they are open (as the palette
/// offers them), and only with the admin token.
pub(super) fn task_for(
    text: &str,
    number: Option<u64>,
    open: bool,
    mine: bool,
    in_repo: bool,
    admin: bool,
) -> Result<TaskRequest, &'static str> {
    let text = text.trim();
    let Some(n) = number else {
        return Err("Open a pull request to ask pilot for something.");
    };
    if !open {
        return Err("pilot works only on open pull requests.");
    }
    if !in_repo {
        return Err("pilot works only in ArakunHQ/arakun.");
    }
    if !mine {
        return Err("pilot takes tasks only on your pull requests; on others, comment.");
    }
    if !admin {
        return Err(pilot::NO_ADMIN);
    }
    if text.is_empty() {
        return Err("Write in the box what you want pilot to do.");
    }
    Ok(TaskRequest {
        // The palette's cut: the same task gets the same title from either.
        title: crate::palette::cloud::title(text),
        prompt: text.to_string(),
        target: TaskTarget::Pull(n),
    })
}

/// "16:41" in local time, for the steps of the pilot timeline.
fn clock(at: &str) -> String {
    let Some(t) = crate::ci::parse_time(at) else {
        return String::new();
    };
    match crate::platform::local_time(t) {
        Some(l) => format!("{:02}:{:02}", l.hour, l.minute),
        None => String::new(),
    }
}

/// A step's text and what goes under it: "Leyó la revisión · 4 hilos".
fn split_step(texto: &str) -> (String, Option<String>) {
    match texto.split_once(" · ") {
        Some((a, b)) if !a.trim().is_empty() && !b.trim().is_empty() => {
            (a.trim().to_string(), Some(b.trim().to_string()))
        }
        _ => (texto.trim().to_string(), None),
    }
}

/// The chip that says where a pull request stands.
fn estado_chip(e: Estado) -> gpui::Div {
    let color = estado_color(e);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .px(crate::ui::sp(8.))
        .py(crate::ui::sp(2.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .bg(color.opacity(0.12))
        .ty(Type::Label)
        .text_color(color)
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(e.label())
}

/// A quiet part after a value: "· 12 min ago".
fn after(text: impl Into<gpui::SharedString>) -> gpui::Div {
    div()
        .flex_none()
        .text_color(theme::text_subtle())
        .child(text.into())
}

impl GitHubView {
    /// The board's pull request for the item picked.
    pub(super) fn board_pr(&self, cx: &App) -> Option<BoardPr> {
        let item = self.item().filter(|i| i.kind == ItemKind::Pull)?;
        self.board(cx)?.find(&item.repo, item.number).cloned()
    }

    /// Whether the sheet shows over another window.
    pub fn floating(&self) -> bool {
        self.floating
    }

    /// From the island: the sheet of the pull request the notice `id` is
    /// about, over whatever window is in front. False when the board and the
    /// cards don't know it (the tab then shows it).
    pub fn float_pilot(&mut self, id: &str, over: bool, cx: &mut Context<Self>) -> bool {
        let from_board = self
            .board(cx)
            .and_then(|b| b.find_decision(id))
            .and_then(BoardPr::item_ref);
        let target = from_board.or_else(|| {
            self.pilot
                .read(cx)
                .find(id)
                .and_then(crate::github::pilot::Item::item_ref)
        });
        let Some(item) = target else { return false };
        self.floating = over;
        let title = self
            .board(cx)
            .and_then(|b| b.find(&item.repo, item.number))
            .map(|p| p.title.clone())
            .unwrap_or_default();
        // The tab may never have opened: "Ask pilot" needs to know whose
        // pull request this is.
        self.github.update(cx, |g, cx| g.want_me(cx));
        self.open_row(&item, &title, cx);
        true
    }

    /// The GitHub tab came to the front: the floating sheet stays open in it.
    pub fn unfloat(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.floating) {
            self.sheet_open = self.picked.is_some();
            cx.notify();
        }
    }

    /// Esc, the ×, or a click beside it: the sheet goes. Over another
    /// window, that window gets the keyboard back.
    pub(super) fn close_sheet(&mut self, cx: &mut Context<Self>) {
        self.sheet_open = false;
        self.confirm = None;
        self.editing_draft = false;
        if std::mem::take(&mut self.floating) {
            cx.emit(GitHubViewEvent::Closed);
        }
        cx.notify();
    }

    pub(super) fn render_sheet(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.picked.clone() else {
            return div().into_any_element();
        };
        let board = self.board_pr(cx);
        let item = entry.item.clone();
        let g = self.github.read(cx);
        let details = item.as_ref().and_then(|i| g.details(i).cloned());
        let problem = item.as_ref().and_then(|i| g.detail_problem(i).cloned());
        let reading = item.as_ref().is_some_and(|i| g.fetching(i));

        // A short sheet folds its details to one line, so the tabs under
        // them keep most of the height.
        let folded = self.details_folded().0;
        let mut top = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(20.))
            .px(crate::ui::sp(28.))
            .pt(crate::ui::sp(6.))
            .pb(crate::ui::sp(18.));
        let title = match (&board, &details) {
            (Some(b), _) if !b.title.is_empty() => b.title.clone(),
            (_, Some(Details::Pull(p))) => p.title.clone(),
            (_, Some(Details::Issue(i))) => i.title.clone(),
            _ => entry.title.clone(),
        };
        top = top.child(div().ty(Type::Title).text_color(theme::text()).child(title));
        if item.is_none() {
            // A notification about no pull request or issue.
            return self.sheet_frame(None, &board, reading, self.render_notice(cx), cx);
        }
        match (&board, &details) {
            (Some(b), _) => {
                top = top.child(self.board_props(b, details.as_ref(), folded, cx));
                if let Some(q) = b
                    .que_pasa
                    .as_ref()
                    .filter(|q| !folded && !q.trim().is_empty())
                {
                    // pilot writes markdown: `code` and **bold** read as such.
                    let body = self.markdown(
                        "gh-que-pasa",
                        q.trim(),
                        crate::agent::markdown::MdStyle {
                            color: theme::text(),
                            ..super::body_style()
                        },
                    );
                    top = top.child(que_pasa(body));
                }
                if b.open_decision().is_some() {
                    top = top.child(self.render_decision(b, cx));
                } else if let Some(line) = self.answered_line(b) {
                    top = top.child(line);
                }
                // pilot's cards about this pull request, above the timeline.
                if let Some(cards) = item.as_ref().and_then(|i| self.render_from_pilot(i, cx)) {
                    top = top.child(cards);
                }
            }
            (None, Some(d)) => {
                let props = match d {
                    Details::Pull(p) => self.pull_props(p, cx),
                    Details::Issue(i) => self.issue_props(i, cx),
                };
                top = top.child(prop_list(props, folded));
                top = top.child(match d {
                    Details::Pull(p) => self.pull_actions(p, cx),
                    Details::Issue(i) => self.issue_actions(i, cx),
                });
            }
            (None, None) => {
                top = top.child(match &problem {
                    Some(p) => self.detail_problem(item.as_ref(), p, cx),
                    None => ui::loading("Reading the pull request…").into_any_element(),
                });
            }
        }
        // A pull request pilot follows still shows where it stands, but the
        // tabs below need GitHub's read: say why it failed, with a retry.
        if let (Some(_), None, Some(p)) = (&board, &details, &problem) {
            top = top.child(self.detail_problem(item.as_ref(), p, cx));
        }
        // Under the details' scroll, not in it: unfolded ones can be folded
        // again without scrolling to their end.
        let toggle = (details.is_some() || board.is_some()).then(|| {
            div()
                .flex()
                .flex_none()
                .px(crate::ui::sp(28.))
                // Open details scroll and end mid-row: keep the toggle off them.
                .when(!folded, |d| d.pt(crate::ui::sp(10.)))
                .pb(crate::ui::sp(12.))
                .child(self.details_toggle(folded, cx))
        });
        let content = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("gh-sheet-top")
                    .flex()
                    .flex_col()
                    .flex_none()
                    // The tabs keep room under a long head.
                    .max_h(relative(0.45))
                    .overflow_y_scroll()
                    .child(top),
            )
            .children(toggle)
            .children(self.sheet_tabs(&board, details.as_ref(), cx))
            .child(super::Pane::cached(
                &self.body_pane,
                gpui::StyleRefinement::default().flex_1().min_h_0(),
            ))
            .child(self.render_composer(&board, cx))
            .into_any_element();
        self.sheet_frame(item, &board, reading, content, cx)
    }

    /// "All details" under folded details, "Fewer details" under unfolded
    /// ones; the choice holds for the next pull request too.
    fn details_toggle(&self, folded: bool, cx: &mut Context<Self>) -> AnyElement {
        let (label, glyph) = if folded {
            ("All details", "arrow-down01")
        } else {
            ("Fewer details", "arrow-up01")
        };
        div()
            .id("gh-details-toggle")
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .ty(Type::Caption)
            .text_color(theme::text_subtle())
            .cursor_pointer()
            .hover(|s| s.text_color(theme::text()))
            .child(label)
            .child(icon(glyph, 12.).text_color(theme::text_faint()))
            .tooltip(ui::tip(if folded {
                "All details (D)"
            } else {
                "Fewer details (D)"
            }))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx)))
            .into_any_element()
    }

    /// Whether the sheet's details are folded, and whether the height alone
    /// would fold them.
    fn details_folded(&self) -> (bool, bool) {
        let short = self.size.get().1 < SHEET_TALL;
        (self.details_open.map_or(short, |open| !open), short)
    }

    /// Folds the sheet's details, or unfolds them: the toggle and `D`.
    pub(super) fn toggle_details(&mut self, cx: &mut Context<Self>) {
        let (folded, short) = self.details_folded();
        // Back to automatic once it matches what the height picks.
        self.details_open = if folded == short { Some(folded) } else { None };
        cx.notify();
    }

    /// Why GitHub's read of `item` failed, and "Try again".
    fn detail_problem(
        &self,
        item: Option<&ItemRef>,
        p: &crate::github::Problem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(ui::text(
                Type::Body,
                theme::text_muted(),
                format!(
                    "Couldn't read {}: {}",
                    item.map(ItemRef::label).unwrap_or_default(),
                    p.text()
                ),
            ))
            .child(
                ui::button("gh-retry", "Try again", ui::ButtonKind::Outline, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        if let Some(item) = this.item() {
                            this.github.update(cx, |g, cx| g.fetch_item(item, cx));
                        }
                    }),
                ),
            )
            .into_any_element()
    }

    /// The bar and the confirm strip around the sheet's content.
    fn sheet_frame(
        &self,
        item: Option<ItemRef>,
        board: &Option<BoardPr>,
        reading: bool,
        content: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.sheet_bar(item, board, reading, cx))
            .children(self.render_strip(cx))
            .child(content)
            .into_any_element()
    }

    /// "arakun / #151 · Needs you", and the actions.
    fn sheet_bar(
        &self,
        item: Option<ItemRef>,
        board: &Option<BoardPr>,
        reading: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let repo = self.picked.as_ref().map(|e| e.repo()).unwrap_or_default();
        let short = repo.rsplit('/').next().unwrap_or_default().to_string();
        let state = match (
            board,
            self.item()
                .and_then(|i| self.github.read(cx).details(&i).cloned()),
        ) {
            (Some(b), _) => b.estado.label().to_string(),
            (None, Some(Details::Pull(p))) => super::item::pull_look(&p).2.to_string(),
            (None, Some(Details::Issue(i))) => if i.open() { "Open" } else { "Closed" }.into(),
            _ => String::new(),
        };
        let crumb = match &item {
            Some(i) if !state.is_empty() => format!("#{} · {state}", i.number),
            Some(i) => format!("#{}", i.number),
            None => String::new(),
        };
        let listo = board.as_ref().is_some_and(|b| b.estado == Estado::Listo);
        let busy = self.busy.is_some();
        let url = item.as_ref().map(ItemRef::url).or_else(|| {
            self.picked
                .as_ref()
                .and_then(|e| e.notification.as_ref())
                .map(|n| n.web_url())
        });
        let mine = self.is_mine(cx);
        let me = self.github.read(cx).me().unwrap_or_default().to_string();
        let merge = board.as_ref().is_some_and(|b| offers_merge(b, &me, mine));
        let open = matches!(self.item().and_then(|i| self.github.read(cx).details(&i).cloned()), Some(Details::Pull(p)) if p.open());
        let review = offers_review(listo, open, mine);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(28.))
            .pt(crate::ui::sp(18.))
            .pb(crate::ui::sp(12.))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .ty(Type::Label)
                    .font_weight(FontWeight::NORMAL)
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme::text_subtle())
                            .child(short),
                    )
                    .child(div().flex_none().text_color(theme::text_faint()).child("/"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::text_muted())
                            .child(crumb),
                    )
                    .when(reading, |d| d.child(ui::spinner(12., theme::text_faint()))),
            )
            .when(review, |d| {
                d.child(
                    ui::key_pill(
                        "gh-changes",
                        None,
                        "Request changes",
                        None,
                        PillKind::Quiet,
                        !busy,
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.ask_confirm(Confirm::RequestChanges, cx)),
                    ),
                )
                .child(
                    ui::key_pill(
                        "gh-approve",
                        Some(("tick02", None)),
                        "Approve",
                        Some("A"),
                        PillKind::Normal,
                        !busy,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.ask_confirm(Confirm::Approve, cx))),
                )
            })
            .when(merge, |d| {
                let can = self.merge_method().is_some() && !busy;
                d.child(
                    ui::key_pill(
                        "gh-merge",
                        Some(("git-merge", None)),
                        "Merge",
                        None,
                        PillKind::Primary,
                        can,
                    )
                    .when(can, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.ask_merge(cx)))
                    }),
                )
            })
            .child(
                ui::icon_button("gh-open-web", "link-square02", 14., "Open on GitHub (O)")
                    .on_click(cx.listener(|this, _, _, cx| this.open_on_github(cx))),
            )
            .when_some(url, |d, url| {
                d.child(
                    ui::icon_button("gh-copy-link", "link01", 14., "Copy link").on_click(
                        cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                            this.note = Some(Note::ok("Link copied."));
                            cx.notify();
                        }),
                    ),
                )
            })
            .child(
                ui::icon_button("gh-sheet-close", "cancel01", 14., "Close (Esc)")
                    .on_click(cx.listener(|this, _, _, cx| this.close_sheet(cx))),
            )
            .into_any_element()
    }

    /// Where a pull request pilot follows stands, as rows of icon, name, value.
    fn board_props(
        &self,
        b: &BoardPr,
        details: Option<&Details>,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let me = self.github.read(cx).me().unwrap_or_default().to_string();
        let mut props = vec![Prop::new("cs-progress", "Status", estado_chip(b.estado)).key()];
        let lt = &b.lo_tiene;
        if !lt.login.is_empty() || lt.tipo != Tipo::Otro {
            let who = Who {
                login: lt.login.clone(),
                tipo: lt.tipo,
            };
            props.push(
                Prop::new(
                    "user",
                    "Who has it",
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .child(self.who_face(&who, 20., cx))
                        .child(div().text_color(theme::text()).child(lt.shown())),
                )
                .key(),
            );
        }
        if !b.siguiente.trim().is_empty() {
            props.push(Prop::new(
                "arrow-right01",
                "Next step",
                div()
                    .text_color(theme::text())
                    .child(b.siguiente.trim().to_string()),
            ));
        }
        if let Some(u) = b
            .ultimo_pilot
            .as_ref()
            .filter(|u| !u.texto.trim().is_empty())
        {
            let when = board::hace(&u.at);
            props.push(Prop::new(
                "clock01",
                "pilot's latest",
                div()
                    .flex()
                    .flex_wrap()
                    .gap_x(crate::ui::sp(6.))
                    .child(
                        div()
                            .text_color(theme::text())
                            .child(u.texto.trim().to_string()),
                    )
                    .when(!when.is_empty(), |d| d.child(after(format!("· {when}")))),
            ));
        }
        let c = &b.checks;
        if c.total > 0 {
            let (color, rest): (Hsla, String) = if !c.fallando.is_empty() {
                let names = c.fallando.join(", ");
                (theme::error().into(), format!("· failing {names}"))
            } else if c.corriendo > 0 {
                (theme::working(), "· running".into())
            } else if c.pasaron >= c.total {
                (theme::success().into(), "· all passed".into())
            } else {
                (theme::text_subtle(), String::new())
            };
            props.push(
                Prop::new(
                    "check-list",
                    "Checks",
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .min_w_0()
                        .child(div().size(px(6.)).flex_none().rounded_full().bg(color))
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme::text())
                                .child(format!("{} of {}", c.pasaron, c.total)),
                        )
                        .when(!rest.is_empty(), |d| d.child(after(rest).truncate())),
                )
                .key(),
            );
        }
        if let Some(base) = b.va_encima_de.filter(|n| *n > 0) {
            let base_title = self
                .board(cx)
                .and_then(|bd| bd.find(&b.repo, base))
                .map(|p| p.title.clone())
                .unwrap_or_else(|| {
                    if b.base.is_empty() {
                        String::new()
                    } else {
                        b.base.clone()
                    }
                });
            let target = ItemRef {
                repo: b.repo.clone(),
                number: base,
                kind: ItemKind::Pull,
            };
            let t2 = base_title.clone();
            props.push(Prop::new(
                "git-branch",
                "Stacked on",
                div()
                    .id("gh-stack-base")
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .min_w_0()
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(2.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .bg(theme::fill_raised())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_hover()))
                    .child(super::mono(format!("#{base}"), theme::text_muted()).flex_none())
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::text())
                            .child(base_title),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open_row(&target, &t2, cx))),
            ));
        }
        // `listo` means approved and green: the author merges, not pilot.
        if let Some(al) = b.al_fusionar.as_ref().filter(|a| !a.trim().is_empty()) {
            props.push(Prop::new(
                "git-merge",
                "On merge",
                div().text_color(theme::text()).child(al.trim().to_string()),
            ));
        }
        // Someone else's pull request: whose it is, and whether it is a draft.
        let author = if b.author.is_empty() {
            match details {
                Some(Details::Pull(p)) => p.author.login.clone(),
                _ => String::new(),
            }
        } else {
            b.author.clone()
        };
        if !author.is_empty() && !author.eq_ignore_ascii_case(&me) {
            props.push(Prop::new(
                "git-pull-request",
                "Author",
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(self.face(&author, 20., cx))
                    .child(div().text_color(theme::text()).child(author.clone()))
                    .when(b.draft, |d| d.child(after("· draft"))),
            ));
        }
        prop_list(props, folded).into_any_element()
    }

    /// pilot's question with one button per answer, each with its own words.
    fn render_decision(&self, b: &BoardPr, cx: &mut Context<Self>) -> AnyElement {
        let Some(d) = b.open_decision() else {
            return div().into_any_element();
        };
        let id = d.item.clone();
        let sending = self.pilot_sending.contains(&id);
        let failed = self.pilot_notes.get(&id).cloned();
        let sent = self
            .answers
            .sent(&id)
            .map(str::to_string)
            .filter(|_| failed.is_none() && !sending);
        let mut buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(8.))
            .pl(crate::ui::sp(18.));
        if sent.is_none() {
            let mut first = true;
            for (ix, o) in d.opciones.iter().enumerate() {
                let kind = if o.quiet() {
                    DecisionKind::Quiet
                } else if std::mem::take(&mut first) {
                    DecisionKind::Primary
                } else {
                    DecisionKind::Normal
                };
                let (oid, opt) = (id.clone(), o.clone());
                buttons = buttons.child(decision_button(ix, &o.texto, kind, !sending).when(
                    !sending,
                    |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.answer_decision(&oid, opt.clone(), cx)
                        }))
                    },
                ));
            }
            // None of the ready answers may fit: the user's own words go too.
            let writing = self.pilot_other.as_deref() == Some(id.as_str());
            if !d.opciones.is_empty() && !writing {
                let oid = id.clone();
                buttons = buttons.child(
                    ui::key_pill(
                        "gh-decision-other",
                        None,
                        "Other answer…",
                        None,
                        PillKind::Quiet,
                        !sending,
                    )
                    .when(!sending, |b| {
                        b.on_click(cx.listener(move |this, _, window, cx| {
                            this.pilot_open_other(&oid, window, cx)
                        }))
                    }),
                );
            }
        }
        let text_row = sent.is_none()
            && (d.opciones.is_empty() || self.pilot_other.as_deref() == Some(id.as_str()));
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .p(crate::ui::sp(16.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .bg(theme::needs_you().opacity(0.07))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .child(
                        div()
                            .size(px(8.))
                            .flex_none()
                            .rounded_full()
                            .bg(theme::needs_you()),
                    )
                    .child(
                        div()
                            .ty(Type::Heading)
                            .text_color(theme::text())
                            .child(d.pregunta.trim().to_string()),
                    ),
            )
            .when(!d.opciones.is_empty() && sent.is_none(), |el| {
                el.child(buttons)
            })
            .when(text_row, |el| {
                el.child(self.decision_text_row(&id, sending, cx))
            })
            .when(sending, |el| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .pl(crate::ui::sp(18.))
                        .child(ui::spinner(12., theme::text_faint()))
                        .child(ui::text(
                            Type::Label,
                            theme::text_muted(),
                            "Sending it to pilot…",
                        )),
                )
            })
            .when_some(sent, |el, texto| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .pl(crate::ui::sp(18.))
                        .child(icon("tick02", 13.).text_color(theme::success()))
                        .child(ui::text(
                            Type::Label,
                            theme::text_muted(),
                            format!("You told pilot “{texto}”. It's on it."),
                        )),
                )
            })
            .when_some(failed, |el, why| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .pl(crate::ui::sp(18.))
                        .child(
                            div()
                                .size(px(6.))
                                .flex_none()
                                .rounded_full()
                                .bg(theme::error()),
                        )
                        .child(ui::text(Type::Label, theme::text_muted(), why)),
                )
            })
            .into_any_element()
    }

    /// The user's own words for pilot's question, with or without ready
    /// answers: they go to pilot, not to the pull request's comments.
    fn decision_text_row(&self, id: &str, sending: bool, cx: &mut Context<Self>) -> AnyElement {
        let row = div()
            .id("gh-decision-text")
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .pl(crate::ui::sp(18.));
        if self.pilot_other.as_deref() != Some(id) {
            let oid = id.to_string();
            return row
                .child(
                    div()
                        .ty(Type::Label)
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme::text_muted())
                        .child("pilot left no ready answers."),
                )
                .child(
                    ui::key_pill(
                        "gh-decision-answer",
                        None,
                        "Answer…",
                        None,
                        PillKind::Normal,
                        !sending,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pilot_open_other(&oid, window, cx)
                    })),
                )
                .into_any_element();
        }
        row
            // A click in the line stays in it and keeps the focus.
            .on_click(cx.listener(|_, _, _, cx| cx.stop_propagation()))
            .child(
                ui::field()
                    .flex_1()
                    .min_w_0()
                    .child(self.pilot_input.clone()),
            )
            .child(
                ui::key_pill(
                    "gh-decision-send",
                    None,
                    "Send",
                    Some("↵"),
                    PillKind::Normal,
                    !sending,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.pilot_send_other(cx);
                })),
            )
            .into_any_element()
    }

    /// After pilot took the answer and dropped the question: what was said.
    fn answered_line(&self, b: &BoardPr) -> Option<AnyElement> {
        let key = crate::github::queue::key_of(&b.item_ref()?);
        let (id, texto) = self.answers.on(&key)?;
        if self.pilot_notes.contains_key(id) || self.pilot_sending.contains(id) {
            return None;
        }
        Some(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .child(icon("tick02", 13.).text_color(theme::success()))
                .child(ui::text(
                    Type::Label,
                    theme::text_muted(),
                    format!("You told pilot “{texto}”. It's on it."),
                ))
                .into_any_element(),
        )
    }

    /// One answer to pilot's question: it goes to `POST /items/<id>/answer`
    /// and the board is read again.
    fn answer_decision(&mut self, id: &str, o: Opcion, cx: &mut Context<Self>) {
        if self.pilot_answer(id, Answer::Opcion(o.id), cx) {
            self.note_decision_answer(id, o.texto);
        }
    }

    /// Remembers what the user told pilot, to show it after the question goes.
    pub(super) fn note_decision_answer(&mut self, id: &str, texto: String) {
        let key = self.item().map(|i| crate::github::queue::key_of(&i));
        self.answers.note(id, key, texto);
    }

    /// The tabs under the head: what pilot did, the conversation, checks, files.
    fn sheet_tabs(
        &self,
        board: &Option<BoardPr>,
        details: Option<&Details>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let pull = match details {
            Some(Details::Pull(p)) => Some(p),
            Some(Details::Issue(_)) => return None,
            None => None,
        };
        if board.is_none() && pull.is_none() {
            return None;
        }
        let mut list: Vec<(Tab, &str, Option<String>)> = Vec::new();
        if board.is_some() {
            list.push((Tab::Pilot, "Pilot activity", None));
        }
        let talk = pull.map(|p| (p.comments.len() + p.reviews.len()).to_string());
        list.push((Tab::Timeline, "Conversation", talk));
        let checks = match (board, pull) {
            (Some(b), _) if b.checks.total > 0 => {
                Some(format!("{}/{}", b.checks.pasaron, b.checks.total))
            }
            (_, Some(p)) => {
                let s = pr::checks_summary(p);
                Some(format!("{}/{}", s.passed, s.total() - s.skipped))
            }
            _ => None,
        };
        list.push((Tab::Checks, "Checks", checks));
        list.push((
            Tab::Commits,
            "Commits",
            pull.map(|p| p.commits.len().to_string()),
        ));
        list.push((
            Tab::Files,
            "Files",
            pull.map(|p| p.changed_files.to_string()),
        ));
        // What was pushed after the user's last review, so they read that first.
        let me = self.github.read(cx).me().unwrap_or_default().to_string();
        let new_since = pull.map_or(0, |p| pr::new_since_my_review(p, &me).len());
        let tab = self.tab;
        // Five tabs and the new-commits note don't fit a narrow sheet: the
        // note wraps under the tabs instead of running off the edge.
        let mut row = div()
            .flex()
            .flex_wrap()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(4.))
            .px(crate::ui::sp(24.))
            .pb(crate::ui::sp(10.));
        for (t, label, count) in list {
            let on = t == tab;
            row = row.child(
                div()
                    .id(("gh-tab", t as usize))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .h(px(ui::BUTTON_H))
                    .px(crate::ui::sp(10.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .cursor_pointer()
                    .ty(Type::Label)
                    .when(on, |d| {
                        d.bg(theme::fill_selected()).text_color(theme::text())
                    })
                    .when(!on, |d| {
                        d.text_color(theme::text_subtle())
                            .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
                    })
                    .child(label)
                    .when_some(count, |d, c| d.child(super::mono(c, theme::text_faint())))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(t, cx))),
            );
        }
        if new_since > 0 {
            row = row.child(
                div()
                    .ml_auto()
                    .child(super::mono(new_since_text(new_since), theme::text_subtle())),
            );
        }
        Some(row.into_any_element())
    }

    /// "Pilot activity": the board's steps, then what comes next.
    pub(super) fn render_pilot_steps(&self, b: &BoardPr) -> AnyElement {
        let mut col = div()
            .flex()
            .flex_col()
            .px(crate::ui::sp(28.))
            .py(crate::ui::sp(12.))
            .gap(crate::ui::sp(14.));
        if b.timeline.is_empty() && b.siguiente.trim().is_empty() {
            return col
                .child(ui::text(
                    Type::Body,
                    theme::text_subtle(),
                    "pilot hasn't done anything on this pull request yet.",
                ))
                .into_any_element();
        }
        let steps = b.steps();
        let last = steps.len().saturating_sub(1);
        let working = b.estado == Estado::PilotTrabajando;
        let ci_running = b.checks.corriendo > 0;
        for (i, p) in steps.into_iter().enumerate() {
            // The last step is live while pilot works, or while CI runs on it.
            let live = i == last && (working || (ci_running && p.tipo == PasoTipo::Ci));
            let (mark, color): (&str, Hsla) = match p.tipo {
                _ if live => ("loading03", theme::working()),
                PasoTipo::Pregunta => ("help-circle", theme::needs_you()),
                PasoTipo::Merge => ("git-merge", theme::success().into()),
                PasoTipo::Revision => ("comment01", theme::text_subtle()),
                PasoTipo::Ci
                    if b.checks
                        .fallando
                        .iter()
                        .any(|f| p.texto.contains(f.as_str())) =>
                {
                    ("alert-circle", theme::error().into())
                }
                PasoTipo::Commit => ("git-commit", theme::text_subtle()),
                _ => ("tick02", theme::text_subtle()),
            };
            let (text, sub) = split_step(&p.texto);
            col = col.child(step_row(mark, color, text, sub, clock(&p.at), live));
        }
        let next = b.siguiente.trim();
        if !next.is_empty() {
            col = col.child(
                step_row(
                    "circle-dashed",
                    theme::text_faint(),
                    next.to_string(),
                    None,
                    String::new(),
                    false,
                )
                .opacity(0.7),
            );
        }
        col.into_any_element()
    }

    /// The box at the bottom: a comment for the pull request (Enter, or
    /// "Comentar"), or a task for pilot on the user's own.
    fn render_composer(&self, board: &Option<BoardPr>, cx: &mut Context<Self>) -> AnyElement {
        let sending = self.busy == Some("comment");
        let why_not = self.task_check(board, cx).err();
        let can_task = why_not.is_none();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(20.))
            .pt(crate::ui::sp(8.))
            .pb(crate::ui::sp(20.))
            .children(self.render_note_line(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(10.))
                    .px(crate::ui::sp(14.))
                    .pt(crate::ui::sp(12.))
                    .pb(crate::ui::sp(10.))
                    .rounded(crate::ui::rad(ui::RADIUS_LG))
                    .bg(theme::raised())
                    .child(div().ty(Type::Body).child(self.comment.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(10.))
                            .child(
                                div()
                                    .id("gh-ask-pilot")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap(crate::ui::sp(6.))
                                    .h(px(ui::CONTROL_H))
                                    .px(crate::ui::sp(10.))
                                    .rounded(crate::ui::rad(ui::RADIUS))
                                    .ty(Type::Label)
                                    .bg(theme::working().opacity(if can_task {
                                        0.14
                                    } else {
                                        0.06
                                    }))
                                    .text_color(if can_task {
                                        theme::working()
                                    } else {
                                        theme::text_faint()
                                    })
                                    .cursor_pointer()
                                    .child(if self.task_sending {
                                        ui::spinner(11., theme::working()).into_any_element()
                                    } else {
                                        icon("sparkles", 12.)
                                            .text_color(if can_task {
                                                theme::working()
                                            } else {
                                                theme::text_faint()
                                            })
                                            .into_any_element()
                                    })
                                    .child("Ask pilot")
                                    .when_some(why_not, |d, why| d.tooltip(ui::tip_text(why)))
                                    // Even when it can't go, a click says why in the note.
                                    .on_click(cx.listener(|this, _, _, cx| this.ask_pilot(cx))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_faint())
                                    .child("Enter posts · / for actions"),
                            )
                            .child(
                                ui::key_pill(
                                    "gh-comment-send",
                                    None,
                                    if sending { "Sending…" } else { "Comment" },
                                    Some("⏎"),
                                    PillKind::Primary,
                                    !sending,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.send_comment(cx))),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// Whether "Ask pilot" can go with what is typed, and the task.
    fn task_check(&self, board: &Option<BoardPr>, cx: &App) -> Result<TaskRequest, &'static str> {
        let item = self.item().filter(|i| i.kind == ItemKind::Pull);
        let me = self.github.read(cx).me().unwrap_or_default();
        let mine = self.is_mine(cx)
            || board
                .as_ref()
                .is_some_and(|b| !me.is_empty() && b.author.eq_ignore_ascii_case(me));
        let in_repo = item
            .as_ref()
            .is_some_and(|i| i.repo.eq_ignore_ascii_case(pilot::REPO));
        let admin = self
            .pilot
            .read(cx)
            .config()
            .is_some_and(|c| c.admin_token.is_some());
        // An empty box still enables the pill: the click says what to write.
        let text = self.comment.read(cx).text().to_string();
        let probe = if text.trim().is_empty() {
            "x"
        } else {
            text.as_str()
        };
        // Merged or closed: no task. Unread yet, the board's word stands.
        let open = item
            .as_ref()
            .and_then(|i| match self.github.read(cx).details(i) {
                Some(Details::Pull(p)) => Some(p.open()),
                _ => None,
            })
            .unwrap_or(true);
        task_for(probe, item.map(|i| i.number), open, mine, in_repo, admin)
    }

    /// "Ask pilot": what is in the box goes to pilot as a task on
    /// this pull request (`POST /tasks`), or the note says why it can't.
    fn ask_pilot(&mut self, cx: &mut Context<Self>) {
        if self.task_sending {
            self.note = Some(Note::quiet("Already sending pilot a task."));
            cx.notify();
            return;
        }
        let board = self.board_pr(cx);
        let text = self.comment.read(cx).text().to_string();
        let task = match self.task_check(&board, cx).and_then(|_| {
            let item = self.item();
            task_for(&text, item.map(|i| i.number), true, true, true, true)
        }) {
            Ok(t) => t,
            Err(why) => {
                self.note = Some(Note::quiet(why));
                cx.notify();
                return;
            }
        };
        let Some(cfg) = self.pilot.read(cx).config().cloned() else {
            return;
        };
        self.task_sending = true;
        self.note = None;
        cx.notify();
        let sent_text = text.clone();
        let on = self.item();
        cx.spawn(async move |this, cx| {
            let t = task.clone();
            let sent = cx
                .background_spawn(crate::guard::task(
                    async move { pilot::send_task(&cfg, &t) },
                ))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.task_sending = false;
                match sent {
                    Ok(id) => {
                        let id = if id.is_empty() {
                            String::new()
                        } else {
                            format!(" (task {id})")
                        };
                        this.note = Some(Note::ok(format!(
                            "pilot got the task{id}: “{}”. You'll see it in Pilot activity.",
                            task.title
                        )));
                        // Text typed since, or for another item, stays.
                        if super::same_item(this.item().as_ref(), on.as_ref())
                            && this.comment.read(cx).text() == sent_text
                        {
                            this.comment.update(cx, |i, cx| i.set_text("", cx));
                        } else if let Some(on) = &on {
                            let key = crate::github::queue::key_of(on);
                            if this.comment_drafts.get(&key) == Some(&sent_text) {
                                this.comment_drafts.remove(&key);
                            }
                        }
                        this.pilot.update(cx, |p, cx| p.refresh(cx));
                    }
                    Err(e) => {
                        this.note = Some(Note::failed(format!("pilot didn't get the task: {e}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// How a decision's button looks: the first real answer filled in amber,
/// the rest outlined, "Lo veo yo" as plain text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DecisionKind {
    Primary,
    Normal,
    Quiet,
}

fn decision_button(
    ix: usize,
    label: &str,
    kind: DecisionKind,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let base = div()
        .id(("gh-decision", ix))
        .flex()
        .flex_none()
        .items_center()
        .h(px(ui::BUTTON_H))
        .px(crate::ui::sp(12.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .ty(Type::Label)
        .font_weight(FontWeight::SEMIBOLD)
        .when(enabled, |d| d.cursor_pointer())
        .when(!enabled, |d| d.opacity(0.5))
        .child(label.to_string());
    match kind {
        DecisionKind::Primary => base
            .bg(theme::needs_you())
            .text_color(theme::on_accent())
            .hover(|s| s.opacity(0.9)),
        DecisionKind::Normal => base
            .bg(theme::background())
            .text_color(theme::text())
            .hover(|s| s.bg(theme::fill_hover())),
        DecisionKind::Quiet => base
            .text_color(theme::text_muted())
            .hover(|s| s.text_color(theme::text())),
    }
}

/// One step of "Pilot activity": its mark, its words, its time.
fn step_row(
    mark: &str,
    color: Hsla,
    text: String,
    sub: Option<String>,
    at: String,
    live: bool,
) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap(crate::ui::sp(12.))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(18.))
                .mt(px(1.))
                .child(icon(mark, 14.).text_color(color)),
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
                        .text_color(if live {
                            theme::text()
                        } else {
                            theme::text_muted()
                        })
                        .child(text),
                )
                .when_some(sub, |d, s| d.child(super::mono(s, theme::text_subtle()))),
        )
        .when(!at.is_empty(), |d| {
            d.child(
                super::mono(
                    if live { "now".to_string() } else { at },
                    if live {
                        theme::working()
                    } else {
                        theme::text_faint()
                    },
                )
                .flex_none(),
            )
        })
}

/// "What's going on": pilot's words on where the pull request is.
fn que_pasa(body: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(8.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .ty(Type::Label)
                .font_weight(FontWeight::NORMAL)
                .text_color(theme::text_subtle())
                .child(icon("left-to-right-list-bullet", 13.).text_color(theme::text_faint()))
                .child("What's going on")
                .child(div().flex_1())
                .child(icon("sparkles", 11.).text_color(theme::working()))
                .child("written by pilot"),
        )
        .child(body)
        .into_any_element()
}

/// "1 new since your review", "3 new since your review".
fn new_since_text(n: usize) -> String {
    if n == 1 {
        "1 new since your review".to_string()
    } else {
        format!("{n} new since your review")
    }
}

/// Whether the bar offers "Approve" and "Request changes": on someone else's
/// open pull request, pilot following it or not. A `Listo` one offers
/// "Merge" instead.
fn offers_review(listo: bool, open: bool, mine: bool) -> bool {
    !listo && open && !mine
}

/// Whether the bar offers "Merge": a `Listo` pull request, only to its
/// author or to whom the board says has it. The author merges, not pilot
/// and not a reviewer.
fn offers_merge(b: &BoardPr, me: &str, mine: bool) -> bool {
    let is_me = |login: &str| !me.is_empty() && login.eq_ignore_ascii_case(me);
    b.estado == Estado::Listo
        && (mine || is_me(&b.author) || b.lo_tiene.tipo == Tipo::Tu || is_me(&b.lo_tiene.login))
}

/// The board's pull request with the key `key` (`queue::key_of`).
fn board_pr_by_key<'a>(b: &'a board::Board, key: &str) -> Option<&'a BoardPr> {
    b.prs.iter().find(|p| {
        p.item_ref()
            .is_some_and(|r| crate::github::queue::key_of(&r) == key)
    })
}

/// pilot's last move on a pull request, to tell when it made a new one.
fn last_move(p: &BoardPr) -> Option<(String, String)> {
    p.ultimo_pilot
        .as_ref()
        .map(|u| (u.at.clone(), u.texto.clone()))
}

/// The decisions answered from the sheet, kept only while the board needs
/// them: while pilot still asks (it hasn't read the answer yet), then as the
/// "You told pilot…" line until pilot moves on.
#[derive(Default)]
pub(super) struct Answers {
    /// The words of each answer, by item id.
    sent: HashMap<String, String>,
    /// The decision answered last on each pull request (by `queue::key_of`).
    on: HashMap<String, String>,
    /// Answers whose question a board read no longer asked, with pilot's
    /// last move at that read. A newer move, or the same question back,
    /// ends them.
    gone: HashMap<String, Option<(String, String)>>,
}

impl Answers {
    /// The user told pilot `texto` about `id`, on the pull request `key`.
    pub(super) fn note(&mut self, id: &str, key: Option<String>, texto: String) {
        self.sent.insert(id.to_string(), texto);
        self.gone.remove(id);
        if let Some(key) = key {
            self.on.insert(key, id.to_string());
        }
    }

    /// The answer didn't go through, or pilot asks again: the buttons come back.
    pub(super) fn forget(&mut self, id: &str) {
        self.sent.remove(id);
        self.gone.remove(id);
        self.on.retain(|_, i| i != id);
    }

    /// What was answered to `id`.
    pub(super) fn sent(&self, id: &str) -> Option<&str> {
        self.sent.get(id).map(String::as_str)
    }

    /// The answer on the pull request `key`: its item id and words.
    pub(super) fn on(&self, key: &str) -> Option<(&str, &str)> {
        let id = self.on.get(key)?;
        Some((id.as_str(), self.sent.get(id)?.as_str()))
    }

    /// Takes in a board read: forgets what it is done with.
    pub(super) fn settle(&mut self, b: &board::Board) {
        let ids: Vec<String> = self.sent.keys().cloned().collect();
        for id in ids {
            let asked = b
                .find_decision(&id)
                .is_some_and(|p| p.open_decision().is_some());
            match (asked, self.gone.contains_key(&id)) {
                // Asked again after a read without it: a new question.
                (true, true) => self.forget(&id),
                // Still asked: pilot hasn't taken the answer yet.
                (true, false) | (false, true) => {}
                (false, false) => {
                    let then = self
                        .on
                        .iter()
                        .find(|(_, i)| **i == id)
                        .and_then(|(k, _)| board_pr_by_key(b, k))
                        .and_then(last_move);
                    self.gone.insert(id, then);
                }
            }
        }
        // The line goes once the pull request leaves the board, asks
        // something else, or shows a newer move by pilot.
        let gone = &self.gone;
        self.on.retain(|key, id| {
            let Some(p) = board_pr_by_key(b, key) else {
                return false;
            };
            if p.open_decision().is_some_and(|d| d.item != *id) {
                return false;
            }
            gone.get(id).is_none_or(|then| *then == last_move(p))
        });
        let (on, gone) = (&self.on, &self.gone);
        self.sent
            .retain(|id, _| !gone.contains_key(id) || on.values().any(|i| i == id));
        let sent = &self.sent;
        self.gone.retain(|id, _| sent.contains_key(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board with pull request #7, asking `asks` when given, pilot's last
    /// move at `moved`.
    fn board_with(asks: Option<&str>, moved: &str) -> board::Board {
        let decision = asks.map_or(serde_json::Value::Null, |id| {
            serde_json::json!({"item": id, "pregunta": "¿Sigo?",
                "opciones": [{"id": "si", "texto": "Sí"}]})
        });
        let json = serde_json::json!({"prs": [{
            "repo": "o/r", "number": 7, "decision": decision,
            "ultimo_pilot": {"texto": "algo", "at": moved},
        }]});
        board::parse_board(json.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn the_tabs_say_how_many_commits_are_new() {
        assert_eq!(new_since_text(1), "1 new since your review");
        assert_eq!(new_since_text(3), "3 new since your review");
    }

    #[test]
    fn only_the_author_is_offered_the_merge() {
        let pr = |author: &str, tipo: Tipo, login: &str| BoardPr {
            estado: Estado::Listo,
            author: author.into(),
            lo_tiene: board::LoTiene {
                login: login.into(),
                tipo,
                ..board::LoTiene::default()
            },
            ..BoardPr::default()
        };
        assert!(offers_merge(&pr("Karlo", Tipo::Otro, ""), "karlo", false));
        assert!(offers_merge(&pr("dani", Tipo::Tu, ""), "karlo", false));
        assert!(offers_merge(
            &pr("dani", Tipo::Persona, "karlo"),
            "karlo",
            false
        ));
        assert!(offers_merge(&pr("dani", Tipo::Otro, ""), "karlo", true));
        // Someone else's approved pull request: they merge it.
        assert!(!offers_merge(
            &pr("dani", Tipo::Persona, "dani"),
            "karlo",
            false
        ));
        assert!(!offers_merge(&pr("", Tipo::Otro, ""), "", false));
        // Not ready: no merge, whoever has it.
        let mut waiting = pr("karlo", Tipo::Tu, "");
        waiting.estado = Estado::Esperando;
        assert!(!offers_merge(&waiting, "karlo", true));
    }

    #[test]
    fn a_followed_pull_can_still_be_reviewed() {
        // Esperando or TeNecesita on the board: the review buttons show.
        assert!(offers_review(false, true, false));
        // Your own, a closed one, or one ready to merge: they don't.
        assert!(!offers_review(false, true, true));
        assert!(!offers_review(false, false, false));
        assert!(!offers_review(true, true, false));
    }

    #[test]
    fn an_answer_lasts_until_the_board_is_done_with_it() {
        let key = Some("o/r#7".to_string());
        let mut a = Answers::default();
        a.note("d1", key.clone(), "Sí".into());
        // pilot hasn't read it yet: the answer stands.
        a.settle(&board_with(Some("d1"), "t0"));
        assert_eq!(a.sent("d1"), Some("Sí"));
        // The question went: the line shows, again on the same read.
        let gone = board_with(None, "t1");
        a.settle(&gone);
        a.settle(&gone);
        assert_eq!(a.on("o/r#7"), Some(("d1", "Sí")));
        // A newer move by pilot ends the line.
        a.settle(&board_with(None, "t2"));
        assert_eq!(a.on("o/r#7"), None);
        assert_eq!(a.sent("d1"), None);

        // The same question back after it went: the buttons return.
        let mut a = Answers::default();
        a.note("d1", key.clone(), "Sí".into());
        a.settle(&board_with(None, "t1"));
        a.settle(&board_with(Some("d1"), "t1"));
        assert_eq!(a.sent("d1"), None);

        // Another question on the pull request, or the pull request gone.
        let mut a = Answers::default();
        a.note("d1", key.clone(), "Sí".into());
        a.settle(&board_with(Some("d2"), "t1"));
        assert_eq!(a.on("o/r#7"), None);
        let mut a = Answers::default();
        a.note("d1", key.clone(), "Sí".into());
        a.settle(&board::Board::default());
        assert_eq!(a.sent("d1"), None);

        // A send that failed leaves nothing behind.
        let mut a = Answers::default();
        a.note("d1", key, "Sí".into());
        a.forget("d1");
        assert_eq!(a.sent("d1"), None);
        assert_eq!(a.on("o/r#7"), None);
    }

    #[test]
    fn a_task_goes_only_on_your_own_pull_with_the_admin_key() {
        let ok = task_for(
            "Arregla el test\ny súbelo",
            Some(175),
            true,
            true,
            true,
            true,
        )
        .unwrap();
        assert_eq!(ok.title, "Arregla el test");
        assert_eq!(ok.prompt, "Arregla el test\ny súbelo");
        assert_eq!(ok.target, TaskTarget::Pull(175));
        assert_eq!(
            pilot::task_body(&ok),
            serde_json::json!({"title": "Arregla el test", "prompt": "Arregla el test\ny súbelo", "pr": 175})
        );
        // Each refusal says why.
        assert!(
            task_for("x", Some(1), true, false, true, true)
                .unwrap_err()
                .contains("your pull requests")
        );
        assert_eq!(
            task_for("x", Some(1), true, true, true, false).unwrap_err(),
            pilot::NO_ADMIN
        );
        assert!(
            task_for("x", Some(1), true, true, false, true)
                .unwrap_err()
                .contains("ArakunHQ/arakun")
        );
        assert!(
            task_for("  ", Some(1), true, true, true, true)
                .unwrap_err()
                .contains("Write in the box")
        );
        assert!(task_for("x", None, true, true, true, true).is_err());
        // Your own pull request, merged or closed: no task, and it says why.
        assert!(
            task_for("x", Some(1), false, true, true, true)
                .unwrap_err()
                .contains("open pull requests")
        );
        // A long first line is cut as the palette cuts it.
        let long = "arregla el tab ".repeat(20);
        let t = task_for(&long, Some(1), true, true, true, true).unwrap();
        assert_eq!(t.title, crate::palette::cloud::title(&long));
        assert!(t.title.chars().count() <= 72 && t.title.ends_with('…'));
    }

    #[test]
    fn a_step_splits_at_its_dot() {
        assert_eq!(
            split_step("Leyó la revisión de Rik · 4 hilos, ronda 1 de 3"),
            (
                "Leyó la revisión de Rik".to_string(),
                Some("4 hilos, ronda 1 de 3".to_string())
            )
        );
        assert_eq!(split_step("Rik aprobó"), ("Rik aprobó".to_string(), None));
        assert_eq!(clock("nope"), "");
    }
}
