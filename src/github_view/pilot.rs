//! "Your agent": the cards arakun-pilot leaves (`crate::github::pilot`), at the
//! top of the queue. With a board, a card shows in the sheet of the pull
//! request it is about ("From pilot"), or in the Bandeja's own group when the
//! board doesn't follow that pull request. Every write to GitHub goes through
//! the tab's own confirm strip, as the user, with `gh`.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use super::{Confirm, GitHubView, NEWLINE, Note, Tab};
use crate::github::Problem;
use crate::github::board::{self, CardPlace};
use crate::github::pilot::{self, Answer, Item, Kind, Opcion, Riesgo};
use crate::github::pr::{self, ItemRef, Verdict};
use crate::theme;
use crate::ui::{self, PillKind, Tone, Type, Typography as _, icon};

/// An approval or change request that came from a card: which item to mark
/// done once GitHub took it, and the commit the agent read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PilotAct {
    pub id: String,
    pub item: ItemRef,
    pub head: String,
}

fn kind_tone(k: Kind) -> Tone {
    match k {
        Kind::Brief => Tone::Accent,
        Kind::Fixed => Tone::Success,
        Kind::NeedsYou => Tone::Warning,
        Kind::Failed => Tone::Danger,
        Kind::Other => Tone::Neutral,
    }
}

fn riesgo_tone(r: Riesgo) -> Tone {
    match r {
        Riesgo::Bajo => Tone::Success,
        Riesgo::Medio => Tone::Warning,
        Riesgo::Alto => Tone::Danger,
        Riesgo::Otro => Tone::Neutral,
    }
}

/// How long "Enviado" stays after an answer went.
const SENT_SHOWS: std::time::Duration = std::time::Duration::from_secs(8);

/// How each ready answer looks: the first real answer is the primary
/// button; "Lo veo yo" (`skip`) stays quiet wherever it is.
fn opcion_kinds(opciones: &[Opcion]) -> Vec<PillKind> {
    let primary = opciones.iter().position(|o| !o.quiet());
    opciones
        .iter()
        .enumerate()
        .map(|(i, o)| {
            if o.quiet() {
                PillKind::Quiet
            } else if Some(i) == primary {
                PillKind::Primary
            } else {
                PillKind::Normal
            }
        })
        .collect()
}

/// Where the `card`th card sits among the queue's rows: after GitHub's
/// problem line, the "Your agent" heading, the sent note and the Worker's
/// problem, as `render_queue_body` and `pilot_section` lay them out.
fn card_row(github_problem: bool, sent: bool, pilot_problem: bool, card: usize) -> usize {
    usize::from(github_problem) + 1 + usize::from(sent) + usize::from(pilot_problem) + card
}

/// Why an approval from a card is refused: the pull request moved on.
fn moved_text(number: u64, read: &str, now: &str) -> String {
    let short = |s: &str| s.chars().take(7).collect::<String>();
    format!(
        "#{number} has new commits since the summary ({} → {}). I didn't approve it: review it again.",
        short(read),
        short(now)
    )
}

/// The review a card sends: only for the commit the agent read, and only
/// while it is still the head. GitHub gets the full SHA, since a card may
/// carry a short one.
fn pinned_job(item: &ItemRef, verdict: Verdict, body: &str, head: &str) -> Result<(), Problem> {
    let now = pr::head_sha(item)?;
    if !pilot::same_commit(&now, head) {
        return Err(Problem::Other(moved_text(item.number, head, &now)));
    }
    pr::review_at(item, verdict, body, &now)
}

/// A quote under a card's text: why it matters, or what the agent asks.
fn quote(color: Hsla, text: impl Into<gpui::SharedString>, strong: bool) -> gpui::Div {
    div()
        .pl(crate::ui::sp(8.))
        .border_l_2()
        .border_color(color.opacity(0.6))
        .ty(Type::Label)
        .font_weight(if strong {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .text_color(if strong {
            theme::text()
        } else {
            theme::text_muted()
        })
        .child(text.into())
}

/// The keys of a picked card's ready answers, in order (`OPCIONES_MAX`).
const ANSWER_KEYS: [&str; 4] = ["1", "2", "3", "4"];

/// The card to pick once `gone` is dealt with: the next question after it,
/// else the next card, wrapping round; None when it was the last.
fn next_card(cards: &[Item], gone: &str) -> Option<String> {
    let at = cards.iter().position(|c| c.id == gone).unwrap_or(0);
    let rest: Vec<&Item> = cards[at..]
        .iter()
        .chain(&cards[..at])
        .filter(|c| c.id != gone)
        .collect();
    rest.iter()
        .find(|c| c.kind == Kind::NeedsYou)
        .or(rest.first())
        .map(|c| c.id.clone())
}

/// What a key does on the picked card.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CardKey {
    Answer(Answer),
    Other,
    Done,
}

/// The key `key` on `card`, when it does something there.
pub(super) fn card_key(card: &Item, key: &str) -> Option<CardKey> {
    match (card.kind, key) {
        (Kind::NeedsYou, "e") => Some(CardKey::Other),
        (Kind::NeedsYou, k) => {
            let n = ANSWER_KEYS.iter().position(|a| *a == k)?;
            let o = card.opciones.get(n)?;
            Some(CardKey::Answer(Answer::Opcion(o.id.clone())))
        }
        (Kind::Fixed | Kind::Failed | Kind::Other, "y") => Some(CardKey::Done),
        _ => None,
    }
}

/// "Settings…" after a pilot problem: the form that fixes it.
pub(super) fn settings_link(
    id: &'static str,
    cx: &mut Context<GitHubView>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .ty(Type::Caption)
        .text_color(theme::link())
        .cursor_pointer()
        .hover(|s| s.underline())
        .child("Settings…")
        .on_click(cx.listener(|_, _, _, cx| cx.emit(super::GitHubViewEvent::ConnectPilot)))
}

impl GitHubView {
    /// The section's heading and its cards; nothing when no card waits.
    /// With a board, only the cards no sheet shows: "From pilot".
    /// The cards the queue shows, in its order: all of them without a
    /// board, with one those no sheet shows.
    pub(super) fn pilot_cards(&self, cx: &App) -> Vec<Item> {
        let p = self.pilot.read(cx);
        let board = p.board();
        // The focus menu's promise: what it leaves out doesn't show.
        let focus = self.github.read(cx).focus();
        p.shown_in(focus)
            .filter(|c| board.is_none_or(|b| board::card_place(b, c) == CardPlace::Loose))
            .cloned()
            .collect()
    }

    pub(super) fn pilot_section(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let items = self.pilot_cards(cx);
        let p = self.pilot.read(cx);
        let board = p.board();
        // Broken settings show here too: pilot would just look off.
        let problem = p
            .config_problem()
            .map(|e| format!("pilot is off: {e}"))
            .or_else(|| {
                p.problem()
                    .map(|e| format!("Couldn't reach your agent: {e}"))
            });
        let sent = self.pilot_sent.as_ref().map(|(t, _)| t.clone());
        if items.is_empty() && problem.is_none() && sent.is_none() {
            return Vec::new();
        }
        let label = if board.is_some() {
            "From pilot"
        } else {
            "Your agent"
        };
        let mut out = vec![super::queue::group_header(label, items.len()).into_any_element()];
        // The answered card is gone; this says where the answer went.
        if let Some(text) = sent {
            out.push(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .px(crate::ui::sp(16.))
                    .pb(crate::ui::sp(6.))
                    .child(icon("tick02", 12.).text_color(theme::success()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_muted())
                            .child(text),
                    )
                    .into_any_element(),
            );
        }
        // A Worker that can't be read says so, or its cards would just stop coming.
        if let Some(e) = problem {
            out.push(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .px(crate::ui::sp(16.))
                    .pb(crate::ui::sp(6.))
                    .child(icon("alert-circle", 12.).text_color(theme::error()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::error())
                            .child(e),
                    )
                    .child(settings_link("gh-pilot-fix", cx))
                    .into_any_element(),
            );
        }
        for (ix, item) in items.iter().enumerate() {
            out.push(self.render_pilot_card(ix, item, cx));
        }
        out
    }

    /// "From pilot" in the sheet of a pull request the board follows: its
    /// briefs and fixed or failed cards, with their own actions ("Looks good"
    /// approves the commit pilot read). None without a board or a card.
    pub(super) fn render_from_pilot(
        &self,
        pr: &ItemRef,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let p = self.pilot.read(cx);
        let b = p.board()?;
        let key = crate::github::queue::key_of(pr);
        let cards: Vec<Item> = p
            .shown()
            .filter(|c| board::card_place(b, c) == CardPlace::Sheet)
            .filter(|c| {
                c.item_ref()
                    .is_some_and(|r| crate::github::queue::key_of(&r) == key)
            })
            .cloned()
            .collect();
        if cards.is_empty() {
            return None;
        }
        let mut col = div()
            .flex()
            .flex_col()
            .child(super::queue::group_header("From pilot", cards.len()));
        for (ix, card) in cards.iter().enumerate() {
            col = col.child(self.render_pilot_card(ix, card, cx));
        }
        Some(col.into_any_element())
    }

    fn render_pilot_card(&self, ix: usize, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        let id = item.id.clone();
        let focused = self.pilot_focus.as_deref() == Some(id.as_str());
        let open = self.pilot_open.contains(&id);
        let busy = self.busy.is_some();
        let pr_ref = item.item_ref();
        let g = self.github.read(cx);
        let lead = if item.author.is_empty() {
            ui::icon_chip("sparkles", 32., Some(theme::accent()))
        } else {
            ui::avatar_badge(
                &item.author,
                g.avatar(&item.author),
                32.,
                "sparkles",
                theme::accent(),
                if focused {
                    theme::fill_raised()
                } else {
                    theme::background()
                },
            )
        };
        let when = crate::ci::parse_time(&item.at).unwrap_or(0);

        let top = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .min_w_0()
            .child(ui::badge(item.kind.label(), kind_tone(item.kind)))
            .when(item.importante, |d| {
                d.child(ui::state_pill(
                    "alert-circle",
                    "Important",
                    theme::attention(),
                ))
            })
            .child(div().flex_1())
            .child(super::mono(super::ago(when), theme::text_subtle()).flex_none());
        // Like a queue row's second line: where it is, then who. The
        // repository shows too: an approval lands there.
        let place = match (&pr_ref, item.repo.trim()) {
            (Some(r), _) => r.label(),
            (None, "") => format!("#{}", item.number),
            (None, repo) => format!("{repo}#{}", item.number),
        };
        let meta = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .min_w_0()
            .when(item.number > 0 || pr_ref.is_some(), |d| {
                d.child(super::mono(place, theme::text_subtle()).flex_none())
            })
            .when(!item.author.is_empty(), |d| {
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .ty(Type::Caption)
                        .text_color(theme::text_subtle())
                        .child(item.author.clone()),
                )
            });

        let title = div()
            .ty(Type::Body)
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text())
            .line_clamp(2)
            .child(if item.title.is_empty() {
                item.repo.clone()
            } else {
                item.title.clone()
            });

        let mut body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(crate::ui::sp(8.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(4.))
                    .child(top)
                    .child(title)
                    .child(meta),
            );
        if !item.resumen.is_empty() {
            body = body.child(ui::text(
                Type::Label,
                theme::text_muted(),
                item.resumen.clone(),
            ));
        }
        if item.kind == Kind::NeedsYou && !item.pregunta.is_empty() {
            body = body.child(quote(
                theme::attention().into(),
                item.pregunta.clone(),
                true,
            ));
        }
        if item.importante && !item.por_que.is_empty() {
            body = body.child(quote(
                theme::attention().into(),
                item.por_que.clone(),
                false,
            ));
        }
        if !item.toca.is_empty() || item.riesgo.is_some() {
            body = body.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(crate::ui::sp(4.))
                    .when_some(item.riesgo, |d, r| {
                        d.child(ui::badge(r.label(), riesgo_tone(r)))
                    })
                    .children(
                        item.toca
                            .iter()
                            .map(|t| ui::badge(t.clone(), Tone::Neutral)),
                    ),
            );
        }
        if item.kind == Kind::Fixed && !item.commits.is_empty() {
            body = body.child(div().flex().flex_col().gap(crate::ui::sp(2.)).children(
                item.commits.iter().map(|c| {
                    let (sha, subject) = c.split_once(' ').unwrap_or((c, ""));
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .min_w_0()
                        .child(icon("git-commit", 12.).text_color(theme::text_subtle()))
                        .child(
                            super::mono(
                                sha.chars().take(7).collect::<String>(),
                                theme::text_subtle(),
                            )
                            .flex_none(),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .ty(Type::Label)
                                .font_weight(FontWeight::NORMAL)
                                .text_color(theme::text_muted())
                                .child(subject.to_string()),
                        )
                }),
            ));
        }
        if !item.puntos.is_empty() {
            let n = item.puntos.len();
            let toggle_id = id.clone();
            body = body.child(
                div()
                    .id(("gh-pilot-points", ix))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(4.))
                    .cursor_pointer()
                    .ty(Type::Label)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme::text_subtle())
                    .hover(|s| s.text_color(theme::text()))
                    .child(
                        icon(if open { "arrow-up01" } else { "arrow-down01" }, 11.)
                            .text_color(theme::text_subtle()),
                    )
                    .child(if n == 1 {
                        "1 point".to_string()
                    } else {
                        format!("{n} points")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if !this.pilot_open.remove(&toggle_id) {
                            this.pilot_open.insert(toggle_id.clone());
                        }
                        cx.notify();
                    })),
            );
            if open {
                let mut list = div().flex().flex_col().gap(crate::ui::sp(6.));
                for (pi, p) in item.puntos.iter().enumerate() {
                    let target = pr_ref.clone();
                    let place = p.place();
                    list = list.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(2.))
                            .when(!place.is_empty(), |d| {
                                d.child(
                                    div()
                                        .id(("gh-pilot-point", ix * 100 + pi))
                                        .flex()
                                        .items_center()
                                        .gap(crate::ui::sp(4.))
                                        .min_w_0()
                                        .cursor_pointer()
                                        .text_color(theme::link())
                                        .hover(|s| s.text_color(theme::text()))
                                        .child(icon("file01", 12.).text_color(theme::link()))
                                        .child(
                                            div().min_w_0().truncate().ty(Type::Mono).child(place),
                                        )
                                        .tooltip(ui::tip("See the pull request's files"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            if let Some(r) = &target {
                                                this.reveal(r, cx);
                                                this.set_tab(Tab::Files, cx);
                                            }
                                        })),
                                )
                            })
                            .child(ui::text(Type::Label, theme::text_muted(), p.texto.clone())),
                    );
                }
                body = body.child(list);
            }
        }

        // The buttons, by kind. Every one stops the click from also picking the card.
        // They take the card's whole width, under the picture too: three
        // fit in the narrowest queue.
        let mut buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(6.));
        let see = {
            let it = item.clone();
            ui::key_pill(
                ("gh-pilot-see", ix),
                None,
                "See",
                None,
                PillKind::Normal,
                pr_ref.is_some(),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.pilot_see(&it, cx);
            }))
        };
        let done = |label: &'static str, primary: bool, cx: &mut Context<Self>| {
            let done_id = id.clone();
            ui::key_pill(
                ("gh-pilot-done", ix),
                Some(("tick02", None)),
                label,
                focused.then_some("Y"),
                if primary {
                    PillKind::Primary
                } else {
                    PillKind::Normal
                },
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.pilot_finish(&done_id, cx);
            }))
        };
        match item.kind {
            Kind::Brief => {
                let (a, r) = (item.clone(), item.clone());
                buttons = buttons
                    .child(
                        ui::key_pill(
                            ("gh-pilot-approve", ix),
                            Some(("tick02", None)),
                            "Looks good",
                            None,
                            PillKind::Primary,
                            !busy && pr_ref.is_some(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.pilot_approve(&a, cx);
                        })),
                    )
                    .child(
                        ui::key_pill(
                            ("gh-pilot-changes", ix),
                            None,
                            "Request changes",
                            None,
                            PillKind::Normal,
                            !busy && pr_ref.is_some(),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.pilot_request_changes(&r, window, cx);
                            },
                        )),
                    )
                    .child(see);
            }
            Kind::Fixed => {
                buttons = buttons.child(done("Ok", true, cx)).child(see);
            }
            // A question: its ready answers when it has some, and always the
            // user's own words.
            Kind::NeedsYou => {
                let sending = self.pilot_sending.contains(&id);
                for ((oi, o), kind) in item
                    .opciones
                    .iter()
                    .enumerate()
                    .zip(opcion_kinds(&item.opciones))
                {
                    let (aid, answer) = (id.clone(), Answer::Opcion(o.id.clone()));
                    buttons = buttons.child(
                        ui::key_pill(
                            ("gh-pilot-opcion", ix * 10 + oi),
                            None,
                            o.texto.clone(),
                            // The picked card answers from the keyboard.
                            focused.then(|| ANSWER_KEYS[oi]),
                            kind,
                            !sending,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.pilot_answer(&aid, answer.clone(), cx);
                        })),
                    );
                }
                let other_id = id.clone();
                buttons = buttons.child(see).child(
                    div()
                        .id(("gh-pilot-other", ix))
                        .cursor_pointer()
                        .ty(Type::Label)
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme::text_subtle())
                        .hover(|s| s.text_color(theme::text()))
                        .child(if focused {
                            "Other answer… (E)"
                        } else {
                            "Other answer…"
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.pilot_open_other(&other_id, window, cx);
                        })),
                );
            }
            Kind::Failed | Kind::Other => {
                buttons = buttons.child(see).child(done("Done", false, cx));
            }
        }
        let mut foot = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .child(buttons);
        if self.pilot_other.as_deref() == Some(id.as_str()) {
            let sending = self.pilot_sending.contains(&id);
            foot = foot.child(
                div()
                    .id(("gh-pilot-other-row", ix))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    // A click in the line stays in it: picking the card
                    // would take the focus from the input.
                    .on_click(cx.listener(|_, _, _, cx| cx.stop_propagation()))
                    .child(
                        ui::field()
                            .flex_1()
                            .min_w_0()
                            .h(px(30.))
                            .child(self.pilot_input.clone()),
                    )
                    .child(
                        ui::key_pill(
                            ("gh-pilot-other-send", ix),
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
                    ),
            );
        }
        if self.pilot_sending.contains(&id) {
            foot = foot.child(ui::text(
                Type::Caption,
                theme::text_subtle(),
                "Sending your answer…",
            ));
        }
        if let Some(note) = self.pilot_notes.get(&id) {
            foot = foot.child(
                div()
                    .flex()
                    .items_start()
                    .gap(crate::ui::sp(6.))
                    .child(
                        icon("alert-circle", 13.)
                            .text_color(theme::error())
                            .flex_none()
                            .mt(px(2.)),
                    )
                    .child(div().flex_1().min_w_0().child(ui::text(
                        Type::Caption,
                        theme::error().into(),
                        note.clone(),
                    ))),
            );
        }

        let pick = item.clone();
        div()
            .id(("gh-pilot", ix))
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .gap(crate::ui::sp(10.))
            .mx(px(6.))
            .px(crate::ui::sp(10.))
            .py(crate::ui::sp(10.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .cursor_pointer()
            .when(focused, |d| {
                d.bg(theme::fill_raised()).child(
                    div()
                        .absolute()
                        .left(px(-6.))
                        .top(px(12.))
                        .bottom(px(12.))
                        .w(px(3.))
                        .rounded_full()
                        .bg(theme::accent()),
                )
            })
            .when(!focused, |d| d.hover(|s| s.bg(theme::fill_hover())))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(crate::ui::sp(12.))
                    .child(lead)
                    .child(body),
            )
            .child(foot)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.pilot_focus = Some(pick.id.clone());
                if let Some(r) = pick.item_ref() {
                    this.reveal(&r, cx);
                }
                window.focus(&this.focus, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// The card's pull request, picked in the tab.
    fn pilot_see(&mut self, item: &Item, cx: &mut Context<Self>) {
        self.pilot_focus = Some(item.id.clone());
        if let Some(r) = item.item_ref() {
            self.reveal(&r, cx);
        }
        cx.notify();
    }

    /// A key on the picked card: an answer, "Other answer…" or done. False
    /// when the key does nothing there, so it keeps its queue meaning.
    pub(super) fn pilot_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(id) = self.pilot_focus.clone() else {
            return false;
        };
        let Some(card) = self.pilot_cards(cx).into_iter().find(|c| c.id == id) else {
            return false;
        };
        match card_key(&card, key) {
            Some(CardKey::Answer(a)) => {
                self.pilot_answer(&id, a, cx);
            }
            Some(CardKey::Other) => self.pilot_open_other(&id, window, cx),
            Some(CardKey::Done) => self.pilot_finish(&id, cx),
            None => return false,
        }
        true
    }

    /// "Other answer…": a line under the card for the user's own words.
    pub(super) fn pilot_open_other(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pilot_other.as_deref() != Some(id) {
            self.pilot_other = Some(id.to_string());
            self.pilot_input.update(cx, |i, cx| i.set_text("", cx));
        }
        window.focus(&self.pilot_input.focus_handle(cx), cx);
        cx.notify();
    }

    /// Enter on the "Other answer…" line.
    pub(super) fn pilot_send_other(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.pilot_other.clone() else {
            return;
        };
        let text = self.pilot_input.read(cx).text().trim().to_string();
        if text.is_empty() {
            self.pilot_notes
                .insert(id, "Write your answer first.".into());
            cx.notify();
            return;
        }
        let decision = self.board_pr(cx).and_then(|b| b.open_decision().cloned());
        // Only words that went out: a refused or second send must not show.
        if self.pilot_answer(&id, Answer::Texto(text.clone()), cx)
            && decision.is_some_and(|d| d.item == id)
        {
            self.note_decision_answer(&id, text);
        }
    }

    /// Hands the answer to the agent. The card goes once the Worker took it;
    /// on an error it stays, saying why. False when nothing was sent.
    pub(super) fn pilot_answer(
        &mut self,
        id: &str,
        answer: Answer,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(cfg) = self.pilot.read(cx).config().cloned() else {
            self.pilot_notes.insert(
                id.to_string(),
                "Not sent: arakun-pilot isn't connected (\u{201c}Connect arakun-pilot\u{2026}\u{201d} in the GitHub tab's menu).".into(),
            );
            cx.notify();
            return false;
        };
        if !self.pilot_sending.insert(id.to_string()) {
            return false;
        }
        self.pilot_notes.remove(id);
        cx.notify();
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let (i, a) = (id.clone(), answer.clone());
            let sent = cx
                .background_spawn(crate::guard::task(async move {
                    pilot::send_answer(&cfg, &i, &a)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.pilot_sending.remove(&id);
                match sent {
                    Ok(()) => {
                        if this.pilot_other.as_deref() == Some(id.as_str()) {
                            this.pilot_other = None;
                        }
                        if this.pilot_focus.as_deref() == Some(id.as_str()) {
                            let cards = this.pilot_cards(cx);
                            this.pilot_focus = next_card(&cards, &id);
                        }
                        this.pilot_open.remove(&id);
                        let clear = cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(SENT_SHOWS).await;
                            let _ = this.update(cx, |this, cx| {
                                this.pilot_sent = None;
                                cx.notify();
                            });
                        });
                        this.pilot_sent = Some(("Sent: the agent is on it.".into(), clear));
                        this.pilot.update(cx, |p, cx| {
                            p.answered(&id, cx);
                            // The board moves on with the answer: read it again.
                            p.refresh(cx);
                        });
                    }
                    Err(e) => {
                        // Nothing was said: no "You told pilot…" for it.
                        this.answers.forget(&id);
                        this.pilot_notes
                            .insert(id.clone(), format!("Your answer wasn't sent: {e}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// "Looks good": the confirm strip for an approval of the commit the
    /// agent read.
    fn pilot_approve(&mut self, item: &Item, cx: &mut Context<Self>) {
        self.pilot_pin(item, Confirm::Approve, cx);
    }

    /// The confirm strip `c` for a card's review, pinned to the commit the
    /// agent read: approval and change request follow the same rule. False,
    /// with a note on the card, when the card names no full commit. A push
    /// since then cancels the strip.
    fn pilot_pin(&mut self, item: &Item, c: Confirm, cx: &mut Context<Self>) -> bool {
        let Some(r) = item.item_ref() else {
            return false;
        };
        self.pilot_notes.remove(&item.id);
        if !pilot::full_sha(&item.head) {
            self.pilot_notes.insert(
                item.id.clone(),
                "The summary doesn't name the full commit it's for; review it from the pull request."
                    .into(),
            );
            cx.notify();
            return false;
        }
        self.pilot_see(item, cx);
        self.ask_confirm(c, cx);
        let act = PilotAct {
            id: item.id.clone(),
            item: r.clone(),
            head: item.head.trim().to_string(),
        };
        self.pilot_act = Some(act.clone());
        // Asked now so the strip goes before anyone types; the review asks again.
        cx.spawn(async move |this, cx| {
            let it = r.clone();
            let now = cx
                .background_spawn(crate::guard::task(async move { pr::head_sha(&it) }))
                .await;
            let Ok(now) = now else { return };
            if pilot::same_commit(&now, &act.head) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                let text = moved_text(act.item.number, &act.head, &now);
                if this.pilot_act.as_ref() == Some(&act) {
                    this.pilot_act = None;
                    this.confirm = None;
                    this.note = Some(Note::failed(text.clone()));
                }
                this.pilot_notes.insert(act.id.clone(), text);
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// "Request changes": the confirm strip with the agent's review in it, to
    /// edit or send as it is, for the commit the agent read.
    fn pilot_request_changes(&mut self, item: &Item, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pilot_pin(item, Confirm::RequestChanges, cx) {
            return;
        }
        let text = item.borrador.trim_end().replace('\n', NEWLINE);
        self.body_input.update(cx, |i, cx| i.set_text(text, cx));
        window.focus(&self.body_input.focus_handle(cx), cx);
    }

    /// Ok, Done, or a card's write that GitHub took: the card goes.
    pub(super) fn pilot_finish(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.pilot_act.as_ref().is_some_and(|a| a.id == id) {
            self.pilot_act = None;
        }
        self.pilot_notes.remove(id);
        self.pilot_open.remove(id);
        if self.pilot_focus.as_deref() == Some(id) {
            let cards = self.pilot_cards(cx);
            self.pilot_focus = next_card(&cards, id);
        }
        let id = id.to_string();
        self.pilot.update(cx, |p, cx| p.done(&id, cx));
    }

    /// The card's action for the confirm strip `c`, when the strip came from one.
    pub(super) fn pilot_for(&self, c: Confirm) -> Option<PilotAct> {
        let act = self.pilot_act.clone()?;
        let item = self.item()?;
        (matches!(c, Confirm::Approve | Confirm::RequestChanges) && act.item == item).then_some(act)
    }

    /// Sends the confirm strip's review for a card, and marks the card done
    /// once GitHub took it.
    pub(super) fn run_pilot_confirm(
        &mut self,
        c: Confirm,
        act: PilotAct,
        body: String,
        cx: &mut Context<Self>,
    ) {
        let id = act.id.clone();
        let from = act.item.clone();
        let after = move |this: &mut Self, cx: &mut Context<Self>| {
            // Another card's or pull request's strip may be up now: only
            // this card's goes.
            if this.pilot_act.as_ref().is_some_and(|a| a.id == id)
                && this.item().as_ref() == Some(&from)
            {
                this.confirm = None;
            }
            this.pilot_finish(&id, cx);
        };
        let head = act.head;
        match c {
            Confirm::Approve => self.write(
                "approve",
                "Approved.".into(),
                move |i| pinned_job(i, Verdict::Approve, &body, &head),
                after,
                cx,
            ),
            Confirm::RequestChanges => {
                if body.trim().is_empty() {
                    self.note = Some(Note::failed("Say what should change."));
                    cx.notify();
                    return;
                }
                self.write(
                    "request-changes",
                    "Changes requested.".into(),
                    move |i| pinned_job(i, Verdict::RequestChanges, &body, &head),
                    after,
                    cx,
                )
            }
            _ => {}
        }
    }

    /// From the island's "See": the card in view, its pull request picked.
    pub fn reveal_pilot(&mut self, id: &str, cx: &mut Context<Self>) {
        let g = self.github.read(cx);
        let p = self.pilot.read(cx);
        let found = p.find(id).cloned();
        // The card itself in view, not just the top of the list: it may be
        // below the first cards.
        let at = p.shown_in(g.focus()).position(|i| i.id == id);
        let row = at.map(|card| {
            card_row(
                g.problem().or(g.search_problem()).is_some(),
                self.pilot_sent.is_some(),
                p.problem().is_some(),
                card,
            )
        });
        match row {
            Some(row) => self.queue_scroll.scroll_to_item(row),
            None => self.queue_scroll.set_offset(gpui::Point::default()),
        }
        if let Some(item) = found {
            self.pilot_see(&item, cx);
        }
    }

    /// The confirm strip's words when it came from a card.
    pub(super) fn pilot_question(&self, c: Confirm) -> Option<(String, String)> {
        let act = self.pilot_for(c)?;
        // The repository too: the review lands there.
        let n = act.item.label();
        Some(match c {
            Confirm::Approve => (format!("Approve {n} as you?"), "Approve".into()),
            _ => (format!("Request changes on {n}?"), "Request changes".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opcion(id: &str) -> Opcion {
        Opcion {
            id: id.into(),
            texto: id.to_uppercase(),
        }
    }

    fn card(id: &str, kind: Kind) -> Item {
        Item {
            id: id.into(),
            kind,
            ..Default::default()
        }
    }

    #[test]
    fn the_next_question_is_picked_after_an_answer() {
        let cards = [
            card("a", Kind::NeedsYou),
            card("b", Kind::Fixed),
            card("c", Kind::NeedsYou),
        ];
        assert_eq!(next_card(&cards, "a").as_deref(), Some("c"));
        // Round to the start before settling for a card that asks nothing.
        assert_eq!(next_card(&cards, "c").as_deref(), Some("a"));
        assert_eq!(next_card(&cards[1..2], "b"), None);
        assert_eq!(next_card(&cards[..2], "a").as_deref(), Some("b"));
    }

    #[test]
    fn keys_answer_the_picked_card() {
        let mut q = card("q", Kind::NeedsYou);
        q.opciones = vec![opcion("migrar"), opcion("quedan")];
        assert_eq!(
            card_key(&q, "2"),
            Some(CardKey::Answer(Answer::Opcion("quedan".into())))
        );
        assert_eq!(card_key(&q, "3"), None);
        assert_eq!(card_key(&q, "e"), Some(CardKey::Other));
        assert_eq!(card_key(&q, "y"), None);
        assert_eq!(card_key(&card("f", Kind::Fixed), "y"), Some(CardKey::Done));
        // A brief's approval asks twice in the sheet: no key skips that.
        assert_eq!(card_key(&card("b", Kind::Brief), "y"), None);
    }

    #[test]
    fn the_first_real_answer_is_the_primary_one() {
        let kinds = opcion_kinds(&[opcion("migrar"), opcion("quedan"), opcion("skip")]);
        assert_eq!(
            kinds,
            [PillKind::Primary, PillKind::Normal, PillKind::Quiet]
        );
        // "Lo veo yo" first doesn't take the primary look.
        let kinds = opcion_kinds(&[opcion("skip"), opcion("migrar")]);
        assert_eq!(kinds, [PillKind::Quiet, PillKind::Primary]);
    }

    #[test]
    fn a_notice_scrolls_to_its_own_card() {
        // Heading, then the cards.
        assert_eq!(card_row(false, false, false, 0), 1);
        assert_eq!(card_row(false, false, false, 4), 5);
        // GitHub's problem line, the sent note and the Worker's problem
        // come first when they show.
        assert_eq!(card_row(true, true, true, 2), 6);
    }

    #[test]
    fn a_moved_head_says_both_commits() {
        assert_eq!(
            moved_text(95, "40a1b2c3d4e5f6", "99ffeedd00112233"),
            "#95 has new commits since the summary (40a1b2c → 99ffeed). I didn't approve it: review it again."
        );
    }
}
